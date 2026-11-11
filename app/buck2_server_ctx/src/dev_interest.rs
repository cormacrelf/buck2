/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! Which file changes a `buck2 dev` session may care about.
//!
//! A session subscribes to the file changes that the daemon's file watcher sees. That
//! subscription takes no lock on the daemon's state, so it cannot ask what a path is to the
//! session's target. Instead, each `buck2 dev` request that finds what the server and the
//! outputs claim leaves a `FileInterest` here, under the name of the session, and the
//! subscription drops the changes that the interest says cannot matter. The session then asks
//! the daemon only about the rest.
//!
//! An interest is a summary, and may be out of date: it lets through more than matters, and what
//! it holds back cannot change what the target is built from without another change that it
//! lets through. See `FileInterest::may_matter`.
//!
//! A session also holds the local resources of its server here (see `SessionHeld`), so that they
//! outlive each request, and are released when the session's last subscription ends.

use std::any::Any;
use std::collections::HashMap;
use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::sync::Arc;
use std::sync::Mutex;

use allocative::Allocative;
use buck2_core::fs::project_rel_path::ProjectRelativePath;
use futures::future::BoxFuture;

/// A hash of `text` that is the same in another daemon.
pub fn stable_hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// The paths that may matter to a `buck2 dev` session, by the hashes of project-relative paths.
#[derive(Allocative, Debug, Default, PartialEq, Eq)]
pub struct FileInterest {
    /// The directories of the packages of the target's graph. Sorted.
    package_dirs: Vec<u64>,
    /// The directories above them. Sorted.
    above: Vec<u64>,
    /// Files that matter wherever they are: the build files and `.bzl` files of the graph, and
    /// the source files that the server or an output claims other than as a source file of one
    /// of its targets. Sorted.
    files: Vec<u64>,
}

impl FileInterest {
    pub fn new<'a>(
        package_dirs: impl IntoIterator<Item = &'a ProjectRelativePath>,
        files: impl IntoIterator<Item = u64>,
    ) -> FileInterest {
        let mut dirs = Vec::new();
        let mut above = Vec::new();
        for dir in package_dirs {
            dirs.push(stable_hash(dir.as_str()));
            let mut parent = dir.parent();
            while let Some(dir) = parent {
                above.push(stable_hash(dir.as_str()));
                parent = dir.parent();
            }
        }
        let mut files: Vec<u64> = files.into_iter().collect();
        for hashes in [&mut dirs, &mut above, &mut files] {
            hashes.sort_unstable();
            hashes.dedup();
        }
        FileInterest {
            package_dirs: dirs,
            above,
            files,
        }
    }

    /// What may matter to any of `interests`: the session's target, and the dynamic outputs
    /// that its server defined, each with a graph of its own.
    pub fn union<'a>(interests: impl IntoIterator<Item = &'a FileInterest>) -> FileInterest {
        let mut union = FileInterest::default();
        for interest in interests {
            union.package_dirs.extend(&interest.package_dirs);
            union.above.extend(&interest.above);
            union.files.extend(&interest.files);
        }
        for hashes in [&mut union.package_dirs, &mut union.above, &mut union.files] {
            hashes.sort_unstable();
            hashes.dedup();
        }
        union
    }

    /// Whether `dir` is the directory of a package of the graph.
    pub fn is_package_dir(&self, dir: &ProjectRelativePath) -> bool {
        self.package_dirs
            .binary_search(&stable_hash(dir.as_str()))
            .is_ok()
    }

    /// Whether `path` is in a package of the graph, or is one of the other files that matter.
    /// `false` means that neither the server nor an output claims it, and that it is no build
    /// file or `.bzl` file of the graph.
    pub fn may_claim(&self, path: &ProjectRelativePath) -> bool {
        if self
            .files
            .binary_search(&stable_hash(path.as_str()))
            .is_ok()
        {
            return true;
        }
        let mut dir = path.parent();
        while let Some(parent) = dir {
            if self.is_package_dir(parent) {
                return true;
            }
            dir = parent.parent();
        }
        false
    }

    /// Whether a change to `path` may matter to the session: `may_claim`, or the path is one
    /// that can change what the graph is without being in it.
    ///
    /// - A directory of a package of the graph, or one above it: removing or renaming it takes
    ///   the package away.
    /// - A `PACKAGE` file, which applies to the packages below it.
    /// - A `.buckconfig` file, or a file in a `.buckconfig.d` directory.
    ///
    /// A file that a `.buckconfig` includes under another name is held back. The daemon reads
    /// the configuration again at its next command, so the session sees that change with the
    /// next change that is let through.
    pub fn may_matter(&self, path: &ProjectRelativePath) -> bool {
        if self.may_claim(path) {
            return true;
        }
        let hash = stable_hash(path.as_str());
        if self.package_dirs.binary_search(&hash).is_ok() || self.above.binary_search(&hash).is_ok()
        {
            return true;
        }
        let name = path.file_name().map_or("", |name| name.as_str());
        name == "PACKAGE"
            || name == "PACKAGE.bzl"
            || name.starts_with(".buckconfig")
            || path
                .as_str()
                .split('/')
                .any(|component| component == ".buckconfig.d")
    }
}

/// What a `buck2 dev` session holds in the daemon besides its interest: the local resources of
/// its server. It lives as long as the session's subscriptions, and is released when the last of
/// them ends, or when the daemon shuts down.
pub trait SessionHeld: Send + Sync + 'static {
    /// Release what is held. It may be called again, and then releases what was taken since.
    fn release(self: Arc<Self>) -> BoxFuture<'static, ()>;
}

/// A session's interest, and how many subscriptions of the session there are: a subscription
/// that replaces a lost one may start before the lost one is gone.
#[derive(Default)]
struct Session {
    subscriptions: usize,
    /// `None` until a request of the session leaves it.
    interest: Option<Arc<FileInterest>>,
    /// What the session holds, once a request has taken something: the same value as each of
    /// the two types.
    held: Option<(Arc<dyn Any + Send + Sync>, Arc<dyn SessionHeld>)>,
}

/// The sessions whose subscriptions asked for their interest.
static SESSIONS: Mutex<Option<HashMap<String, Session>>> = Mutex::new(None);

fn with_sessions<R>(f: impl FnOnce(&mut HashMap<String, Session>) -> R) -> R {
    let mut sessions = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    f(sessions.get_or_insert_with(HashMap::new))
}

/// A subscription that drops the changes that cannot matter to a session. Ending the last one
/// of a session forgets the session's interest, and releases what the session holds.
pub struct InterestSubscription {
    session: String,
    ended: bool,
}

impl InterestSubscription {
    pub fn new(session: String) -> InterestSubscription {
        with_sessions(|sessions| {
            sessions.entry(session.clone()).or_default().subscriptions += 1;
        });
        InterestSubscription {
            session,
            ended: false,
        }
    }

    /// End the subscription. If it is the last of its session, release what the session holds,
    /// and wait for that.
    pub async fn end(mut self) {
        if let Some(held) = self.leave() {
            held.release().await;
        }
    }

    /// Count this subscription out of its session. Returns what the session holds if this was
    /// its last subscription.
    fn leave(&mut self) -> Option<Arc<dyn SessionHeld>> {
        if std::mem::replace(&mut self.ended, true) {
            return None;
        }
        with_sessions(|sessions| {
            let session = sessions.get_mut(&self.session)?;
            session.subscriptions -= 1;
            if session.subscriptions != 0 {
                return None;
            }
            let session = sessions.remove(&self.session)?;
            session.held.map(|(_, held)| held)
        })
    }

    /// The session's interest, or `None` until its first request has left one: everything may
    /// matter until then.
    pub fn interest(&self) -> Option<Arc<FileInterest>> {
        with_sessions(|sessions| {
            sessions
                .get(&self.session)
                .and_then(|session| session.interest.clone())
        })
    }
}

impl Drop for InterestSubscription {
    /// Reached without `end` only if the subscription stops unexpectedly. The release then runs
    /// on its own.
    fn drop(&mut self) {
        if let Some(held) = self.leave() {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(held.release());
            }
        }
    }
}

/// What `session` holds, made by `make` if it holds nothing yet. `None` if the session has no
/// subscription, and so nothing that would release it.
pub fn session_held<T: SessionHeld>(session: &str, make: impl FnOnce() -> T) -> Option<Arc<T>> {
    with_sessions(|sessions| {
        let session = sessions.get_mut(session)?;
        let (any, _) = session.held.get_or_insert_with(|| {
            let held = Arc::new(make());
            (held.clone(), held)
        });
        any.clone().downcast::<T>().ok()
    })
}

/// What `session` holds, if it holds something of type `T`.
pub fn session_held_if_any<T: SessionHeld>(session: &str) -> Option<Arc<T>> {
    with_sessions(|sessions| {
        let (any, _) = sessions.get(session)?.held.as_ref()?;
        any.clone().downcast::<T>().ok()
    })
}

/// Release what every session holds: the daemon is shutting down. The sessions stay, so that a
/// request that runs meanwhile finds them.
pub async fn release_all_sessions() {
    let held: Vec<Arc<dyn SessionHeld>> = with_sessions(|sessions| {
        sessions
            .values()
            .filter_map(|session| session.held.as_ref().map(|(_, held)| held.clone()))
            .collect()
    });
    futures::future::join_all(held.into_iter().map(|held| held.release())).await;
}

/// Leave the interest of `session` for its subscriptions, if it has any.
pub fn set_interest(session: &str, interest: Arc<FileInterest>) {
    with_sessions(|sessions| {
        if let Some(session) = sessions.get_mut(session) {
            session.interest = Some(interest);
        }
    });
}

/// Add `interest` to the interest of `session`, if it has one: a build found what new dynamic
/// outputs are built from, before the next request that leaves the whole interest.
pub fn add_interest(session: &str, interest: &FileInterest) {
    with_sessions(|sessions| {
        if let Some(session) = sessions.get_mut(session) {
            if let Some(old) = &session.interest {
                session.interest = Some(Arc::new(FileInterest::union([&**old, interest])));
            }
        }
    });
}

/// Forget the interest of `session`, so that its subscriptions send every change again: its
/// target could not be analysed, and what fixes that may be outside what it was built from.
pub fn clear_interest(session: &str) {
    with_sessions(|sessions| {
        if let Some(session) = sessions.get_mut(session) {
            session.interest = None;
        }
    });
}

#[cfg(test)]
mod tests {
    use buck2_core::fs::project_rel_path::ProjectRelativePath;

    use super::FileInterest;
    use super::stable_hash;

    fn path(path: &str) -> &ProjectRelativePath {
        ProjectRelativePath::new(path).unwrap()
    }

    #[test]
    fn test_what_may_matter() {
        let interest = FileInterest::new([path("app"), path("libs/a")], [stable_hash("defs.bzl")]);
        // In a package of the graph, however deep.
        assert!(interest.may_claim(path("app/main.txt")));
        assert!(interest.may_claim(path("app/pages/a.txt")));
        // One of the files that matter wherever they are.
        assert!(interest.may_claim(path("defs.bzl")));
        // Outside the packages: nothing claims it, and it cannot change the graph.
        assert!(!interest.may_matter(path("other/main.txt")));
        assert!(!interest.may_matter(path("libs/b/lib.txt")));
        assert!(!interest.may_matter(path("other.bzl")));
        // A directory of a package, or one above it, takes the package away with it.
        assert!(!interest.may_claim(path("libs")));
        assert!(interest.may_matter(path("libs")));
        assert!(interest.may_matter(path("libs/a")));
        // Files that apply to the packages below or around them.
        assert!(interest.may_matter(path("PACKAGE")));
        assert!(interest.may_matter(path("other/.buckconfig")));
        assert!(interest.may_matter(path(".buckconfig.d/local")));
    }

    #[test]
    fn test_union() {
        let a = FileInterest::new([path("app")], [stable_hash("defs.bzl")]);
        let b = FileInterest::new([path("libs/a")], [stable_hash("dev.bxl")]);
        let union = FileInterest::union([&a, &b]);
        assert!(union.may_claim(path("app/main.txt")));
        assert!(union.may_claim(path("libs/a/lib.txt")));
        assert!(union.may_claim(path("defs.bzl")));
        assert!(union.may_claim(path("dev.bxl")));
        assert!(union.may_matter(path("libs")));
        assert!(!union.may_matter(path("other/main.txt")));
    }
}
