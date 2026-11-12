// A buck2 persistent worker that runs transform.mjs, so that oxc, lightningcss and Sass load once
// and stay warm instead of starting with every module.
//
// buck2 starts it with `WORKER_SOCKET` set, and sends each action's arguments to
// `worker.Worker/Execute`, a unary gRPC call over that Unix socket. The protocol is two small
// messages, so this speaks it with node's `http2` and a hand-written protobuf codec rather than
// a gRPC library:
//
//   message ExecuteCommand { repeated bytes argv = 1; repeated EnvironmentEntry env = 2; }
//   message ExecuteResponse { int32 exit_code = 1; string stderr = 2; }
//
// Without `WORKER_SOCKET` (an executor without persistent workers), it runs one action from its
// command line, as transform.mjs does.

import fs from "node:fs";
import http2 from "node:http2";
import { transform, TransformError } from "./transform.mjs";

async function run(argv) {
  try {
    await transform(argv);
    return { exitCode: 0, stderr: "" };
  } catch (e) {
    return { exitCode: 1, stderr: (e instanceof TransformError ? e.message : e.stack) + "\n" };
  }
}

// --- protobuf -----------------------------------------------------------------------------

function readVarint(buf, pos) {
  let result = 0;
  let shift = 0;
  for (;;) {
    const b = buf[pos++];
    result += (b & 0x7f) * 2 ** shift;
    if (b < 0x80) return [result, pos];
    shift += 7;
  }
}

function decodeArgv(buf) {
  const argv = [];
  let pos = 0;
  while (pos < buf.length) {
    let tag, len;
    [tag, pos] = readVarint(buf, pos);
    const field = tag >>> 3;
    const wire = tag & 7;
    if (wire === 2) {
      [len, pos] = readVarint(buf, pos);
      if (field === 1) argv.push(buf.subarray(pos, pos + len).toString("utf8"));
      pos += len;
    } else if (wire === 0) {
      [, pos] = readVarint(buf, pos);
    } else {
      throw new Error(`unexpected wire type ${wire}`);
    }
  }
  return argv;
}

function varint(n) {
  const out = [];
  while (n > 0x7f) {
    out.push((n & 0x7f) | 0x80);
    n = Math.floor(n / 128);
  }
  out.push(n);
  return Buffer.from(out);
}

function encodeResponse({ exitCode, stderr }) {
  const parts = [];
  if (exitCode) parts.push(Buffer.from([0x08]), varint(exitCode));
  if (stderr) {
    const s = Buffer.from(stderr, "utf8");
    parts.push(Buffer.from([0x12]), varint(s.length), s);
  }
  return Buffer.concat(parts);
}

// --- gRPC over HTTP/2 ---------------------------------------------------------------------

function frame(message) {
  const header = Buffer.alloc(5);
  header.writeUInt32BE(message.length, 1);
  return Buffer.concat([header, message]);
}

const socket = process.env.WORKER_SOCKET;
if (!socket) {
  const { exitCode, stderr } = await run(process.argv.slice(2));
  process.stderr.write(stderr);
  process.exit(exitCode);
}

fs.rmSync(socket, { force: true });
const server = http2.createServer();
server.on("stream", (stream, headers) => {
  if (headers[":path"] !== "/worker.Worker/Execute") {
    stream.respond({ ":status": 200, "content-type": "application/grpc", "grpc-status": "12" }, { endStream: true });
    return;
  }
  const chunks = [];
  stream.on("data", (c) => chunks.push(c));
  stream.on("end", async () => {
    const body = Buffer.concat(chunks);
    const response = await run(decodeArgv(body.subarray(5, 5 + body.readUInt32BE(1))));
    stream.respond({ ":status": 200, "content-type": "application/grpc" }, { waitForTrailers: true });
    stream.on("wantTrailers", () => stream.sendTrailers({ "grpc-status": "0" }));
    stream.end(frame(encodeResponse(response)));
  });
});
server.listen(socket);
