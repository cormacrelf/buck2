/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

#![cfg_attr(feature = "gazebo_lint", allow(deprecated))] // :(

use std::fs::File;
use std::path::PathBuf;
use std::process;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use allocative::Allocative;
use anyhow::Context as _;
use async_trait::async_trait;
use buck2_audit_server::server::server_audit_command;
use buck2_cli_proto::DaemonProcessInfo;
use buck2_client_ctx::daemon_constraints::gen_daemon_constraints;
use buck2_client_ctx::version::BuckVersion;
use buck2_common::buckd_connection::ConnectionType;
use buck2_common::daemon_dir::DaemonDir;
use buck2_common::invocation_paths::InvocationPaths;
use buck2_common::legacy_configs::init::DaemonStartupConfig;
use buck2_common::memory;
use buck2_core::env_helper::EnvHelper;
use buck2_core::fs::fs_util;
use buck2_core::logging::LogConfigurationReloadHandle;
use buck2_server::builtin_docs::docs::docs_command;
use buck2_server::daemon::daemon_tcp::create_listener;
use buck2_server::daemon::server::BuckdServer;
use buck2_server::daemon::server::BuckdServerDelegate;
use buck2_server::daemon::server::BuckdServerDependencies;
use buck2_server::daemon::server::BuckdServerInitPreferences;
use buck2_server::profile::profile_command;
use buck2_server_ctx::bxl::BXL_SERVER_COMMANDS;
use buck2_server_ctx::ctx::ServerCommandContextTrait;
use buck2_server_ctx::partial_result_dispatcher::NoPartialResult;
use buck2_server_ctx::partial_result_dispatcher::PartialResultDispatcher;
use buck2_starlark::server::server_starlark_command;
use futures::channel::mpsc;
use futures::channel::mpsc::UnboundedSender;
use futures::pin_mut;
use futures::select;
use futures::FutureExt;
use futures::StreamExt;
use rand::Rng;
use tokio::runtime::Builder;

use crate::commands::daemon_lower_priority::daemon_lower_priority;
use crate::commands::schedule_termination::maybe_schedule_termination;
use crate::DaemonBeforeSubcommandOptions;

#[derive(Debug, buck2_error::Error)]
enum DaemonError {
    #[error("The buckd pid file at `{0}` had a mismatched pid, expected `{1}`, got `{2}`")]
    PidFileMismatch(PathBuf, u32, u32),
}

/// Start or run buck daemon.
///
/// This is an internal command, not intended to be used directly.
/// Buck client invokes it to spawn a server process.
#[derive(Clone, Debug, clap::Parser)]
pub(crate) struct DaemonCommand {
    /// Sets the interval for how often the daemon performs consistency checks.
    /// These are used to ensure that the daemon is still the one referenced
    /// by files in the daemon dir.
    #[clap(long, default_value("60"))]
    checker_interval_seconds: u64,
    /// Run buck daemon but do not daemonize the process.
    #[clap(long)]
    dont_daemonize: bool,
    /// This flag is set to prevent infinite recursion when the process is restarted
    /// with lower priority.
    #[clap(long)]
    skip_macos_qos: bool,
    /// Early configs that the daemon needs at startup. Those are read by the client then passed to
    /// the daemon. The client will restart the daemon if they mismatch.
    #[clap(parse(try_from_str = DaemonStartupConfig::deserialize))]
    daemon_startup_config: DaemonStartupConfig,
}

impl DaemonCommand {
    /// Command instance for `--no-buckd`.
    pub(crate) fn new_in_process(daemon_startup_config: DaemonStartupConfig) -> DaemonCommand {
        DaemonCommand {
            checker_interval_seconds: 60,
            dont_daemonize: true,
            skip_macos_qos: true,
            daemon_startup_config,
        }
    }
}

struct BuckdServerDependenciesImpl;

#[async_trait]
impl BuckdServerDependencies for BuckdServerDependenciesImpl {
    async fn audit(
        &self,
        ctx: &dyn ServerCommandContextTrait,
        partial_result_dispatcher: PartialResultDispatcher<buck2_cli_proto::StdoutBytes>,
        req: buck2_cli_proto::GenericRequest,
    ) -> anyhow::Result<buck2_cli_proto::GenericResponse> {
        server_audit_command(ctx, partial_result_dispatcher, req).await
    }
    async fn starlark(
        &self,
        ctx: &dyn ServerCommandContextTrait,
        partial_result_dispatcher: PartialResultDispatcher<buck2_cli_proto::StdoutBytes>,
        req: buck2_cli_proto::GenericRequest,
    ) -> anyhow::Result<buck2_cli_proto::GenericResponse> {
        server_starlark_command(ctx, partial_result_dispatcher, req).await
    }
    async fn profile(
        &self,
        ctx: &dyn ServerCommandContextTrait,
        partial_result_dispatcher: PartialResultDispatcher<NoPartialResult>,
        req: buck2_cli_proto::ProfileRequest,
    ) -> anyhow::Result<buck2_cli_proto::ProfileResponse> {
        match req.profile_opts.as_ref().expect("Missing profile opts") {
            buck2_cli_proto::profile_request::ProfileOpts::TargetProfile(_) => {
                profile_command(ctx, partial_result_dispatcher, req).await
            }
            buck2_cli_proto::profile_request::ProfileOpts::BxlProfile(_) => {
                BXL_SERVER_COMMANDS
                    .get()?
                    .bxl_profile(ctx, partial_result_dispatcher, req)
                    .await
            }
        }
    }
    async fn docs(
        &self,
        ctx: &dyn ServerCommandContextTrait,
        partial_result_dispatcher: PartialResultDispatcher<NoPartialResult>,
        req: buck2_cli_proto::UnstableDocsRequest,
    ) -> anyhow::Result<buck2_cli_proto::UnstableDocsResponse> {
        docs_command(ctx, partial_result_dispatcher, req).await
    }
}

pub(crate) fn init_listener() -> anyhow::Result<(std::net::TcpListener, ConnectionType)> {
    let (endpoint, listener) = create_listener()?;

    tracing::info!("Listener created on {}", &endpoint);

    Ok((listener, endpoint))
}

pub(crate) fn write_process_info(
    daemon_dir: &DaemonDir,
    process_info: &DaemonProcessInfo,
) -> anyhow::Result<()> {
    let file = File::create(daemon_dir.buckd_info())?;
    serde_json::to_writer(&file, &process_info)?;
    Ok(())
}

fn verify_current_daemon(daemon_dir: &DaemonDir) -> anyhow::Result<()> {
    let file = daemon_dir.buckd_pid();
    let my_pid = process::id();

    let recorded_pid: u32 = fs_util::read_to_string(&file)?.trim().parse()?;
    if recorded_pid != my_pid {
        return Err(
            DaemonError::PidFileMismatch(file.into_path_buf(), my_pid, recorded_pid).into(),
        );
    }

    Ok(())
}

fn gen_auth_token() -> String {
    (0..20)
        .map(|_| rand::thread_rng().gen_range('a'..='z'))
        .collect()
}

fn terminate_on_panic() {
    let orig_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        orig_hook(panic_info);
        // We are using `_exit` instead of `exit` to avoid running global destructors.
        // This is similar to what default rust panic handler does
        // when there `panic=abort`: it does `abort`.
        unsafe { libc::_exit(1) }
    }));
}

impl DaemonCommand {
    fn run(
        self,
        fb: fbinit::FacebookInit,
        log_reload_handle: Arc<dyn LogConfigurationReloadHandle>,
        paths: InvocationPaths,
        before_subcommand_options: DaemonBeforeSubcommandOptions,
        in_process: bool,
        listener_created: impl FnOnce() + Send,
    ) -> anyhow::Result<()> {
        // NOTE: Do not create any threads before this point.
        //   Daemonize does not preserve threads.

        let server_init_ctx = BuckdServerInitPreferences {
            detect_cycles: before_subcommand_options.detect_cycles,
            which_dice: before_subcommand_options.which_dice,
            enable_trace_io: before_subcommand_options.enable_trace_io,
            reject_materializer_state: before_subcommand_options
                .reject_materializer_state
                .map(|s| s.into()),
            daemon_startup_config: self.daemon_startup_config,
        };

        let span = tracing::info_span!("daemon_listener");
        let span_guard = span.enter();

        let daemon_dir = paths.daemon_dir()?;
        let pid_path = daemon_dir.buckd_pid();
        let stdout_path = daemon_dir.buckd_stdout();
        let stderr_path = daemon_dir.buckd_stderr();
        // Even if we don't redirect output, we still need to create stdout/stderr files,
        // because tailer opens them. This is untidy.
        let stdout = File::create(stdout_path)?;
        let stderr = File::create(stderr_path)?;

        let auth_token = gen_auth_token();

        let (listener, process_info, endpoint) = if !self.dont_daemonize {
            // We must create stdout/stderr before creating a listener,
            // otherwise it is race:
            // * daemon parent process exits
            // * client successfully connects to the unix socket
            // * but stdout/stderr may be not yet created, so tailer fails to open them
            let (listener, endpoint) = init_listener()?;

            Self::daemonize(stdout, stderr)?;

            fs_util::write(&pid_path, format!("{}", process::id()))?;

            let pid = process::id();
            let process_info = DaemonProcessInfo {
                pid: pid as i64,
                endpoint: endpoint.to_string(),
                version: BuckVersion::get().unique_id().to_owned(),
                auth_token,
            };

            // TODO(nga): this code is executed after server daemonization,
            //   so client has to retry to read it. Fix it.
            write_process_info(&daemon_dir, &process_info)?;

            tracing::info!("Daemonized.");

            (listener, process_info, endpoint)
        } else {
            fs_util::write(&pid_path, format!("{}", process::id()))?;

            if !in_process {
                Self::redirect_output(stdout, stderr)?;
            }

            let (listener, endpoint) = init_listener()?;

            let process_info = DaemonProcessInfo {
                pid: process::id() as i64,
                endpoint: endpoint.to_string(),
                version: BuckVersion::get().unique_id().to_owned(),
                auth_token,
            };

            write_process_info(&daemon_dir, &process_info)?;

            (listener, process_info, endpoint)
        };

        tracing::info!("Starting Buck2 daemon");
        tracing::info!("Version: {}", BuckVersion::get_version());
        tracing::info!("PID: {}", process::id());
        tracing::info!("ID: {}", *buck2_events::daemon_id::DAEMON_UUID);
        tracing::info!("Endpoint: {}", endpoint);

        listener_created();

        terminate_on_panic();

        maybe_schedule_termination()?;

        // Higher performance for jemalloc, recommended (but may not have any effect on Mac)
        // https://github.com/jemalloc/jemalloc/blob/dev/TUNING.md#notable-runtime-options-for-performance-tuning
        memory::enable_background_threads()?;

        if cfg!(target_os = "linux") {
            #[cfg(fbcode_build)]
            {
                gflags::set_gflag_value(
                    fb,
                    "cgroup2_reader_update_interval_ms",
                    gflags::GflagValue::U32(2000),
                )
                .expect("failed to set gflag --cgroup2_reader_update_interval_ms");
            }
        }

        let mut builder = Builder::new_multi_thread();
        builder.enable_all();
        builder.thread_name("buck2-rt");

        static RUNTIME_THREADS: EnvHelper<usize> = EnvHelper::new("BUCK2_RUNTIME_THREADS");
        if let Some(threads) = RUNTIME_THREADS.get_copied()? {
            builder.worker_threads(threads);
        }

        static MAX_BLOCKING_THREADS: EnvHelper<usize> =
            EnvHelper::new("BUCK2_MAX_BLOCKING_THREADS");
        if let Some(threads) = MAX_BLOCKING_THREADS.get_copied()? {
            builder.max_blocking_threads(threads);
        }

        tracing::info!("Starting tokio runtime...");

        let rt = builder.build().context("Error creating Tokio runtime")?;
        let handle = rt.handle().clone();

        let rt = Builder::new_multi_thread()
            .enable_all()
            .thread_name("buck2-tn")
            // These values are arbitrary, but I/O shouldn't take up many threads.
            .worker_threads(2)
            .max_blocking_threads(2)
            .build()
            .context("Error creating Tonic Tokio runtime")?;

        rt.block_on(async move {
            // Once any item is received on the hard_shutdown_receiver, the daemon process will exit immediately.
            let (hard_shutdown_sender, mut hard_shutdown_receiver) = mpsc::unbounded();

            #[derive(Allocative)]
            struct Delegate {
                #[allocative(skip)]
                hard_shutdown_sender: UnboundedSender<String>,
            }

            impl BuckdServerDelegate for Delegate {
                fn force_shutdown_with_timeout(&self, reason: String, timeout: Duration) {
                    let sender = self.hard_shutdown_sender.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(timeout).await;
                        sender.unbounded_send(reason).expect("Shouldn't happen.");
                    });
                }
            }

            let delegate = Box::new(Delegate {
                hard_shutdown_sender: hard_shutdown_sender.clone(),
            });
            let daemon_dir = paths.daemon_dir()?;

            listener.set_nonblocking(true)?;
            let listener = tokio::net::TcpListener::from_std(listener)?;
            let listener = tokio_stream::wrappers::TcpListenerStream::new(listener);

            tracing::info!("Listening.");

            drop(span_guard);

            let daemon_constraints =
                gen_daemon_constraints(&server_init_ctx.daemon_startup_config)?;

            let buckd_server = BuckdServer::run(
                fb,
                log_reload_handle,
                paths,
                delegate,
                server_init_ctx,
                process_info,
                daemon_constraints,
                Box::pin(listener),
                &BuckdServerDependenciesImpl,
                handle,
            )
            .fuse();
            let shutdown_future = async move { hard_shutdown_receiver.next().await }.fuse();
            pin_mut!(buckd_server);
            pin_mut!(shutdown_future);

            let checker_interval_seconds = self.checker_interval_seconds;

            thread::Builder::new()
                .name("check-daemon-dir".to_owned())
                .spawn(move || {
                    Self::check_daemon_dir_thread(
                        checker_interval_seconds,
                        daemon_dir,
                        hard_shutdown_sender,
                    )
                })?;

            tracing::info!("Initialization complete, running the server.");

            select! {
                res = buckd_server => {
                    tracing::info!("server shutdown");
                    res
                }
                reason = shutdown_future => {
                    let reason = reason.as_deref().unwrap_or("no reason available");
                    tracing::info!("server forced shutdown: {}", reason);
                    anyhow::Ok(())
                },
            }
        })
    }

    /// We start a dedicated thread to periodically check that the files in the daemon
    /// dir still reflect that we are the current buckd and verify that when you connect
    /// to the server it is our server.
    /// It gets a dedicated thread so that if somehow the main runtime gets all jammed up,
    /// this will still run (and presumably connecting to the server or our request would
    /// then fail and we'd do a hard shutdown).
    fn check_daemon_dir_thread(
        checker_interval_seconds: u64,
        daemon_dir: DaemonDir,
        hard_shutdown_sender: UnboundedSender<String>,
    ) {
        let this_rt = Builder::new_current_thread().enable_all().build().unwrap();

        this_rt.block_on(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(checker_interval_seconds)).await;
                match verify_current_daemon(&daemon_dir) {
                    Ok(()) => {}
                    Err(e) => {
                        // This bit of code cannot relay errors, ignoring that we can't log
                        // a warning is reasonable.
                        let _ignored = buck2_client_ctx::eprintln!(
                            "daemon verification failed, forcing shutdown: {:#}",
                            e
                        );

                        // If this is already shutting down, we don't need to do it again.
                        let _ignored = hard_shutdown_sender
                            .unbounded_send("Daemon verification failed".to_owned());
                    }
                };
            }
        })
    }

    pub(crate) fn exec(
        self,
        init: fbinit::FacebookInit,
        log_reload_handle: Arc<dyn LogConfigurationReloadHandle>,
        paths: InvocationPaths,
        before_subcommand_options: DaemonBeforeSubcommandOptions,
        in_process: bool,
        listener_created: impl FnOnce() + Send,
    ) -> anyhow::Result<()> {
        daemon_lower_priority(self.skip_macos_qos)?;

        let project_root = paths.project_root();
        let daemon_dir = paths.daemon_dir()?;

        if !daemon_dir.path.is_dir() {
            fs_util::create_dir_all(&daemon_dir.path)?;
        }

        // TODO(nga): this breaks relative paths in `--no-buckd`.
        //   `--no-buckd` should capture correct directories earlier.
        //   Or even better, client should set current directory to project root,
        //   and resolve all paths relative to original cwd.
        fs_util::set_current_dir(project_root.root())?;

        self.run(
            init,
            log_reload_handle,
            paths,
            before_subcommand_options,
            in_process,
            listener_created,
        )?;
        Ok(())
    }

    #[cfg(unix)]
    fn redirect_output(stdout: File, stderr: File) -> anyhow::Result<()> {
        use std::os::unix::io::AsRawFd;

        nix::unistd::dup2(stdout.as_raw_fd(), nix::libc::STDOUT_FILENO)?;
        nix::unistd::dup2(stderr.as_raw_fd(), nix::libc::STDERR_FILENO)?;
        Ok(())
    }

    #[cfg(windows)]
    fn redirect_output(stdout: File, stderr: File) -> anyhow::Result<()> {
        use std::os::windows::io::AsRawHandle;

        unsafe {
            let stdout_fd = libc::open_osfhandle(stdout.as_raw_handle() as isize, libc::O_RDWR);
            let stderr_fd = libc::open_osfhandle(stderr.as_raw_handle() as isize, libc::O_RDWR);
            if stdout_fd == -1 || stderr_fd == -1 {
                return Err(anyhow::Error::msg(
                    "Can't get file descriptors for output files",
                ));
            }
            // MSVC libc doesn't export STDOUT_FILENO and STDERR_FILENO.
            let stdout_exit_code = libc::dup2(stdout_fd, 1);
            let stderr_exit_code = libc::dup2(stderr_fd, 2);
            if stdout_exit_code == -1 || stderr_exit_code == -1 {
                return Err(anyhow::Error::msg("Failed to redirect daemon output"));
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    fn daemonize(stdout: File, stderr: File) -> anyhow::Result<()> {
        // TODO(cjhopman): Daemonize is pretty un-maintained. We may need to move
        // to something else or just do it ourselves.
        let daemonize = crate::commands::daemonize::Daemonize::new()
            .stdout(stdout)
            .stderr(stderr);
        daemonize.start()?;
        Ok(())
    }

    #[cfg(windows)]
    /// Restart current process in detached mode with '--dont-daemonize' flag.
    fn daemonize(_stdout: File, _stderr: File) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("Cannot daemonize on Windows"))
    }
}

#[cfg(test)]
mod tests {
    use std::process;
    use std::time::Duration;

    use allocative::Allocative;
    use anyhow::Context;
    use buck2_cli_proto::DaemonProcessInfo;
    use buck2_cli_proto::KillRequest;
    use buck2_cli_proto::PingRequest;
    use buck2_client_ctx::daemon::client::connect::new_daemon_api_client;
    use buck2_client_ctx::daemon_constraints::gen_daemon_constraints;
    use buck2_common::invocation_paths::InvocationPaths;
    use buck2_common::invocation_roots::InvocationRoots;
    use buck2_common::legacy_configs::init::DaemonStartupConfig;
    use buck2_core::fs::paths::file_name::FileNameBuf;
    use buck2_core::fs::project::ProjectRootTemp;
    use buck2_core::logging::LogConfigurationReloadHandle;
    use buck2_server::daemon::daemon_tcp::create_listener;
    use buck2_server::daemon::server::BuckdServer;
    use buck2_server::daemon::server::BuckdServerDelegate;
    use buck2_server::daemon::server::BuckdServerInitPreferences;
    use dupe::Dupe;
    use rand::RngCore;
    use rand::SeedableRng;
    use tokio::runtime::Handle;

    use crate::commands::daemon::BuckdServerDependenciesImpl;

    // `fbinit_tokio` is not on crates, so we cannot use `#[fbinit::test]`.
    #[tokio::test]
    async fn test_daemon_smoke() {
        // TODO(nga): this should be `fbinit::perform_init`, but it is not on crates yet.
        let fbinit = unsafe { fbinit::assume_init() };

        let project_root = ProjectRootTemp::new().unwrap();

        let (endpoint, listener) = create_listener().unwrap();
        listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        let listener = tokio_stream::wrappers::TcpListenerStream::new(listener);

        let invocation_paths = InvocationPaths {
            roots: InvocationRoots {
                cell_root: project_root.path().root().to_buf(),
                project_root: project_root.path().dupe(),
            },
            isolation: FileNameBuf::try_from("v2".to_owned()).unwrap(),
        };

        #[derive(Allocative)]
        struct Delegate;

        impl BuckdServerDelegate for Delegate {
            fn force_shutdown_with_timeout(&self, _reason: String, _timeout: Duration) {}
        }

        let process_info = DaemonProcessInfo {
            endpoint: endpoint.to_string(),
            pid: process::id() as i64,
            version: "13.17.19".to_owned(),
            auth_token: "abc".to_owned(),
        };

        let handle = tokio::spawn(BuckdServer::run(
            fbinit,
            <dyn LogConfigurationReloadHandle>::noop(),
            invocation_paths,
            Box::new(Delegate),
            BuckdServerInitPreferences {
                detect_cycles: None,
                which_dice: None,
                enable_trace_io: false,
                reject_materializer_state: None,
                daemon_startup_config: DaemonStartupConfig::testing_empty(),
            },
            process_info.clone(),
            gen_daemon_constraints(&DaemonStartupConfig::testing_empty()).unwrap(),
            Box::pin(listener),
            &BuckdServerDependenciesImpl,
            Handle::current(),
        ));

        let mut client = new_daemon_api_client(endpoint.clone(), process_info.auth_token)
            .await
            .unwrap();

        client.ping(PingRequest::default()).await.unwrap();

        let mut client_with_wrong_token = new_daemon_api_client(endpoint, "wrong_token".to_owned())
            .await
            .unwrap();

        let err = format!(
            "{:#}",
            client_with_wrong_token
                .ping(PingRequest::default())
                .await
                .unwrap_err()
        );
        assert!(err.contains("invalid auth token"), "Error is: {}", err);

        client.ping(PingRequest::default()).await.unwrap();

        for req_size in [0, 1 << 10, 1 << 20, 10 << 20, 100 << 20] {
            let mut payload = vec![0; req_size];
            rand::rngs::SmallRng::seed_from_u64(20).fill_bytes(&mut payload);
            client
                .ping(PingRequest {
                    payload,
                    ..PingRequest::default()
                })
                .await
                .context(format!("req_size={}", req_size))
                .unwrap();
        }

        for resp_size in [0, 1 << 10, 1 << 20, 10 << 20, 100 << 20] {
            client
                .ping(PingRequest {
                    response_payload_size: resp_size,
                    ..PingRequest::default()
                })
                .await
                .context(format!("resp_size={}", resp_size))
                .unwrap();
        }

        client.kill(KillRequest::default()).await.unwrap();

        handle
            .await
            .expect("handle join failed")
            .expect("daemon returned error");
    }
}
