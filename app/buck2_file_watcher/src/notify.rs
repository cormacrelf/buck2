/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

use std::mem;
use std::sync::Arc;
use std::sync::Mutex;

use allocative::Allocative;
use async_trait::async_trait;
use buck2_common::file_ops::baseline::FileSystemBaseline;
use buck2_common::file_ops::baseline::SetFileSystemBaseline;
use buck2_common::file_ops::io::FileChangeTracker;
use buck2_common::invocation_paths::InvocationPaths;
use buck2_core::fs::i_project_rel_path::IProjectRelativePath;
use buck2_core::fs::project::ProjectRoot;
use buck2_data::FileWatcherEventType;
use buck2_data::FileWatcherKind;
use buck2_error::conversion::from_any_with_tag;
use buck2_events::dispatch::span_async;
use buck2_fs::paths::abs_norm_path::AbsNormPath;
use buck2_hash::BuckMutInsertionSet;
use dice::DiceTransactionUpdater;
use dupe::Dupe;
use notify::EventKind;
use notify::RecommendedWatcher;
use notify::Watcher;
use notify::event::CreateKind;
use notify::event::MetadataKind;
use notify::event::ModifyKind;
use notify::event::RemoveKind;
use tracing::debug;
use tracing::info;

use crate::file_watcher::ChangeKind;
use crate::file_watcher::ChangePublisher;
use crate::file_watcher::FileChangeSubscription;
use crate::file_watcher::FileWatcher;
use crate::file_watcher::PushedChanges;
use crate::file_watcher::SyncOutcome;
use crate::ignores::FileWatcherIgnores;
use crate::mergebase::Mergebase;
use crate::stats::FileWatcherStats;

fn ignore_event_kind(event_kind: EventKind) -> bool {
    match event_kind {
        EventKind::Access(_) => true,
        // Only the access time can change without anything buck2 reads changing. `chmod` arrives
        // as `Metadata(Any)` from every notify backend, never as `Permissions`.
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime)) => true,
        _ => false,
    }
}

/// What `sync` records for an event, as pushed changes. Kinds that `sync` ignores push nothing.
fn change_kinds(event_kind: EventKind) -> &'static [ChangeKind] {
    const FILE: &[ChangeKind] = &[ChangeKind::FileAddedOrRemoved];
    const DIR: &[ChangeKind] = &[ChangeKind::DirAddedOrRemoved];
    const BOTH: &[ChangeKind] = &[
        ChangeKind::FileAddedOrRemoved,
        ChangeKind::DirAddedOrRemoved,
    ];
    match event_kind {
        EventKind::Create(CreateKind::File) | EventKind::Remove(RemoveKind::File) => FILE,
        EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder) => DIR,
        EventKind::Create(CreateKind::Any | CreateKind::Other)
        | EventKind::Remove(RemoveKind::Any | RemoveKind::Other)
        | EventKind::Modify(ModifyKind::Name(_) | ModifyKind::Any | ModifyKind::Other) => BOTH,
        EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_)) => &[ChangeKind::Contents],
        EventKind::Any | EventKind::Access(_) | EventKind::Other => &[],
    }
}

/// Buffer containing the events that have happened since we last got a message.
/// Used to dedupe events, since notify sends a notification on every change.
#[derive(Allocative)]
struct NotifyFileData {
    ignored: u64,
    #[allocative(skip)]
    events: BuckMutInsertionSet<(IProjectRelativePath, EventKind)>,
    /// Whether file system changes were missed
    missed_events: bool,
}

impl NotifyFileData {
    fn new() -> Self {
        Self {
            ignored: 0,
            events: BuckMutInsertionSet::new(),
            missed_events: false,
        }
    }

    /// Record `event` for the next sync, and, for subscribers, in `pushed`.
    fn process(
        &mut self,
        event: notify::Result<notify::Event>,
        root: &ProjectRoot,
        ignores: &FileWatcherIgnores,
        mut pushed: Option<&mut PushedChanges>,
    ) -> buck2_error::Result<()> {
        let event =
            event.map_err(|e| from_any_with_tag(e, buck2_error::ErrorTag::NotifyWatcher))?;

        // Checked before the path loop: the kernel-overflow rescan signal can arrive with no
        // paths attached (platform dependent), and a missed rescan means the daemon keeps
        // answering from a stale graph.
        if event.need_rescan() {
            self.missed_events = true;
            if let Some(pushed) = &mut pushed {
                pushed.missed = true;
            }
            debug!("FileWatcher: File change events were missed");
        }

        for path in &event.paths {
            // Testing shows that we get absolute paths back from the `notify` library.
            // It's not documented though.
            let path = root.relativize(AbsNormPath::new(&path)?)?;
            let path = IProjectRelativePath::intern_directly(path.as_ref());

            // We ignore the buck-out prefix, as those are uninteresting events caused by us.
            // We also ignore other buck-out directories, as if you have two isolation dirs running at once, they are not interesting.
            // We do this in the notify-watcher, rather than a generic layer, as watchman users should configure
            // to ignore buck-out, to reduce the number of events, rather than hiding them later.
            if path.starts_with(InvocationPaths::buck_out_dir_prefix()) {
                // We don't want to event add them as ignored events, since they are super common
                // and very boring
                continue;
            }

            let ignore = ignores.is_ignored(path);

            info!(
                "FileWatcher: {:?} {:?} (ignore = {})",
                path, &event.kind, ignore
            );

            if ignore || ignore_event_kind(event.kind) {
                self.ignored += 1;
            } else {
                self.events.insert((path, event.kind));
                if let Some(pushed) = &mut pushed {
                    for kind in change_kinds(event.kind) {
                        pushed
                            .changes
                            .push((path.as_project_relative_path().to_buf(), *kind));
                    }
                }
            }
        }
        Ok(())
    }

    fn sync(self) -> (buck2_data::FileWatcherStats, Option<FileChangeTracker>) {
        // The changes that go into the DICE transaction
        let mut changed = FileChangeTracker::new();
        // If we missed events, the sync reports a fresh instance and the daemon starts over.
        // Surface that to telemetry/UI the same way the watchman path does.
        let base = if self.missed_events {
            buck2_data::FileWatcherStats {
                fresh_instance: true,
                fresh_instance_data: Some(buck2_data::FreshInstance {
                    new_mergebase: false,
                    cleared_dice: true,
                    cleared_dep_files: false,
                }),
                incomplete_events_reason: Some(
                    "notify dropped events (kernel queue overflow)".to_owned(),
                ),
                ..Default::default()
            }
        } else {
            Default::default()
        };
        let mut stats = FileWatcherStats::new(base, self.events.len());
        stats.add_ignored(self.ignored);

        for (path, event_kind) in self.events {
            match event_kind {
                EventKind::Create(create_kind) => match create_kind {
                    CreateKind::File => {
                        stats.add(path, FileWatcherEventType::Create, FileWatcherKind::File);
                        changed.file_added_or_removed(path);
                    }
                    CreateKind::Folder => {
                        stats.add(
                            path,
                            FileWatcherEventType::Create,
                            FileWatcherKind::Directory,
                        );
                        changed.dir_added_or_removed(path);
                    }
                    CreateKind::Any | CreateKind::Other => {
                        stats.add(path, FileWatcherEventType::Create, FileWatcherKind::File);
                        changed.file_added_or_removed(path);

                        stats.add(
                            path,
                            FileWatcherEventType::Create,
                            FileWatcherKind::Directory,
                        );
                        changed.dir_added_or_removed(path);
                    }
                },
                EventKind::Modify(modify_kind) => match modify_kind {
                    ModifyKind::Data(_) | ModifyKind::Metadata(_) => {
                        stats.add(path, FileWatcherEventType::Modify, FileWatcherKind::File);
                        changed.file_contents_changed(path);
                    }
                    ModifyKind::Name(_) | ModifyKind::Any | ModifyKind::Other => {
                        stats.add(path, FileWatcherEventType::Create, FileWatcherKind::File);
                        stats.add(path, FileWatcherEventType::Delete, FileWatcherKind::File);
                        changed.file_added_or_removed(path);
                        stats.add(
                            path,
                            FileWatcherEventType::Create,
                            FileWatcherKind::Directory,
                        );
                        stats.add(
                            path,
                            FileWatcherEventType::Delete,
                            FileWatcherKind::Directory,
                        );
                        changed.dir_added_or_removed(path);
                    }
                },
                EventKind::Remove(remove_kind) => match remove_kind {
                    RemoveKind::File => {
                        stats.add(path, FileWatcherEventType::Delete, FileWatcherKind::File);
                        changed.file_added_or_removed(path);
                    }
                    RemoveKind::Folder => {
                        stats.add(
                            path,
                            FileWatcherEventType::Delete,
                            FileWatcherKind::Directory,
                        );
                        changed.dir_added_or_removed(path);
                    }
                    RemoveKind::Any | RemoveKind::Other => {
                        stats.add(path, FileWatcherEventType::Delete, FileWatcherKind::File);
                        changed.file_added_or_removed(path);
                        stats.add(
                            path,
                            FileWatcherEventType::Delete,
                            FileWatcherKind::Directory,
                        );
                        changed.dir_added_or_removed(path);
                    }
                },
                _ => {}
            }
        }

        let stats = stats.finish();
        let changed = if self.missed_events {
            None
        } else {
            Some(changed)
        };

        (stats, changed)
    }
}

#[derive(Allocative)]
pub struct NotifyFileWatcher {
    #[allocative(skip)]
    #[expect(unused)]
    // FIXME(JakobDegen): Clarify if this just needs to be kept alive or can be removed?
    watcher: RecommendedWatcher,
    data: Arc<Mutex<buck2_error::Result<NotifyFileData>>>,
    /// What the changes recorded on the updater are relative to.
    baseline: Mutex<FileSystemBaseline>,
    #[allocative(skip)]
    publisher: Arc<ChangePublisher>,
}

impl NotifyFileWatcher {
    pub fn new(root: &ProjectRoot, ignores: FileWatcherIgnores) -> buck2_error::Result<Self> {
        let data = Arc::new(Mutex::new(Ok(NotifyFileData::new())));
        let data2 = data.dupe();
        let root2 = root.dupe();
        let publisher = Arc::new(ChangePublisher::new());
        let publisher2 = publisher.dupe();
        let mut watcher = notify::recommended_watcher(move |event| {
            let mut guard = data2.lock().unwrap();
            if let Ok(state) = &mut *guard {
                // Pushed while the lock is held, after the event is recorded, so that a sync
                // that starts after a subscriber sees a change records it too.
                let mut pushed = publisher2.has_subscribers().then(PushedChanges::default);
                match state.process(event, &root2, &ignores, pushed.as_mut()) {
                    Ok(()) => {
                        if let Some(pushed) = pushed {
                            publisher2.publish(pushed);
                        }
                    }
                    Err(e) => {
                        *guard = Err(e);
                        // Nothing is recorded from now on, until the sync reports the error.
                        publisher2.publish(PushedChanges {
                            changes: Vec::new(),
                            missed: true,
                        });
                    }
                }
            }
        })
        .map_err(|e| from_any_with_tag(e, buck2_error::ErrorTag::NotifyWatcher))?;
        watcher
            .watch(root.root().as_path(), notify::RecursiveMode::Recursive)
            .map_err(|e| from_any_with_tag(e, buck2_error::ErrorTag::NotifyWatcher))?;
        Ok(Self {
            watcher,
            data,
            baseline: Mutex::new(FileSystemBaseline::unique()),
            publisher,
        })
    }

    fn sync2(
        &self,
        mut dice: DiceTransactionUpdater,
    ) -> buck2_error::Result<(buck2_data::FileWatcherStats, DiceTransactionUpdater)> {
        let old = {
            let mut guard = self.data.lock().unwrap();
            mem::replace(&mut *guard, Ok(NotifyFileData::new()))
        };
        let (stats, changes) = old?.sync();
        let mut baseline = self.baseline.lock().unwrap();
        if let Some(changes) = changes {
            changes.write_to_dice(&mut dice)?;
        } else {
            // Events were missed; the fresh instance reported in `stats` has the daemon start
            // over.
            *baseline = FileSystemBaseline::unique();
        }
        dice.set_file_system_baseline(baseline.dupe())?;
        Ok((stats, dice))
    }
}

#[async_trait]
impl FileWatcher for NotifyFileWatcher {
    async fn sync(
        &self,
        dice: DiceTransactionUpdater,
    ) -> buck2_error::Result<(DiceTransactionUpdater, Mergebase, SyncOutcome)> {
        span_async(
            buck2_data::FileWatcherStart {
                provider: buck2_data::FileWatcherProvider::RustNotify as i32,
            },
            async {
                let (stats, res) = match self.sync2(dice) {
                    Ok((stats, dice)) => {
                        let mergebase = Mergebase(Arc::new(stats.branched_from_revision.clone()));
                        let outcome = SyncOutcome::reported_by(&stats);
                        ((Some(stats)), Ok((dice, mergebase, outcome)))
                    }
                    Err(e) => (None, Err(e)),
                };
                (res, buck2_data::FileWatcherEnd { stats })
            },
        )
        .await
    }

    async fn subscribe(&self) -> buck2_error::Result<FileChangeSubscription> {
        Ok(self.publisher.subscribe())
    }
}

#[cfg(test)]
mod tests {
    use buck2_common::ignores::ignore_set::IgnoreSet;
    use buck2_core::cells::CellResolver;
    use buck2_core::cells::cell_root_path::CellRootPathBuf;
    use buck2_core::cells::name::CellName;
    use buck2_core::fs::project_rel_path::ProjectRelativePath;
    use buck2_fs::fs_util::uncategorized as fs_util;
    use buck2_fs::paths::abs_norm_path::AbsNormPathBuf;
    use notify::event::CreateKind;
    use notify::event::Flag;

    use super::*;

    fn fixture() -> (ProjectRoot, FileWatcherIgnores, tempfile::TempDir) {
        let cells = CellResolver::testing_with_name_and_path(
            CellName::testing_new("root"),
            CellRootPathBuf::testing_new(""),
        );
        let tempdir = tempfile::tempdir().unwrap();
        let root_path =
            fs_util::canonicalize(AbsNormPathBuf::new(tempdir.path().to_owned()).unwrap()).unwrap();
        let root = ProjectRoot::new(root_path).unwrap();
        let ignores = Arc::new(
            IgnoreSet::testing_new_single_cell(cells, CellName::testing_new("root"), "").unwrap(),
        );
        (root, FileWatcherIgnores::new(ignores), tempdir)
    }

    /// The kernel-overflow signal can arrive as a rescan-flagged event with no paths attached
    /// (platform dependent). The flag must still be recorded, otherwise lost notifications go
    /// undetected and the daemon keeps answering from a stale graph, which is the cache
    /// inconsistency the missed-events handling exists to close.
    #[test]
    fn rescan_event_with_no_paths_sets_missed_events() {
        let (root, ignores, _t) = fixture();
        let mut state = NotifyFileData::new();
        let event = notify::Event::new(EventKind::Any).set_flag(Flag::Rescan);
        state.process(Ok(event), &root, &ignores, None).unwrap();
        assert!(
            state.missed_events,
            "a pathless rescan event must set missed_events"
        );
    }

    #[test]
    fn rescan_event_with_paths_sets_missed_events() {
        let (root, ignores, _t) = fixture();
        let mut state = NotifyFileData::new();
        let path = root.resolve(ProjectRelativePath::new("f").unwrap());
        let event = notify::Event::new(EventKind::Create(CreateKind::File))
            .add_path(path.into_abs_path_buf().into_path_buf())
            .set_flag(Flag::Rescan);
        state.process(Ok(event), &root, &ignores, None).unwrap();
        assert!(state.missed_events);
    }

    /// Missed events: the sync result carries no tracker (the daemon starts over) and the stats
    /// surface the fresh instance the same way watchman's path does.
    #[test]
    fn sync_with_missed_events_reports_fresh_instance_and_drops_changes() {
        let mut state = NotifyFileData::new();
        state.missed_events = true;
        let (stats, changes) = state.sync();
        assert!(changes.is_none(), "missed events must drop the tracker");
        assert!(stats.fresh_instance);
        let fresh = stats
            .fresh_instance_data
            .expect("fresh instance data populated");
        assert!(fresh.cleared_dice);
        assert!(stats.incomplete_events_reason.is_some());
    }

    #[test]
    fn sync_without_missed_events_returns_changes() {
        let (stats, changes) = NotifyFileData::new().sync();
        assert!(
            changes.is_some(),
            "no missed events: incremental tracker must survive"
        );
        assert!(!stats.fresh_instance);
        assert!(stats.incomplete_events_reason.is_none());
    }

    /// A subscriber gets the same changes the next sync records, and nothing that it ignores.
    #[test]
    fn process_pushes_what_it_records() {
        let (root, ignores, _t) = fixture();
        let mut state = NotifyFileData::new();
        let mut pushed = PushedChanges::default();
        let file = root.resolve(ProjectRelativePath::new("dir/f").unwrap());
        let buck_out = root.resolve(ProjectRelativePath::new("buck-out/v2/x").unwrap());
        for (kind, path) in [
            (
                EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Any)),
                &file,
            ),
            (EventKind::Create(CreateKind::Folder), &file),
            (EventKind::Access(notify::event::AccessKind::Read), &file),
            (EventKind::Create(CreateKind::File), &buck_out),
        ] {
            let event =
                notify::Event::new(kind).add_path(path.clone().into_abs_path_buf().into_path_buf());
            state
                .process(Ok(event), &root, &ignores, Some(&mut pushed))
                .unwrap();
        }
        let dir_f = ProjectRelativePath::new("dir/f").unwrap().to_buf();
        assert_eq!(
            pushed.changes,
            vec![
                (dir_f.clone(), ChangeKind::Contents),
                (dir_f, ChangeKind::DirAddedOrRemoved),
            ]
        );
        assert!(!pushed.missed);
        assert_eq!(state.events.len(), 2);
    }

    #[test]
    fn change_kinds_follow_sync() {
        use notify::event::RenameMode;
        assert_eq!(
            change_kinds(EventKind::Create(CreateKind::File)),
            &[ChangeKind::FileAddedOrRemoved]
        );
        assert_eq!(
            change_kinds(EventKind::Remove(RemoveKind::Folder)),
            &[ChangeKind::DirAddedOrRemoved]
        );
        assert_eq!(
            change_kinds(EventKind::Modify(ModifyKind::Name(RenameMode::Any))),
            &[
                ChangeKind::FileAddedOrRemoved,
                ChangeKind::DirAddedOrRemoved
            ]
        );
        assert_eq!(
            change_kinds(EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any))),
            &[ChangeKind::Contents]
        );
        assert_eq!(change_kinds(EventKind::Any), &[] as &[ChangeKind]);
    }

    /// Against the real watcher: a subscriber gets a change once it is recorded for the next
    /// sync.
    #[tokio::test]
    async fn subscriber_gets_changes_after_they_are_recorded() {
        let (root, ignores, _t) = fixture();
        let watcher = NotifyFileWatcher::new(&root, ignores).unwrap();
        let mut subscription = watcher.subscribe().await.unwrap();
        let path = root.resolve(ProjectRelativePath::new("new.txt").unwrap());
        std::fs::write(path.as_path(), "x").unwrap();
        let pushed = tokio::time::timeout(std::time::Duration::from_secs(10), subscription.next())
            .await
            .expect("a change is pushed")
            .expect("the watcher is alive");
        let new_txt = ProjectRelativePath::new("new.txt").unwrap().to_buf();
        assert!(
            pushed.changes.iter().any(|(path, _)| *path == new_txt),
            "{pushed:?}"
        );
        let data = watcher.data.lock().unwrap();
        let recorded = data.as_ref().unwrap();
        assert!(
            recorded
                .events
                .iter()
                .any(|(path, _)| path.as_project_relative_path() == &*new_txt),
            "pushed before it was recorded"
        );
    }

    /// `chmod` arrives from every notify backend as `Modify(Metadata(Any))` (inotify
    /// `IN_ATTRIB`, FSEvents `INODE_META_MOD`, kqueue `NOTE_ATTRIB`); no backend emits
    /// `Permissions`. Only access events and access-time changes can be dropped.
    #[test]
    fn test_metadata_any_is_not_ignored() {
        assert!(!ignore_event_kind(EventKind::Modify(ModifyKind::Metadata(
            MetadataKind::Any
        ))));
        assert!(!ignore_event_kind(EventKind::Modify(ModifyKind::Metadata(
            MetadataKind::Permissions
        ))));
        assert!(ignore_event_kind(EventKind::Modify(ModifyKind::Metadata(
            MetadataKind::AccessTime
        ))));
        assert!(ignore_event_kind(EventKind::Access(
            notify::event::AccessKind::Read
        )));
    }
}
