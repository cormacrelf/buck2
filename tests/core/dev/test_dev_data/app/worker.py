# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# A persistent worker for `dev_app` with `worker` set. Each request has the arguments
# `OUT SRC...`, and the worker writes `MESSAGE` and then the sources to `OUT`. The tests edit
# `MESSAGE`, which a running worker does not see, as it read this file when it started. Each
# worker appends its pid to `buck-out/worker_pids.txt` when it starts.
#
# buck2 starts it with `WORKER_SOCKET` set and sends each action to `worker.Worker/Execute`, a
# unary gRPC call over that Unix socket. The test environment has no gRPC library, so this
# speaks just enough HTTP/2 and protobuf by hand. Without `WORKER_SOCKET` it runs one action
# from its command line.

import os
import socket
import struct
import sys
import threading
from typing import Dict, List, Tuple

MESSAGE = "worker v1"


def run(argv: List[str]) -> None:
    out, srcs = argv[0], argv[1:]
    parts = [MESSAGE + "\n"]
    for src in srcs:
        with open(src) as f:
            parts.append(f.read())
    with open(out, "w") as f:
        f.write("".join(parts))


# --- protobuf ---


def read_varint(buf: bytes, pos: int) -> Tuple[int, int]:
    result = shift = 0
    while True:
        b = buf[pos]
        pos += 1
        result |= (b & 0x7F) << shift
        if b < 0x80:
            return result, pos
        shift += 7


def decode_argv(buf: bytes) -> List[str]:
    argv = []
    pos = 0
    while pos < len(buf):
        tag, pos = read_varint(buf, pos)
        wire = tag & 7
        if wire == 2:
            n, pos = read_varint(buf, pos)
            if tag >> 3 == 1:
                argv.append(buf[pos : pos + n].decode())
            pos += n
        elif wire == 0:
            _, pos = read_varint(buf, pos)
        else:
            raise ValueError(f"unexpected wire type {wire}")
    return argv


def varint(n: int) -> bytes:
    out = bytearray()
    while n > 0x7F:
        out.append((n & 0x7F) | 0x80)
        n >>= 7
    out.append(n)
    return bytes(out)


def encode_response(exit_code: int, stderr: str) -> bytes:
    out = b""
    if exit_code:
        out += b"\x08" + varint(exit_code)
    if stderr:
        s = stderr.encode()
        out += b"\x12" + varint(len(s)) + s
    return out


# --- HTTP/2 ---

DATA, HEADERS, SETTINGS, PING = 0x0, 0x1, 0x4, 0x6
END_STREAM, ACK, END_HEADERS = 0x1, 0x1, 0x4


def frame(kind: int, flags: int, stream: int, payload: bytes = b"") -> bytes:
    return struct.pack(">I", len(payload))[1:] + struct.pack(">BBI", kind, flags, stream) + payload


def header(name: str, value: str) -> bytes:
    """An HPACK literal header field without indexing, with a new name."""
    return b"\x00" + bytes([len(name)]) + name.encode() + bytes([len(value)]) + value.encode()


def read_exact(conn: socket.socket, n: int) -> bytes:
    buf = b""
    while len(buf) < n:
        chunk = conn.recv(n - len(buf))
        if not chunk:
            raise EOFError
        buf += chunk
    return buf


def respond(conn: socket.socket, stream: int, body: bytes) -> None:
    try:
        run(decode_argv(body[5:]))
        response = encode_response(0, "")
    except Exception as e:
        response = encode_response(1, f"{e}\n")
    conn.sendall(
        # `:status: 200` is entry 8 of the HPACK static table.
        frame(HEADERS, END_HEADERS, stream, b"\x88" + header("content-type", "application/grpc"))
        + frame(DATA, 0, stream, b"\x00" + struct.pack(">I", len(response)) + response)
        + frame(HEADERS, END_HEADERS | END_STREAM, stream, header("grpc-status", "0"))
    )


def serve(conn: socket.socket) -> None:
    try:
        read_exact(conn, 24)  # The client connection preface.
        conn.sendall(frame(SETTINGS, 0, 0))
        bodies: Dict[int, bytes] = {}
        while True:
            head = read_exact(conn, 9)
            length = struct.unpack(">I", b"\x00" + head[:3])[0]
            kind, flags, stream = struct.unpack(">BBI", head[3:])
            stream &= 0x7FFFFFFF
            payload = read_exact(conn, length)
            if kind == SETTINGS and not flags & ACK:
                conn.sendall(frame(SETTINGS, ACK, 0))
            elif kind == PING and not flags & ACK:
                conn.sendall(frame(PING, ACK, 0, payload))
            elif kind == HEADERS:
                # Only `Execute` is called, so the request headers do not matter.
                bodies[stream] = b""
            elif kind == DATA:
                bodies[stream] = bodies.get(stream, b"") + payload
                if length:
                    # Give back the flow control window, for this stream and the connection.
                    update = struct.pack(">I", length)
                    conn.sendall(frame(0x8, 0, 0, update) + frame(0x8, 0, stream, update))
                if flags & END_STREAM:
                    respond(conn, stream, bodies.pop(stream))
    except EOFError:
        pass
    finally:
        conn.close()


def main() -> None:
    path = os.environ.get("WORKER_SOCKET")
    if not path:
        run(sys.argv[1:])
        return
    # The tests read this to see which workers ran. The file watcher does not report `buck-out`.
    with open("buck-out/worker_pids.txt", "a") as f:
        f.write(f"{os.getpid()}\n")
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(path)
    server.listen()
    while True:
        conn, _ = server.accept()
        threading.Thread(target=serve, args=(conn,), daemon=True).start()


if __name__ == "__main__":
    main()
