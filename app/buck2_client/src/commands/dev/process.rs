/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! The `buck2 dev` server process.
//!
//! The server runs in its own process group, so that stopping it stops what it started too.
//! Unless `DevInfo(interactive = True)`, its stdin is empty and it stays in the background:
//! Ctrl-C reaches only `buck2 dev`, which stops it. An interactive server inherits stdin, and its
//! group becomes the terminal's foreground group, so that it gets the keys typed and Ctrl-C.
//! `buck2 dev` takes the terminal back when it stops.
//!
//! For job control, `buck2 dev` stops the server's group with itself on Ctrl-Z, taking the
//! terminal back so that the shell gets it, and continues the group, giving the terminal back,
//! when the shell continues `buck2 dev`.

use std::io::IsTerminal;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::process::Stdio;
use std::time::Duration;

use buck2_client_ctx::subscribers::subscriber::SessionUpdate;
use buck2_error::BuckErrorContext;
use buck2_error::BuckErrorOptionContext;
use buck2_error::ErrorTag;
use buck2_error::buck2_error;
use tokio::sync::mpsc;
use tokio::sync::watch;

use super::pty::Pty;

/// How often `buck2 dev` checks whether a stopping server and the processes it started have
/// exited.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(super) struct ServerProcess {
    /// The exit status, once the server has exited. A task waits for it.
    exit: watch::Receiver<Option<ExitStatus>>,
    group: Group,
    /// For an interactive server that has the terminal: how to take it back.
    terminal: Option<Terminal>,
    /// `buck2 dev` stopped the group with itself, or the server stopped when it read the
    /// terminal while `buck2 dev` was in the background. `resume` continues it.
    paused: bool,
    /// The server's stdout and stderr, when `buck2 dev` shows them in its console.
    pty: Option<Pty>,
}

impl ServerProcess {
    /// Start `argv` in `cwd`, with `BUCK2_DEV_SOCKET` set to `socket`. With `output`, its
    /// stdout and stderr go there as lines, rather than to the terminal.
    pub(super) fn spawn(
        argv: &[String],
        cwd: &str,
        interactive: bool,
        socket: &str,
        output: Option<mpsc::UnboundedSender<SessionUpdate>>,
    ) -> buck2_error::Result<ServerProcess> {
        let (program, args) = argv.split_first().ok_or_else(|| {
            buck2_error!(
                ErrorTag::Input,
                "The `DevInfo` server command line is empty"
            )
        })?;
        // Not `background_command`: the server shares the terminal with `buck2 dev`.
        // ast-grep-ignore: rust/buck2-no-command-new
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .current_dir(cwd)
            .env(super::channel::SOCKET_ENV, socket)
            .stdin(if interactive {
                Stdio::inherit()
            } else {
                // A background process that reads the terminal is stopped by `SIGTTIN`.
                Stdio::null()
            })
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let mut pty = match &output {
            Some(_) => {
                let pty = Pty::open()?;
                command
                    .stdout(Stdio::from(pty.slave()?))
                    .stderr(Stdio::from(pty.slave()?));
                Some(pty)
            }
            None => None,
        };
        let foreground = configure(&mut command, interactive);
        let mut child = command.spawn().with_buck_error_context(|| {
            format!("Failed to start the `buck2 dev` server `{program}`")
        })?;
        let group = match Group::new(&child) {
            Ok(group) => group,
            Err(e) => {
                let _ignored = child.start_kill();
                return Err(e);
            }
        };
        let terminal = if foreground {
            Terminal::give(&group)
        } else {
            None
        };
        // `command` holds copies of the server's end of the pseudo-terminal.
        drop(command);
        if let (Some(pty), Some(output)) = (&mut pty, output) {
            pty.read(output);
        }
        let (tx, exit) = watch::channel(None);
        tokio::spawn(async move {
            let status = match child.wait().await {
                Ok(status) => status,
                Err(e) => {
                    // Not expected. Say that it failed, so that `buck2 dev` does not wait for it.
                    tracing::warn!("Failed to wait for the `buck2 dev` server: {e}");
                    failed_status()
                }
            };
            let _ignored = tx.send(Some(status));
        });
        Ok(ServerProcess {
            exit,
            group,
            terminal,
            paused: false,
            pty,
        })
    }

    /// Wait for the server to exit. Its group may still have processes in it.
    pub(super) async fn wait(&mut self) -> ExitStatus {
        match self.exit.wait_for(|status| status.is_some()).await {
            Ok(status) => status.expect("waited for it"),
            // The task that waits for the server is gone; it never will.
            Err(_) => futures::future::pending().await,
        }
    }

    fn exited(&self) -> bool {
        self.exit.borrow().is_some()
    }

    /// Wait until the server and every process in its group have exited, for up to `timeout`.
    async fn wait_group(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.exited() && !self.group.alive() {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Stop the server and the processes it started. If it was sent `shutdown`, give it up to
    /// `timeout` to exit. Then send the group `SIGTERM`, wait up to `timeout`, and kill what is
    /// left. Take the terminal back.
    pub(super) async fn stop(&mut self, sent_shutdown: bool, timeout: Duration) {
        if self.paused {
            // A stopped process neither reads `shutdown` nor acts on `SIGTERM`.
            self.group.signal(libc::SIGCONT);
            self.paused = false;
        }
        if sent_shutdown {
            let _ignored = tokio::time::timeout(timeout, self.wait()).await;
        }
        if !self.wait_group(Duration::ZERO).await {
            self.group.terminate();
            // In case a process of the group stopped itself.
            self.group.signal(libc::SIGCONT);
            if !self.wait_group(timeout).await {
                self.kill().await;
            }
        }
        self.take_terminal();
    }

    /// The terminal changed size.
    pub(super) fn resize(&self) {
        if let Some(pty) = &self.pty {
            pty.resize();
        }
    }

    /// Kill the server and its group, without waiting.
    pub(super) fn kill_now(&self) {
        self.group.kill();
    }

    /// Kill the server and its group now, and take the terminal back.
    pub(super) async fn kill(&mut self) {
        self.group.kill();
        self.wait().await;
        self.take_terminal();
    }

    /// Take the terminal back from an interactive server, once it has exited or is stopping.
    pub(super) fn take_terminal(&mut self) {
        if let Some(terminal) = self.terminal.take() {
            terminal.take_back();
        }
    }

    /// Whether the server has the terminal: it is the foreground group.
    pub(super) fn has_terminal(&self) -> bool {
        self.terminal
            .as_ref()
            .is_some_and(|terminal| terminal.given)
    }

    /// The signal that stopped the server, if it stopped since the last call. This does not
    /// reap the server: `waitid` without `WEXITED` reports only stops, and leaves its exit status
    /// for the task that waits for it.
    pub(super) fn stopped_by(&self) -> Option<libc::c_int> {
        // SAFETY: `info` is a plain C struct, written by `waitid`.
        unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            let ret = libc::waitid(
                libc::P_PID,
                self.group.pgid as libc::id_t,
                &mut info,
                libc::WSTOPPED | libc::WNOHANG,
            );
            let (pid, status) = stop_info(&info);
            (ret == 0 && pid != 0).then_some(status)
        }
    }

    /// Stop the server's group, if it is not stopped already, and take the terminal back from
    /// it, keeping its settings to give back.
    pub(super) fn pause(&mut self) {
        self.group.signal(libc::SIGSTOP);
        self.paused = true;
        if let Some(terminal) = &mut self.terminal {
            terminal.park();
        }
    }

    /// The server stopped when it read the terminal while `buck2 dev` was in the background.
    /// `resume` continues it once `buck2 dev` has the terminal again.
    pub(super) fn paused_for_terminal(&mut self) {
        self.paused = true;
    }

    /// `buck2 dev` was continued. If it is in the foreground, give the terminal back to a server
    /// that had it. Continue the server if `buck2 dev` stopped it, without the terminal if
    /// `buck2 dev` is in the background: a server that then reads it stops again until `fg`.
    pub(super) fn resume(&mut self) {
        if let Some(terminal) = &mut self.terminal {
            terminal.unpark(&self.group);
        }
        if self.paused {
            self.paused = false;
            self.group.signal(libc::SIGCONT);
        }
    }
}

#[cfg(target_os = "linux")]
fn stop_info(info: &libc::siginfo_t) -> (libc::pid_t, libc::c_int) {
    // SAFETY: `waitid` filled in the fields for a child.
    unsafe { (info.si_pid(), info.si_status()) }
}

#[cfg(not(target_os = "linux"))]
fn stop_info(info: &libc::siginfo_t) -> (libc::pid_t, libc::c_int) {
    (info.si_pid, info.si_status)
}

impl Drop for ServerProcess {
    /// Only reached without `stop` if `buck2 dev` fails unexpectedly. Ask the group to stop,
    /// without waiting.
    fn drop(&mut self) {
        if !self.exited() {
            self.group.terminate();
        }
        self.take_terminal();
    }
}

/// Put the server in a new process group. Returns whether to make that group the terminal's
/// foreground group: when the server is interactive, stdin is a terminal, and `buck2 dev`
/// is in the foreground, so that the terminal is its to give.
fn configure(command: &mut tokio::process::Command, interactive: bool) -> bool {
    command.process_group(0);
    let foreground = interactive && std::io::stdin().is_terminal() && in_foreground();
    // SAFETY: only async-signal-safe calls.
    unsafe {
        command.pre_exec(move || {
            if foreground {
                // As a shell does, the server takes the terminal itself as well as being given
                // it, so that it never reads the terminal before it has it: it would be stopped.
                // It is not yet in the foreground, so it ignores `SIGTTOU` while it takes it.
                // `process_group(0)` has already done this `setpgid`; repeat it in case.
                libc::setpgid(0, 0);
                libc::signal(libc::SIGTTOU, libc::SIG_IGN);
                libc::tcsetpgrp(libc::STDIN_FILENO, libc::getpid());
            }
            // `buck2 dev` ignores `SIGTTOU` while an interactive server has the terminal, and
            // `exec` keeps a signal ignored. The server starts with the default.
            libc::signal(libc::SIGTTOU, libc::SIG_DFL);
            Ok(())
        });
    }
    foreground
}

fn in_foreground() -> bool {
    // SAFETY: no memory is passed.
    unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) == libc::getpgrp() }
}

fn failed_status() -> ExitStatus {
    ExitStatus::from_raw(1 << 8)
}

struct Group {
    pgid: libc::pid_t,
}

impl Group {
    fn new(child: &tokio::process::Child) -> buck2_error::Result<Group> {
        let pid = child
            .id()
            .internal_error("The server has not been waited for")?;
        Ok(Group {
            pgid: pid as libc::pid_t,
        })
    }

    fn signal(&self, signal: libc::c_int) -> bool {
        // SAFETY: no memory is passed. The group's ID cannot be reused while a process of
        // `buck2 dev` has it, and the server is waited for promptly.
        unsafe { libc::killpg(self.pgid, signal) == 0 }
    }

    /// Whether any process is still in the group.
    fn alive(&self) -> bool {
        self.signal(0)
    }

    fn terminate(&self) {
        self.signal(libc::SIGTERM);
    }

    fn kill(&self) {
        self.signal(libc::SIGKILL);
    }
}

/// The terminal, while an interactive server has it or `buck2 dev` keeps it for the server
/// over Ctrl-Z.
struct Terminal {
    /// `buck2 dev`'s group.
    pgrp: libc::pid_t,
    /// The settings before the server got the terminal.
    ours: Option<libc::termios>,
    /// The server's settings, while `buck2 dev` has taken the terminal back over Ctrl-Z.
    server: Option<libc::termios>,
    /// Whether the server's group is the foreground group.
    given: bool,
    /// `buck2 dev`'s `SIGTTOU` disposition before. While the server has the terminal, `buck2 dev`
    /// is in the background, where changing the terminal, or writing to it with `stty tostop`,
    /// would stop it with `SIGTTOU`. So it ignores the signal.
    sigttou: libc::sighandler_t,
}

fn get_termios() -> Option<libc::termios> {
    // SAFETY: `termios` is a plain C struct, written by `tcgetattr`.
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        (libc::tcgetattr(libc::STDIN_FILENO, &mut termios) == 0).then_some(termios)
    }
}

fn set_termios(termios: &Option<libc::termios>) {
    if let Some(termios) = termios {
        // SAFETY: `termios` was written by `tcgetattr`.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSADRAIN, termios);
        }
    }
}

fn set_foreground(pgrp: libc::pid_t) {
    // SAFETY: no memory is passed.
    unsafe {
        libc::tcsetpgrp(libc::STDIN_FILENO, pgrp);
    }
}

impl Terminal {
    fn give(group: &Group) -> Option<Terminal> {
        let ours = get_termios();
        // SAFETY: no memory is passed.
        let (pgrp, sigttou) =
            unsafe { (libc::getpgrp(), libc::signal(libc::SIGTTOU, libc::SIG_IGN)) };
        set_foreground(group.pgid);
        Some(Terminal {
            pgrp,
            ours,
            server: None,
            given: true,
            sigttou,
        })
    }

    /// Take the terminal back for `buck2 dev` over Ctrl-Z, keeping the server's settings.
    fn park(&mut self) {
        if self.given {
            self.server = get_termios();
            set_foreground(self.pgrp);
            set_termios(&self.ours);
            self.given = false;
        }
    }

    /// Give the terminal back to the server after Ctrl-Z, if `buck2 dev` is in the foreground
    /// and so has it to give.
    fn unpark(&mut self, group: &Group) {
        if !self.given && in_foreground() {
            // The shell may have changed the settings while `buck2 dev` was stopped.
            self.ours = get_termios();
            set_termios(&self.server);
            set_foreground(group.pgid);
            self.given = true;
        }
    }

    /// Make `buck2 dev`'s group the foreground group again, and undo any change the server
    /// made to the terminal's settings, such as raw mode, in case it exited without undoing
    /// it.
    fn take_back(self) {
        if self.given {
            set_foreground(self.pgrp);
        }
        set_termios(&self.ours);
        // SAFETY: restores the disposition from before.
        unsafe {
            libc::signal(libc::SIGTTOU, self.sigttou);
        }
    }
}
