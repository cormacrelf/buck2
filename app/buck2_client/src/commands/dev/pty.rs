/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! The server's output, when the superconsole shows it.
//!
//! The server's stdout and stderr are one pseudo-terminal, so that the server still writes to a
//! terminal: it keeps its colours, and line buffering, where a pipe would make many programs
//! buffer their output in blocks. The pseudo-terminal is not the server's controlling terminal.
//! `buck2 dev` reads it, and sends each line to the console, which shows it above its canvas.

use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::time::Duration;

use buck2_client_ctx::subscribers::subscriber::SessionUpdate;
use buck2_error::BuckErrorContext;
use buck2_error::BuckErrorOptionContext;
use tokio::sync::mpsc;

/// How long `buck2 dev` waits for the rest of a line before it shows the part it has, such as a
/// prompt.
const PARTIAL_LINE_DELAY: Duration = Duration::from_millis(300);

/// The pseudo-terminal for one server.
pub(super) struct Pty {
    master: OwnedFd,
    /// The end the server writes to. `buck2 dev` closes its copy once the server has started,
    /// so that reading ends when the server and what it started have closed theirs.
    slave: Option<OwnedFd>,
}

impl Pty {
    pub(super) fn open() -> buck2_error::Result<Pty> {
        let pty = nix::pty::openpty(terminal_size().as_ref(), None)
            .buck_error_context("Failed to open a pseudo-terminal for the `buck2 dev` server")?;
        Ok(Pty {
            master: pty.master,
            slave: Some(pty.slave),
        })
    }

    /// The server's stdout or stderr.
    pub(super) fn slave(&self) -> buck2_error::Result<OwnedFd> {
        Ok(self
            .slave
            .as_ref()
            .internal_error("The server has started")?
            .try_clone()?)
    }

    /// The server has started: read what it writes, and send it to the console as lines.
    pub(super) fn read(&mut self, output: mpsc::UnboundedSender<SessionUpdate>) {
        self.slave = None;
        let Ok(master) = self.master.try_clone() else {
            return;
        };
        let (chunks_tx, chunks) = mpsc::unbounded_channel();
        // A thread, because the read blocks. It ends when every copy of the server's end is
        // closed, when reading fails with `EIO`.
        std::thread::spawn(move || {
            let mut master = std::fs::File::from(master);
            let mut buf = vec![0; 16 << 10];
            loop {
                match master.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if chunks_tx.send(buf[..n].to_vec()).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        tokio::spawn(lines(chunks, output));
    }

    /// The terminal changed size: so does the server's.
    pub(super) fn resize(&self) {
        if let Some(size) = terminal_size() {
            // SAFETY: `size` is a `winsize`, as `TIOCSWINSZ` expects.
            unsafe {
                libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size);
            }
        }
    }
}

/// The size of the terminal that `buck2 dev` draws on, its stderr.
fn terminal_size() -> Option<libc::winsize> {
    // SAFETY: `size` is a `winsize`, which `TIOCGWINSZ` writes.
    unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        (libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut size) == 0).then_some(size)
    }
}

/// Split what the server writes into lines, and send them to the console.
async fn lines(
    mut chunks: mpsc::UnboundedReceiver<Vec<u8>>,
    output: mpsc::UnboundedSender<SessionUpdate>,
) {
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let chunk = if pending.is_empty() {
            chunks.recv().await
        } else {
            match tokio::time::timeout(PARTIAL_LINE_DELAY, chunks.recv()).await {
                Ok(chunk) => chunk,
                Err(_) => {
                    // No more for a while, such as after a prompt: show what there is.
                    let line = finish_line(&std::mem::take(&mut pending));
                    if output.send(SessionUpdate::Output(vec![line])).is_err() {
                        return;
                    }
                    continue;
                }
            }
        };
        let Some(chunk) = chunk else {
            if !pending.is_empty() {
                let _ignored = output.send(SessionUpdate::Output(vec![finish_line(&pending)]));
            }
            return;
        };
        pending.extend(chunk);
        let mut lines = Vec::new();
        while let Some(end) = pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            lines.push(finish_line(&line[..end]));
        }
        if !lines.is_empty() && output.send(SessionUpdate::Output(lines)).is_err() {
            return;
        }
    }
}

/// The text of a line as a terminal would show it in the end. Colour escapes stay, for the console
/// to draw; it drops other control sequences.
fn finish_line(bytes: &[u8]) -> String {
    // The terminal turns `\n` into `\r\n`.
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    // A line that was rewritten with `\r`, such as a progress bar, shows its last text.
    let bytes = match bytes.iter().rposition(|b| *b == b'\r') {
        Some(start) => &bytes[start + 1..],
        None => bytes,
    };
    let text = String::from_utf8_lossy(bytes);
    if !text.contains('\t') {
        return text.into_owned();
    }
    // The console drops control characters, so expand tabs. Escape sequences count towards the
    // column, which only matters for a tab after a colour change.
    let mut line = String::with_capacity(text.len() + 8);
    let mut column = 0;
    for c in text.chars() {
        if c == '\t' {
            let spaces = 8 - column % 8;
            line.extend(std::iter::repeat_n(' ', spaces));
            column += spaces;
        } else {
            line.push(c);
            column += 1;
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::finish_line;

    #[test]
    fn test_finish_line() {
        assert_eq!(finish_line(b"plain\r"), "plain");
        assert_eq!(finish_line(b"10%\r50%\r100%\r"), "100%");
        assert_eq!(finish_line(b"\x1b[31mred\x1b[0m"), "\x1b[31mred\x1b[0m");
        assert_eq!(finish_line(b"a\tb"), "a       b");
    }
}
