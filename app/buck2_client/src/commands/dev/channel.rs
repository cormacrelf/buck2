/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! The Unix socket that a `buck2 dev` server may connect to. `buck2 dev` names it in
//! `BUCK2_DEV_SOCKET`.
//!
//! Tasks read and write each connection, so `buck2 dev` never waits for the server: lines it
//! reads arrive as `ServerEvent`s, and lines to send go to a bounded queue.

use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use buck2_common::invocation_paths::InvocationPaths;
use buck2_error::BuckErrorContext;
use buck2_error::ErrorTag;
use buck2_error::buck2_error;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::AbortHandle;
use tokio::task::JoinHandle;

/// The environment variable with the address of the socket.
pub(super) const SOCKET_ENV: &str = "BUCK2_DEV_SOCKET";

/// What happened on a connection to the socket. Connections are numbered in the order they were
/// made.
pub(super) enum ServerEvent {
    Connected {
        conn: u64,
        connection: Connection,
    },
    Line {
        conn: u64,
        line: String,
    },
    /// The other end sent a line longer than `MAX_LINE`. The connection is no longer read.
    TooLong {
        conn: u64,
    },
    /// The other end closed the connection, or reading it failed.
    Closed {
        conn: u64,
    },
}

/// The longest line `buck2 dev` reads from the server. Requests are small; a longer line is a
/// server writing something else to the socket.
const MAX_LINE: usize = 4 << 20;

/// How many messages may wait to be written to a server. A server that falls this far behind has
/// stopped reading.
const MAX_QUEUED: usize = 1024;

/// One connection. Sending never waits: a task writes the lines. Dropping it closes the
/// connection.
pub(super) struct Connection {
    lines: mpsc::Sender<Vec<u8>>,
    reader: AbortHandle,
    writer: AbortHandle,
}

/// The server did not read the messages `buck2 dev` sent it.
pub(super) struct Full;

impl Connection {
    pub(super) fn send(&self, message: &serde_json::Value) -> Result<(), Full> {
        let Ok(mut line) = serde_json::to_vec(message) else {
            return Ok(());
        };
        line.push(b'\n');
        match self.lines.try_send(line) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(Full),
            // The connection is gone. That is noticed when it closes, or when the server exits.
            Err(mpsc::error::TrySendError::Closed(_)) => Ok(()),
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Dropping the tasks drops both halves of the socket, which closes it. Messages not yet
        // written are lost: the server is stopping, or another connection took over.
        self.reader.abort();
        self.writer.abort();
    }
}

/// The listening socket. Dropping it stops accepting connections and removes the socket.
pub(super) struct Listener {
    address: String,
    next_conn: Arc<AtomicU64>,
    accept: JoinHandle<()>,
    /// The private directory with the socket.
    dir: PathBuf,
}

impl Listener {
    /// Listen on a new socket, and send what happens on its connections to `events`.
    pub(super) fn bind(
        paths: &InvocationPaths,
        events: mpsc::UnboundedSender<ServerEvent>,
    ) -> buck2_error::Result<Listener> {
        bind_impl(paths, events)
    }

    /// The value of `BUCK2_DEV_SOCKET`.
    pub(super) fn address(&self) -> &str {
        &self.address
    }

    /// The number the next connection gets. Connections with smaller numbers were made before
    /// this call.
    pub(super) fn next_conn(&self) -> u64 {
        self.next_conn.load(Ordering::SeqCst)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.accept.abort();
        let _ignored = std::fs::remove_dir_all(&self.dir);
    }
}

/// A short random name, so that two `buck2 dev`s, or a `buck2 dev` and a socket that a killed one
/// left behind, never share an address.
fn unique_name() -> String {
    let random = uuid::Uuid::new_v4().simple().to_string();
    format!("{}-{}", std::process::id(), &random[..8])
}

/// Number a new connection, start the tasks that read and write it, and tell the session.
fn serve<R, W>(
    read: R,
    write: W,
    next_conn: &AtomicU64,
    events: &mpsc::UnboundedSender<ServerEvent>,
) where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let conn = next_conn.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = mpsc::channel(MAX_QUEUED);
    let writer = tokio::spawn(write_lines(write, rx)).abort_handle();
    // The reader waits until the session has `Connected`, so that it comes before the lines.
    let (start_tx, start_rx) = oneshot::channel();
    let reader = tokio::spawn({
        let events = events.clone();
        async move {
            if start_rx.await.is_ok() {
                read_lines(conn, read, events).await;
            }
        }
    })
    .abort_handle();
    let connection = Connection {
        lines: tx,
        reader,
        writer,
    };
    if events
        .send(ServerEvent::Connected { conn, connection })
        .is_ok()
    {
        let _ignored = start_tx.send(());
    }
}

async fn write_lines<W: AsyncWrite + Unpin>(mut write: W, mut lines: mpsc::Receiver<Vec<u8>>) {
    while let Some(line) = lines.recv().await {
        if write.write_all(&line).await.is_err() || write.flush().await.is_err() {
            return;
        }
    }
    let _ignored = write.shutdown().await;
}

async fn read_lines<R: AsyncRead + Unpin>(
    conn: u64,
    read: R,
    events: mpsc::UnboundedSender<ServerEvent>,
) {
    let mut read = BufReader::new(read);
    let mut line = Vec::new();
    loop {
        line.clear();
        // One more than the longest line, to tell a line of `MAX_LINE` bytes from a longer one.
        match (&mut read)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut line)
            .await
        {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        } else if line.len() > MAX_LINE {
            let _ignored = events.send(ServerEvent::TooLong { conn });
            return;
        }
        let line = String::from_utf8_lossy(&line).into_owned();
        if events.send(ServerEvent::Line { conn, line }).is_err() {
            return;
        }
    }
    let _ignored = events.send(ServerEvent::Closed { conn });
}

/// A Unix socket path must fit `sun_path`, which is 108 bytes on Linux and 104 on macOS,
/// with a terminating NUL.
const MAX_SOCKET_PATH: usize = 100;

/// The socket is `s` in a directory that only this user can enter, so that only this user
/// can connect. It is under `buck-out` if the path is short enough, and in the temporary
/// directory otherwise.
fn socket_dir(paths: &InvocationPaths, name: &str) -> PathBuf {
    let dir = paths.tmp_dir().as_path().join(format!("dev-{name}"));
    if dir.join("s").as_os_str().len() <= MAX_SOCKET_PATH {
        dir
    } else {
        std::env::temp_dir().join(format!("buck2-dev-{name}"))
    }
}

fn make_private_dir(dir: &Path) -> buck2_error::Result<()> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::DirBuilder::new().mode(0o700).create(dir)?;
    Ok(())
}

fn bind_impl(
    paths: &InvocationPaths,
    events: mpsc::UnboundedSender<ServerEvent>,
) -> buck2_error::Result<Listener> {
    let dir = socket_dir(paths, &unique_name());
    make_private_dir(&dir).with_buck_error_context(|| {
        format!(
            "Failed to create a directory for the `buck2 dev` socket: `{}`",
            dir.display()
        )
    })?;
    let path = dir.join("s");
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(e) => {
            let _ignored = std::fs::remove_dir_all(&dir);
            return Err(buck2_error!(
                ErrorTag::Environment,
                "Failed to listen on the `buck2 dev` socket `{}`: {e}",
                path.display()
            ));
        }
    };
    let Some(address) = path.to_str().map(str::to_owned) else {
        let _ignored = std::fs::remove_dir_all(&dir);
        return Err(buck2_error!(
            ErrorTag::Environment,
            "The `buck2 dev` socket path is not UTF-8: `{}`",
            path.display()
        ));
    };
    let next_conn = Arc::new(AtomicU64::new(0));
    let accept = tokio::spawn({
        let next_conn = next_conn.clone();
        async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let (read, write) = stream.into_split();
                        serve(read, write, &next_conn, &events);
                    }
                    Err(e) => {
                        // For example, too many open files. Try again later rather than give
                        // up on the server ever connecting.
                        tracing::warn!("`buck2 dev` failed to accept a connection: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
        }
    });
    Ok(Listener {
        address,
        next_conn,
        accept,
        dir,
    })
}
