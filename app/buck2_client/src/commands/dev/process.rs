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

use std::io::IsTerminal;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::process::Stdio;
use std::time::Duration;

use buck2_error::BuckErrorContext;
use buck2_error::BuckErrorOptionContext;
use buck2_error::ErrorTag;
use buck2_error::buck2_error;
use tokio::sync::watch;

/// How often `buck2 dev` checks whether a stopping server and the processes it started have
/// exited.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(super) struct ServerProcess {
    /// The exit status, once the server has exited. A task waits for it.
    exit: watch::Receiver<Option<ExitStatus>>,
    group: Group,
    /// For an interactive server that has the terminal: how to take it back.
    terminal: Option<Terminal>,
}

impl ServerProcess {
    /// Start `argv` in `cwd`, with `BUCK2_DEV_SOCKET` set to `socket`.
    pub(super) fn spawn(
        argv: &[String],
        cwd: &str,
        interactive: bool,
        socket: &str,
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
        if sent_shutdown {
            let _ignored = tokio::time::timeout(timeout, self.wait()).await;
        }
        if !self.wait_group(Duration::ZERO).await {
            self.group.terminate();
            if !self.wait_group(timeout).await {
                self.kill().await;
            }
        }
        self.take_terminal();
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

/// The terminal's state before an interactive server got it.
struct Terminal {
    pgrp: libc::pid_t,
    termios: Option<libc::termios>,
    /// `buck2 dev`'s `SIGTTOU` disposition before. While the server has the terminal, `buck2 dev`
    /// is in the background, where changing the terminal, or writing to it with `stty tostop`,
    /// would stop it with `SIGTTOU`. So it ignores the signal.
    sigttou: libc::sighandler_t,
}

impl Terminal {
    fn give(group: &Group) -> Option<Terminal> {
        // SAFETY: `termios` is a plain C struct, written by `tcgetattr`. The rest pass no memory.
        unsafe {
            let mut termios: libc::termios = std::mem::zeroed();
            let termios =
                (libc::tcgetattr(libc::STDIN_FILENO, &mut termios) == 0).then_some(termios);
            let pgrp = libc::getpgrp();
            let sigttou = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
            libc::tcsetpgrp(libc::STDIN_FILENO, group.pgid);
            Some(Terminal {
                pgrp,
                termios,
                sigttou,
            })
        }
    }

    /// Make `buck2 dev`'s group the foreground group again, and undo any change the server
    /// made to the terminal's settings, such as raw mode, in case it exited without undoing
    /// it.
    fn take_back(self) {
        // SAFETY: `termios` was written by `tcgetattr`. The rest pass no memory.
        unsafe {
            libc::tcsetpgrp(libc::STDIN_FILENO, self.pgrp);
            if let Some(termios) = &self.termios {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSADRAIN, termios);
            }
            libc::signal(libc::SIGTTOU, self.sigttou);
        }
    }
}
