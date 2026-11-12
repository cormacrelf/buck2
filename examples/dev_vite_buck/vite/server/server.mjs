// The Vite dev server, with buck2 doing the building.
//
// Vite serves the page, runs its HMR client and module graph, and rewrites imports for HMR
// (`?t=`, `import.meta.hot`, `import.meta.env`). It transforms nothing and watches nothing:
// every module comes from `buck2 dev`, which builds it with an action of its own, already
// compiled and with its imports resolved to URLs, and says when one is stale. A stale module is
// built again, and if it changed, Vite sends the browser an HMR update for it. That works for
// any change buck2 knows about, whether to a source file, a generated one, a package compiled
// by another tool, or the BUCK files that decide how they build.
//
//   server.mjs --app apps/web [--public apps/web/public] [--npm root//:esm] [--port 5173]

import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import { parseArgs } from "node:util";
import { createServer } from "vite";

const { values: args, positionals } = parseArgs({
  options: {
    app: { type: "string" },
    public: { type: "string" },
    npm: { type: "string", default: "root//:esm" },
    port: { type: "string", default: process.env.PORT ?? "5173" },
  },
  allowPositionals: true,
});
const root = process.cwd();
// The React refresh runtime of @vitejs/plugin-react, beside its `dist/index.js`.
const refreshRuntime = fs.readFileSync(new URL("refresh-runtime.js", import.meta.resolve("@vitejs/plugin-react")), "utf8");
const JS_EXTS = [".ts", ".tsx", ".mts", ".js", ".jsx", ".mjs"];
const log = (...a) => process.env.BUCK2_VITE_DEBUG && console.error(`[buck ${performance.now().toFixed(0)}]`, ...a);

// --- buck2 dev ----------------------------------------------------------------------------

class Buck {
  constructor() {
    this.waiters = new Map(); // output name -> [{resolve, reject}]
    this.defined = new Set(); // the dynamic outputs that this server defined
    this.definitions = new Map(); // output name -> handler call, to define with the next build
    this.pending = new Set(); // output names to ask for with the next build
    this.handlers = { stale: () => {} };
    this.ready = new Promise((r) => (this.onReady = r));
    this.sock = net.connect(process.env.BUCK2_DEV_SOCKET);
    let buf = "";
    this.sock.on("data", (d) => {
      buf += d;
      let i;
      while ((i = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, i);
        buf = buf.slice(i + 1);
        if (line) this.message(JSON.parse(line));
      }
    });
    this.sock.on("close", () => process.exit(0));
    this.send({ type: "hello", version: 2 });
  }
  send(m) {
    this.sock.write(JSON.stringify(m) + "\n");
  }
  // The output `name`, built: `{path, digest, changed, ...}`. `def` defines it the first time,
  // as a call of a handler. What is asked for in one tick goes in one `build`.
  get(name, def) {
    return new Promise((resolve, reject) => {
      if (!this.waiters.has(name)) this.waiters.set(name, []);
      this.waiters.get(name).push({ resolve, reject });
      if (def && !this.defined.has(name)) {
        this.defined.add(name);
        this.definitions.set(name, def);
      }
      this.pending.add(name);
      if (this.pending.size === 1) setImmediate(() => this.flush());
    });
  }
  flush() {
    if (this.definitions.size) {
      this.send({ type: "define", outputs: Object.fromEntries(this.definitions) });
      this.definitions.clear();
    }
    if (!this.pending.size) return;
    log("build", this.pending.size, "outputs");
    this.send({ type: "build", outputs: [...this.pending] });
    this.pending.clear();
  }
  settle(name, f) {
    const ws = this.waiters.get(name);
    this.waiters.delete(name);
    for (const w of ws ?? []) f(w);
  }
  message(m) {
    switch (m.type) {
      case "ready":
        this.onReady(m);
        break;
      case "built":
        log("built", m.output, m.changed ? "changed" : "same");
        this.settle(m.output, (w) => w.resolve(m));
        break;
      case "failed":
        // Defined again with the next request for it, in case its definition was what failed.
        this.defined.delete(m.output);
        this.settle(m.output, (w) => w.reject(new Error(m.error)));
        break;
      case "cancelled":
        // A file changed during the build, and `stale` follows. Ask again if it is awaited.
        if (this.waiters.has(m.output)) {
          this.pending.add(m.output);
          this.flush();
        }
        break;
      case "stale":
        this.handlers.stale(m.outputs);
        break;
    }
  }
}

const buck = new Buck();
const ready = await buck.ready;
log("ready", ready);

// --- Module keys and URLs (the same scheme as vite/tools/transform.mjs) --------------------

function extname(p) {
  const base = p.slice(p.lastIndexOf("/") + 1);
  const i = base.lastIndexOf(".");
  return i <= 0 ? "" : base.slice(i);
}
// `/apps/web/src/a.module.css.js` -> `apps/web/src/a.module.css`.
function urlKey(url) {
  let key = url.slice(1);
  if (key.endsWith(".js")) {
    const inner = key.slice(0, -3);
    const [p, q] = inner.split("~");
    if (q !== undefined || !JS_EXTS.includes(extname(p)) && extname(p) !== "") key = inner;
  }
  return key;
}
const moduleOutput = (key) => ["m:" + key, { handler: "module", args: ["--key", key] }];
const npmOutput = (entry) => ["npm:" + entry, { handler: "npm", args: ["--entry", entry, "--target", args.npm] }];

async function readOutput([name, def]) {
  const out = await buck.get(name, def);
  return out.path;
}

// --- The Vite plugin ----------------------------------------------------------------------

let server;
let indexPath;
const stale = new Set(); // module output names with changes not yet built
const ours = new Set(); // the module ids this plugin resolved, which buck2 builds

function buckPlugin() {
  return {
    name: "buck2-dev",
    enforce: "pre",
    resolveId(source, importer) {
      if (source === "/@react-refresh") return source;
      if (source.startsWith("/@npm/")) return source;
      if (importer?.startsWith("/@npm/") && source.startsWith(".")) {
        return path.posix.join(path.posix.dirname(importer), source);
      }
      if (source.startsWith("/") && !source.startsWith("/@") && !source.startsWith(root + "/")) {
        ours.add(root + source);
        return root + source;
      }
      if (ours.has(source)) return source;
      return null;
    },
    async load(id) {
      if (id === "/@react-refresh") return refreshRuntime;
      if (id.startsWith("/@npm/")) {
        const [, , entry, ...rest] = id.split("/");
        const dir = await readOutput(npmOutput(entry));
        return fs.readFileSync(path.join(dir, ...rest), "utf8");
      }
      if (!ours.has(id)) return null;
      const key = urlKey(id.slice(root.length));
      try {
        const file = await readOutput(moduleOutput(key));
        return { code: fs.readFileSync(file, "utf8"), moduleType: "js" };
      } catch (e) {
        this.error(e.message);
      }
    },
    transformIndexHtml() {
      return [{
        tag: "script",
        attrs: { type: "module" },
        injectTo: "head-prepend",
        children: `import { injectIntoGlobalHook } from "/@react-refresh";
injectIntoGlobalHook(window);
window.$RefreshReg$ = () => {};
window.$RefreshSig$ = () => (type) => type;`,
      }];
    },
    configureServer(s) {
      return () => {
        // The SPA's page, for `/` and any route without a file extension.
        s.middlewares.use(async (req, res, next) => {
          const url = req.url.split("?")[0];
          if (req.method !== "GET" || (url !== "/" && extname(url) !== "" && !url.endsWith(".html"))) return next();
          try {
            if (!indexPath) indexPath = await readOutput(["index", null]);
            const html = await s.transformIndexHtml(url, fs.readFileSync(indexPath, "utf8"), req.originalUrl);
            res.setHeader("Content-Type", "text/html");
            res.end(html);
          } catch (e) {
            next(e);
          }
        });
      };
    },
  };
}

// --- Changes ------------------------------------------------------------------------------

buck.handlers.stale = async (outputs) => {
  const names = Object.keys(outputs);
  log("stale", names);
  if (names.includes("index") || names.some((n) => n.startsWith("npm:"))) {
    indexPath = undefined;
    // Rebuild before reloading, so that the page does not wait on a build it cannot see.
    await Promise.allSettled(names.filter((n) => n === "index" || n.startsWith("npm:")).map((n) => buck.get(n)));
    server.environments.client.hot.send({ type: "full-reload" });
  }
  const modules = names.filter((n) => n.startsWith("m:"));
  const graph = server.environments.client.moduleGraph;
  await Promise.all(modules.map(async (name) => {
    stale.add(name);
    let out;
    try {
      out = await buck.get(name);
    } catch (e) {
      server.environments.client.hot.send({
        type: "error",
        err: { message: e.message, stack: "", plugin: "buck2-dev", id: name.slice(2) },
      });
      return;
    }
    stale.delete(name);
    if (!out.changed) return;
    const key = name.slice(2);
    const url = "/" + key + (key.includes("~") || !JS_EXTS.includes(extname(key)) ? ".js" : "");
    const mod = graph.getModuleById(root + url);
    if (mod) server.environments.client.reloadModule(mod);
  }));
};

// --- Start --------------------------------------------------------------------------------

server = await createServer({
  configFile: false,
  root,
  publicDir: args.public ? path.join(root, args.public) : false,
  envDir: path.join(root, args.app),
  appType: "custom",
  plugins: [buckPlugin()],
  oxc: false,
  optimizeDeps: { noDiscovery: true, include: [] },
  server: { port: Number(args.port), strictPort: true, watch: null, ...(positionals.includes("--host") ? { host: true } : {}) },
  clearScreen: false,
  logLevel: "info",
});
await server.listen();
server.printUrls();
