/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! The `buck2 dev` session: one daemon command for the whole of a `buck2 dev` session.
//!
//! The client sends requests over the command's request stream, and the session answers with
//! partial results. Requests run in _epochs_. An epoch is one DICE transaction, entered through
//! the concurrency handler like any command's, and all the requests of an epoch run in it side
//! by side: the client starts a new epoch only once the file watcher has reported a change, so
//! they all see the same files. A request of a later epoch waits until those of the current one
//! are done, and then gets a new transaction.
//!
//! The transaction outlives the requests for a moment (`IDLE_RELEASE`), so that requests that
//! come one after the other, such as the imports of a page that a browser finds level by level,
//! share it rather than each paying for a new one. Another command with other files or another
//! configuration preempts it as it would any `buck2 dev` command, and the session runs what was
//! running again in a new transaction once the other command lets it in.
//!
//! Each output of a build is built on its own, and its result goes to the client as soon as it
//! is known. What a dynamic output is built from is found next to its build, not before it, so
//! that it does not hold up the first build. While a build runs, the session watches the files
//! that it may be built from, and stops it when one of them changes.

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use buck2_cli_proto::NewGenericPartialResultMessage;
use buck2_cli_proto::new_generic::DevAction;
use buck2_cli_proto::new_generic::DevDynamicCall;
use buck2_cli_proto::new_generic::DevOutputError;
use buck2_cli_proto::new_generic::DevResponse;
use buck2_cli_proto::new_generic::DevSessionEvent;
use buck2_cli_proto::new_generic::DevSessionRequest;
use buck2_cli_proto::new_generic::DevSessionStart;
use buck2_cli_proto::new_generic::DevWatchRequest;
use buck2_cli_proto::new_generic::DevWatchState;
use buck2_cli_proto::new_generic::NewGenericPartialResult;
use buck2_common::io::IoProvider;
use buck2_core::fs::artifact_path_resolver::ArtifactFs;
use buck2_core::fs::project_rel_path::ProjectRelativePathBuf;
use buck2_error::BuckErrorContext;
use buck2_error::internal_error;
use buck2_events::dispatch::span_async;
use buck2_server_ctx::commands::command_end;
use buck2_server_ctx::ctx::FileChangeBatch;
use buck2_server_ctx::ctx::ServerCommandContextTrait;
use buck2_server_ctx::ctx::ServerCommandDiceContext;
use buck2_server_ctx::dev_interest::add_interest;
use buck2_server_ctx::dev_interest::clear_interest;
use buck2_server_ctx::dev_interest::session_held_if_any;
use buck2_server_ctx::partial_result_dispatcher::PartialResultDispatcher;
use buck2_server_ctx::streaming_request_handler::StreamingRequestHandler;
use dice::DiceTransaction;
use dupe::Dupe;
use futures::FutureExt;
use futures::StreamExt;
use futures::future::AbortHandle;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::stream::FuturesUnordered;
use tokio::sync::OnceCell;
use tokio::sync::mpsc;

use super::DevClaims;
use super::DevError;
use super::Ensured;
use super::Target;
use super::Wanted;
use super::build_output;
use super::changed_since;
use super::command_argv;
use super::dev_claims;
use super::dynamic_claims;
use super::output_error;
use super::resolve_dynamic;
use super::server_action;
use super::slot_bit;
use super::watch_action;
use crate::dev_resources::SessionResources;

/// How long the session keeps a transaction that nothing runs in, for requests that come soon
/// after. Another command that needs other files preempts it at once; one that needs the daemon
/// to itself, such as `buck2 clean`, waits for this long at most.
const IDLE_RELEASE: Duration = Duration::from_secs(1);

pub(crate) async fn dev_session_command(
    ctx: &dyn ServerCommandContextTrait,
    partial_result_dispatcher: PartialResultDispatcher<NewGenericPartialResultMessage>,
    requests: StreamingRequestHandler<buck2_cli_proto::DevSessionMessage>,
) -> buck2_error::Result<buck2_cli_proto::NewGenericResponseMessage> {
    let start_event = ctx
        .command_start_event(buck2_data::DevCommandStart {}.into())
        .await?;
    span_async(start_event, async move {
        let events = Events(Mutex::new(partial_result_dispatcher));
        let result = run(ctx, &events, requests)
            .await
            .map(|()| buck2_cli_proto::NewGenericResponseMessage {
                new_generic_response: String::new(),
            });
        let end_event = command_end(&result, buck2_data::DevCommandEnd {});
        (result, end_event)
    })
    .await
}

/// Logs a timing line for measuring `buck2 dev`, at the `debug` level of `buck2_dev_timing`.
pub(crate) fn timing(what: std::fmt::Arguments<'_>) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    tracing::debug!(target: "buck2_dev_timing", "T {:.1} {}", now.as_secs_f64() * 1000.0, what);
}

/// Where the session's events go.
struct Events(Mutex<PartialResultDispatcher<NewGenericPartialResultMessage>>);

impl Events {
    fn emit(&self, event: DevSessionEvent) {
        match serde_json::to_string(&NewGenericPartialResult::DevSession(event)) {
            Ok(json) => {
                if let Ok(mut dispatcher) = self.0.lock() {
                    dispatcher.emit(NewGenericPartialResultMessage {
                        new_generic_partial_result: json,
                    });
                }
            }
            Err(e) => tracing::warn!("Could not serialize a `buck2 dev` session event: {e}"),
        }
    }
}

/// Something that a request asks the session to do, in a transaction. A build is one job for
/// each of its outputs.
#[derive(Clone)]
enum Job {
    Server {
        id: u64,
    },
    Watch {
        id: u64,
        request: DevWatchRequest,
        /// The call of each dynamic output of the request, with the state of its last answer.
        dynamic: Vec<(DevDynamicCall, Option<DevWatchState>)>,
    },
    Build {
        id: u64,
        name: String,
        /// The definition of a dynamic output, as it was when the build was asked for.
        call: Option<DevDynamicCall>,
    },
}

impl Job {
    /// The job could not run: say why.
    fn fail(&self, events: &Events, e: &buck2_error::Error) {
        match self {
            Job::Server { id } | Job::Watch { id, .. } => events.emit(DevSessionEvent::Failed {
                id: *id,
                errors: vec![format!("{e:#}")],
            }),
            Job::Build { id, name, .. } => events.emit(DevSessionEvent::Built {
                id: *id,
                name: name.clone(),
                result: Err(output_error(e)),
            }),
        }
    }
}

/// The transaction of an epoch, and the jobs that run in it.
struct Runner<'a> {
    epoch: u64,
    /// Jobs go to the transaction through this. `None` once the runner should end, as soon as
    /// its jobs are done.
    jobs: Option<mpsc::UnboundedSender<(u64, Job)>>,
    future: BoxFuture<'a, buck2_error::Result<()>>,
}

impl<'a> Runner<'a> {
    fn start(
        ctx: &'a dyn ServerCommandContextTrait,
        start: &'a DevSessionStart,
        events: &'a Events,
        done: mpsc::UnboundedSender<u64>,
        epoch: u64,
    ) -> Runner<'a> {
        let (jobs, receiver) = mpsc::unbounded_channel();
        let future = async move {
            // Subscribe before the transaction syncs the file watcher, so that every change after
            // what it sees is reported. One before is no change to it.
            timing(format_args!("daemon runner start epoch {epoch}"));
            let changes = ctx.subscribe_to_file_changes().await.ok();
            ctx.with_dice_ctx(move |server_ctx, dice| {
                timing(format_args!("daemon runner entered epoch {epoch}"));
                run_epoch(server_ctx, dice, start, events, receiver, done, changes)
            })
            .await
        }
        .boxed();
        Runner {
            epoch,
            jobs: Some(jobs),
            future,
        }
    }
}

/// The runner's end, or never if there is none.
async fn runner_end(runner: &mut Option<Runner<'_>>) -> buck2_error::Result<()> {
    match runner {
        Some(runner) => (&mut runner.future).await,
        None => futures::future::pending().await,
    }
}

async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => futures::future::pending().await,
    }
}

async fn run(
    ctx: &dyn ServerCommandContextTrait,
    events: &Events,
    mut requests: StreamingRequestHandler<buck2_cli_proto::DevSessionMessage>,
) -> buck2_error::Result<()> {
    let start = match parse(requests.message().await?)? {
        DevSessionRequest::Start(start) => start,
        _ => {
            return Err(internal_error!(
                "A `buck2 dev` session must start with `Start`"
            ));
        }
    };
    let start = &start;
    // The dynamic outputs that are defined, by name.
    let mut definitions: BTreeMap<String, DevDynamicCall> = BTreeMap::new();
    // Each job with a number of its own and its epoch: those that wait for a runner, and those
    // that a runner has.
    let mut queue: VecDeque<(u64, u64, Job)> = VecDeque::new();
    let mut running: BTreeMap<u64, (u64, Job)> = BTreeMap::new();
    let mut next_job = 0;
    let (done, mut finished) = mpsc::unbounded_channel();
    let mut runner: Option<Runner<'_>> = None;
    let mut idle_since: Option<tokio::time::Instant> = None;
    loop {
        // Give the jobs that wait to the runner of their epoch, or start one.
        while let Some((_, epoch, _)) = queue.front() {
            match &mut runner {
                None => runner = Some(Runner::start(ctx, start, events, done.clone(), *epoch)),
                Some(current) => match &current.jobs {
                    Some(jobs) if *epoch <= current.epoch => {
                        if let Some((seq, epoch, job)) = queue.pop_front() {
                            // If the runner has ended, its end puts the job back.
                            let _ignored = jobs.send((seq, job.clone()));
                            running.insert(seq, (epoch, job));
                        }
                    }
                    _ => {
                        // A later epoch: the runner ends once its jobs are done.
                        if running.is_empty() {
                            current.jobs = None;
                        }
                        break;
                    }
                },
            }
        }
        let idle = runner
            .as_ref()
            .is_some_and(|runner| runner.jobs.is_some() && running.is_empty());
        if !idle {
            idle_since = None;
        } else if idle_since.is_none() {
            idle_since = Some(tokio::time::Instant::now());
        }

        tokio::select! {
            request = requests.next() => {
                let Some(request) = request else {
                    // The client is done.
                    return Ok(());
                };
                match parse(request?)? {
                    DevSessionRequest::Start(_) => {
                        return Err(internal_error!("A `buck2 dev` session started twice"));
                    }
                    DevSessionRequest::Define(calls) => {
                        for call in calls {
                            definitions.insert(call.name.clone(), call);
                        }
                    }
                    DevSessionRequest::Undefine(names) => {
                        for name in names {
                            definitions.remove(&name);
                        }
                    }
                    DevSessionRequest::Run { id, epoch, action } => {
                        timing(format_args!("daemon request {id} epoch {epoch}"));
                        let jobs = match action {
                            DevAction::Server => vec![Job::Server { id }],
                            DevAction::Watch(request) => {
                                let dynamic = request
                                    .dynamic
                                    .iter()
                                    .filter_map(|dynamic| {
                                        let call = definitions.get(&dynamic.name)?.clone();
                                        Some((call, dynamic.previous.clone()))
                                    })
                                    .collect();
                                vec![Job::Watch { id, request, dynamic }]
                            }
                            DevAction::Build { outputs } => outputs
                                .into_iter()
                                .map(|name| Job::Build {
                                    id,
                                    call: definitions.get(&name).cloned(),
                                    name,
                                })
                                .collect(),
                            DevAction::Release => {
                                // Stop everything: dropping the runner cancels what runs in it.
                                runner = None;
                                queue.clear();
                                running.clear();
                                let response = match session_held_if_any::<SessionResources>(
                                    &start.session,
                                ) {
                                    Some(resources) => resources.release_all().await,
                                    None => Ok(()),
                                };
                                events.emit(match response {
                                    Ok(()) => DevSessionEvent::Response {
                                        id,
                                        response: DevResponse::Released,
                                    },
                                    Err(e) => DevSessionEvent::Failed {
                                        id,
                                        errors: vec![format!("{e:#}")],
                                    },
                                });
                                Vec::new()
                            }
                        };
                        for job in jobs {
                            next_job += 1;
                            queue.push_back((next_job, epoch, job));
                        }
                    }
                }
            }
            end = runner_end(&mut runner) => {
                runner = None;
                let left = std::mem::take(&mut running);
                match end {
                    Ok(()) => {}
                    Err(e) if e.has_tag(buck2_error::ErrorTag::DaemonPreempted) => {
                        // Another command took the daemon. Run what was running again, once
                        // that command lets the session in.
                        for (seq, (epoch, job)) in left.into_iter().rev() {
                            queue.push_front((seq, epoch, job));
                        }
                    }
                    Err(e) => {
                        for (_, (_, job)) in left {
                            job.fail(events, &e);
                        }
                    }
                }
            }
            Some(seq) = finished.recv() => {
                running.remove(&seq);
            }
            () = sleep_until(idle_since.map(|since| since + IDLE_RELEASE)) => {
                if let Some(runner) = &mut runner {
                    runner.jobs = None;
                }
            }
        }
    }
}

fn parse(message: buck2_cli_proto::DevSessionMessage) -> buck2_error::Result<DevSessionRequest> {
    serde_json::from_str(&message.dev_session_request)
        .buck_error_context("Could not deserialize a `buck2 dev` session request")
}

/// What becomes of a build that has started.
#[derive(Default)]
struct Flags {
    /// The client got its `Built`.
    built: bool,
    /// It stopped, and the client got `Changed` instead.
    stopped: bool,
}

/// A build of an output that runs in the transaction, for stopping it when a file that it is
/// built from changes.
struct Building {
    id: u64,
    name: String,
    abort: AbortHandle,
    flags: Arc<Mutex<Flags>>,
    /// What the output may be built from, and its slots there, once known.
    claims: Option<(Arc<DevClaims>, u64)>,
    /// The client got `Changed` for it.
    reported: bool,
}

/// The changes that the file watcher reported since the transaction started.
#[derive(Default)]
struct ChangeLog {
    paths: Vec<ProjectRelativePathBuf>,
    missed: bool,
}

/// Run the jobs of an epoch in its transaction, until `jobs` closes and they are done.
async fn run_epoch(
    server_ctx: &dyn ServerCommandContextTrait,
    dice: DiceTransaction,
    start: &DevSessionStart,
    events: &Events,
    mut jobs: mpsc::UnboundedReceiver<(u64, Job)>,
    done: mpsc::UnboundedSender<u64>,
    changes: Option<BoxStream<'static, FileChangeBatch>>,
) -> buck2_error::Result<()> {
    let dice = &dice;
    let target: OnceCell<buck2_error::Result<Target>> = OnceCell::new();
    let target = &target;
    let io = dice
        .ctx()
        .global_data()
        .get::<Arc<dyn IoProvider>>()
        .map_err(|_| internal_error!("The daemon has no I/O provider"))?
        .dupe();
    let (found_tx, mut found_rx) = mpsc::unbounded_channel::<(u64, Arc<DevClaims>, u64)>();
    let found_tx = &found_tx;
    let mut changes = changes.unwrap_or_else(|| futures::stream::pending().boxed());
    let mut log = ChangeLog::default();
    let mut builds: BTreeMap<u64, Building> = BTreeMap::new();
    let mut running: FuturesUnordered<BoxFuture<'_, u64>> = FuturesUnordered::new();
    let mut closed = false;
    loop {
        if closed && running.is_empty() {
            return Ok(());
        }
        tokio::select! {
            job = jobs.recv(), if !closed => {
                let Some((seq, job)) = job else {
                    closed = true;
                    continue;
                };
                let future: BoxFuture<'_, ()> = match job {
                    Job::Server { id } => async move {
                        let response = async {
                            let target = target_of(target, server_ctx, dice, start).await?;
                            server_action(server_ctx, dice, target, &start.session).await
                        }
                        .await;
                        events.emit(match response {
                            Ok(info) => DevSessionEvent::Response {
                                id,
                                response: DevResponse::Server(info),
                            },
                            Err(e) => DevSessionEvent::Failed {
                                id,
                                errors: vec![format!("{e:#}")],
                            },
                        });
                    }
                    .boxed(),
                    Job::Watch {
                        id,
                        request,
                        dynamic,
                    } => async move {
                        let response = async {
                            let target = target_of(target, server_ctx, dice, start).await?;
                            watch_action(
                                server_ctx,
                                dice,
                                target,
                                &start.session,
                                &request,
                                &dynamic,
                            )
                            .await
                        }
                        .await;
                        events.emit(match response {
                            Ok(watched) => DevSessionEvent::Response {
                                id,
                                response: DevResponse::Watch(watched),
                            },
                            Err(e) => {
                                // What fixes the target may be outside what it was built from.
                                clear_interest(&start.session);
                                DevSessionEvent::Failed {
                                    id,
                                    errors: vec![format!("{e:#}")],
                                }
                            }
                        });
                    }
                    .boxed(),
                    Job::Build { id, name, call } => {
                        let flags = Arc::new(Mutex::new(Flags::default()));
                        let (future, abort) = futures::future::abortable(build_job(
                            server_ctx,
                            dice,
                            target,
                            start,
                            events,
                            found_tx,
                            seq,
                            id,
                            name.clone(),
                            call,
                            flags.dupe(),
                        ));
                        builds.insert(
                            seq,
                            Building {
                                id,
                                name,
                                abort,
                                flags,
                                claims: None,
                                reported: false,
                            },
                        );
                        future.map(|_| ()).boxed()
                    }
                };
                running.push(future.map(move |()| seq).boxed());
            }
            Some(seq) = running.next() => {
                builds.remove(&seq);
                // The session is ending if this fails.
                let _ignored = done.send(seq);
            }
            Some((seq, claims, slots)) = found_rx.recv() => {
                if let Some(building) = builds.get_mut(&seq) {
                    building.claims = Some((claims, slots));
                    check(dice, &io, events, building, &log.paths, log.missed).await;
                }
            }
            Some(batch) = changes.next() => {
                for building in builds.values_mut() {
                    check(dice, &io, events, building, &batch.paths, batch.missed).await;
                }
                log.missed |= batch.missed;
                log.paths.extend(batch.paths);
            }
        }
    }
}

/// The target of the transaction, analysed once for all its jobs.
async fn target_of<'a>(
    cell: &'a OnceCell<buck2_error::Result<Target>>,
    server_ctx: &dyn ServerCommandContextTrait,
    dice: &DiceTransaction,
    start: &DevSessionStart,
) -> buck2_error::Result<&'a Target> {
    cell.get_or_init(|| Target::resolve(server_ctx, dice, start))
        .await
        .as_ref()
        .map_err(|e| e.dupe())
}

/// Stop `building`, or tell the client that it is out of date, if a file that it is built from
/// changed after the transaction saw it: one of `paths`, or anything if `missed`.
async fn check(
    dice: &DiceTransaction,
    io: &Arc<dyn IoProvider>,
    events: &Events,
    building: &mut Building,
    paths: &[ProjectRelativePathBuf],
    missed: bool,
) {
    if building.reported {
        return;
    }
    let mut changed = Vec::new();
    if !missed {
        let Some((claims, slots)) = &building.claims else {
            return;
        };
        for path in paths {
            // A path that cannot be looked at counts as changed.
            if changed_since(&mut dice.ctx(), claims, *slots, io, path)
                .await
                .unwrap_or(true)
            {
                changed.push(path.to_string());
            }
        }
        if changed.is_empty() {
            return;
        }
    }
    let stopped = match building.flags.lock() {
        Ok(mut flags) if !flags.built => {
            flags.stopped = true;
            true
        }
        _ => false,
    };
    if missed && !stopped {
        // The client hears of missed changes itself, and knows that anything may have changed.
        return;
    }
    if stopped {
        building.abort.abort();
    }
    building.reported = true;
    events.emit(DevSessionEvent::Changed {
        id: building.id,
        name: building.name.clone(),
        paths: changed,
        missed,
        stopped,
    });
}

/// Build output `name`, which is a dynamic output if it has a `call`, and send the client its
/// `Built` as soon as it is known. What it may be built from is found next to the build, and goes
/// to `found` for `check`, and, for a dynamic output, to the client.
#[allow(clippy::too_many_arguments)]
async fn build_job(
    server_ctx: &dyn ServerCommandContextTrait,
    dice: &DiceTransaction,
    target: &OnceCell<buck2_error::Result<Target>>,
    start: &DevSessionStart,
    events: &Events,
    found: &mpsc::UnboundedSender<(u64, Arc<DevClaims>, u64)>,
    seq: u64,
    id: u64,
    name: String,
    call: Option<DevDynamicCall>,
    flags: Arc<Mutex<Flags>>,
) {
    // Returns whether the client got it: not if the build stopped.
    let built = |result: Result<_, DevOutputError>| -> bool {
        let Ok(mut flags) = flags.lock() else {
            return false;
        };
        if flags.stopped {
            return false;
        }
        flags.built = true;
        events.emit(DevSessionEvent::Built {
            id,
            name: name.clone(),
            result,
        });
        true
    };
    timing(format_args!("daemon job {id} {name}"));
    let target = match target_of(target, server_ctx, dice, start).await {
        Ok(target) => target,
        Err(e) => {
            built(Err(output_error(&e)));
            return;
        }
    };
    timing(format_args!("daemon target {id} {name}"));
    let resolved = async {
        let outputs = target.info.view().outputs()?;
        match &call {
            Some(call) => {
                let (handler, output) = resolve_dynamic(
                    server_ctx,
                    dice,
                    &target.subject,
                    &target.info,
                    &outputs,
                    &target.global_cfg_options,
                    call,
                )
                .await?;
                let wanted = Wanted {
                    name: name.clone(),
                    output,
                    sidecars: Vec::new(),
                };
                buck2_error::Ok((wanted, Some(handler), slot_bit(1)))
            }
            None => {
                let (index, output) = outputs
                    .iter()
                    .enumerate()
                    .find(|(_, (output, _))| **output == name)
                    .map(|(index, (_, output))| (index, output.clone()))
                    .ok_or_else(|| DevError::UnknownOutput {
                        subject: target.subject.dupe(),
                        name: name.clone(),
                    })?;
                let sidecars = target
                    .info
                    .view()
                    .sidecars()?
                    .get(&name)
                    .cloned()
                    .unwrap_or_default();
                let wanted = Wanted {
                    name: name.clone(),
                    output,
                    sidecars,
                };
                // Slot 0 is the server.
                Ok((wanted, None, slot_bit(index + 1)))
            }
        }
    }
    .await;
    let (wanted, handler, slots) = match resolved {
        Ok(resolved) => resolved,
        Err(e) => {
            built(Err(output_error(&e)));
            return;
        }
    };
    let info = &target.info;
    let argv = move |name: &str, artifact_fs: &ArtifactFs, ensured: &Ensured| {
        let command = info
            .view()
            .output_command_line(name)
            .ok_or_else(|| internal_error!("Output `{name}` is not a command line"))?;
        command_argv(artifact_fs, command, ensured)
    };
    timing(format_args!("daemon resolved {id} {name}"));
    let dynamic = handler.is_some();
    let build = async {
        let result = build_output(server_ctx.project_root(), &mut dice.ctx(), wanted, &argv).await;
        timing(format_args!("daemon built {id} {name}"));
        built(result)
    };
    let claims = async {
        let claims = match handler {
            Some(handler) => dynamic_claims(&mut dice.ctx(), &target.subject, handler).await,
            None => dev_claims(&mut dice.ctx(), &target.subject).await,
        };
        // An output whose claims cannot be found is not stopped by changes, and a `Watch` says
        // that any change may be to it.
        let claims = claims.ok()?;
        timing(format_args!("daemon found {id} {name}"));
        if dynamic {
            // From now on the session's file change subscription lets through the changes to
            // what the output is built from. `check` looks at those before.
            add_interest(&start.session, &claims.interest);
        }
        let _ignored = found.send((seq, claims.dupe(), slots));
        Some(claims)
    };
    let (got, claims) = futures::future::join(build, claims).await;
    if got && dynamic {
        if let Some(claims) = claims {
            events.emit(DevSessionEvent::Found {
                id,
                name,
                state: claims.state(),
            });
        }
    }
}
