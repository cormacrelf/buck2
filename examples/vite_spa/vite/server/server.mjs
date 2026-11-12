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
//              [--plugins-dir DIR --plugin FILE:EXPORT[:JSON]...]
//
// `--plugin` names the app's own Vite plugins whose server hooks run here: `configResolved`,
// `configureServer`, `transformIndexHtml` and `hotUpdate`. Their build hooks run in buck2's
// actions, if at all; see vite/tools/transform.mjs.

import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import { parseArgs } from "node:util";
import { createServer } from "vite";
import { staticImports } from "@vitebuck/tools/imports.mjs";
import { loadPlugins } from "@vitebuck/tools/plugins.mjs";

const { values: args, positionals } = parseArgs({
  options: {
    app: { type: "string" },
    public: { type: "string" },
    npm: { type: "string", default: "root//:esm" },
    port: { type: "string", default: process.env.PORT ?? "5173" },
    "plugins-dir": { type: "string" },
    plugin: { type: "string", multiple: true, default: [] },
  },
  allowPositionals: true,
});
const root = process.cwd();
// The React refresh runtime of @vitejs/plugin-react, beside its `dist/index.js`.
const refreshRuntime = fs.readFileSync(new URL("refresh-runtime.js", import.meta.resolve("@vitejs/plugin-react")), "utf8");
const JS_EXTS = [".ts", ".tsx", ".mts", ".js", ".jsx", ".mjs"];
const IMAGE_TYPES = {
  avif: "image/avif", webp: "image/webp", jpg: "image/jpeg", jpeg: "image/jpeg", png: "image/png",
  gif: "image/gif", tiff: "image/tiff",
};
const log = (...a) => process.env.BUCK2_VITE_DEBUG && console.error(`[buck ${performance.now().toFixed(0)}]`, ...a);

// --- buck2 dev ----------------------------------------------------------------------------

class Buck {
  constructor() {
    this.waiters = new Map(); // output name -> [{resolve, reject}]
    this.defined = new Set(); // the dynamic outputs that this server defined
    this.definitions = new Map(); // output name -> handler call, to define with the next build
    this.pending = new Set(); // output names to ask for with the next build
    this.building = new Set(); // output names asked for and not yet answered
    this.results = new Map(); // output name -> its last `built`, until it is stale
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
  // as a call of a handler. What is asked for in one tick goes in one `build`. An output that
  // is built and not stale since is not asked for again, nor one that is building.
  get(name, def) {
    const done = this.results.get(name);
    if (done) return Promise.resolve(done);
    return new Promise((resolve, reject) => {
      if (!this.waiters.has(name)) this.waiters.set(name, []);
      this.waiters.get(name).push({ resolve, reject });
      if (def && !this.defined.has(name)) {
        this.defined.add(name);
        this.definitions.set(name, def);
      }
      if (this.building.has(name)) return;
      this.building.add(name);
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
        this.building.delete(m.output);
        this.results.set(m.output, m);
        this.settle(m.output, (w) => w.resolve(m));
        break;
      case "failed":
        // Defined again with the next request for it, in case its definition was what failed.
        this.building.delete(m.output);
        this.defined.delete(m.output);
        this.settle(m.output, (w) => w.reject(new Error(m.error)));
        break;
      case "cancelled":
        // A file changed during the build, and `stale` follows. Ask again if it is awaited.
        this.building.delete(m.output);
        if (this.waiters.has(m.output)) {
          this.building.add(m.output);
          this.pending.add(m.output);
          this.flush();
        }
        break;
      case "stale":
        for (const name of Object.keys(m.outputs)) this.results.delete(name);
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

// --- Building imports before the browser asks for them ------------------------------------
//
// The browser finds a page's modules one level of imports at a time, and each level would wait
// for a round trip through Vite and buck2. So once a module is built, its static imports are
// built too, and theirs once they are, as Vite's `preTransformRequests` does: by the time the
// browser asks for a module, it is usually built. The imports of a compiled module are already
// the URLs that serve them; those of an npm bundle are relative to it, or other bundles.
//
// `import()`s are left for the browser to ask for. They are lazy routes, locale messages and
// the like, which a page may never load, and on a first build they would take the daemon's
// time from the modules that the page does need.

const prefetched = new Set(); // the URLs whose imports have been built, or are building

function urlOutput(url) {
  if (url.startsWith("/@npm/")) {
    if (!url.endsWith(".js")) return null;
    const [, , entry, ...rest] = url.split("/");
    return [npmOutput(entry), (dir) => path.join(dir, ...rest)];
  }
  if (!url.startsWith("/") || url.startsWith("/@")) return null;
  return [moduleOutput(urlKey(url)), (file) => file];
}

function prefetch(url) {
  if (prefetched.has(url)) return;
  const output = urlOutput(url);
  if (!output) return;
  prefetched.add(url);
  const [[name, def], file] = output;
  buck.get(name, def).then(async (built) => {
    const code = await fs.promises.readFile(file(built.path), "utf8");
    for (const spec of staticImports(code)) {
      prefetch(spec.startsWith(".") ? path.posix.join(path.posix.dirname(url), spec) : spec);
    }
  }).catch(() => {
    // The browser gets the error when it asks for the module.
  });
}

// --- The Vite plugin ----------------------------------------------------------------------

let server;
let indexPath;
const stale = new Set(); // module output names with changes not yet built
const ours = new Set(); // the module ids this plugin resolved, which buck2 builds
const idsByFile = new Map(); // source file -> the ids of its modules, one per query

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
        prefetch(id);
        const [, , entry, ...rest] = id.split("/");
        const dir = await readOutput(npmOutput(entry));
        return fs.readFileSync(path.join(dir, ...rest), "utf8");
      }
      if (!ours.has(id)) return null;
      prefetch(id.slice(root.length));
      const key = urlKey(id.slice(root.length));
      const file = path.join(root, key.split("~")[0]);
      if (!idsByFile.has(file)) idsByFile.set(file, new Set());
      idsByFile.get(file).add(id);
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
      // The images that image modules name, `/@image/<path>/<width>w[-q<quality>].<version>.<format>`
      // (see `imageUrl` in vite/tools/transform.mjs), each built when the browser first asks
      // for it. The version is the source's digest: it makes a new image a new URL, and the
      // output is always built from the source as it is.
      s.middlewares.use(async (req, res, next) => {
        const m = /^\/@image\/(.+)\/(\d+)w(?:-q(\d+))?\.[0-9a-f]+\.(\w+)$/.exec(req.url.split("?")[0]);
        if (!m) return next();
        const [, p, width, quality, format] = m;
        const file = decodeURIComponent(p);
        const args = ["--path", file, "--width", width, "--format", format, ...(quality ? ["--quality", quality] : [])];
        try {
          const out = await readOutput([`img:${file}/${width}w${quality ? "-q" + quality : ""}.${format}`, { handler: "image", args }]);
          res.setHeader("Content-Type", IMAGE_TYPES[format] ?? "application/octet-stream");
          res.setHeader("Cache-Control", "no-cache");
          fs.createReadStream(out).pipe(res);
        } catch (e) {
          next(e);
        }
      });
      // Files of npm packages that stylesheets name, such as fonts.
      s.middlewares.use(async (req, res, next) => {
        const url = req.url.split("?")[0];
        if (!url.startsWith("/@npm/") || url.endsWith(".js")) return next();
        try {
          const [, , entry, ...rest] = url.split("/");
          const dir = await readOutput(npmOutput(entry));
          const file = path.join(dir, ...rest.map(decodeURIComponent));
          if (!fs.existsSync(file)) return next();
          const types = { ".woff2": "font/woff2", ".woff": "font/woff", ".ttf": "font/ttf", ".svg": "image/svg+xml", ".png": "image/png", ".css": "text/css" };
          res.setHeader("Content-Type", types[extname(file)] ?? "application/octet-stream");
          res.setHeader("Cache-Control", "no-cache");
          fs.createReadStream(file).pipe(res);
        } catch (e) {
          next(e);
        }
      });
      return () => {
        // The SPA's page, for `/` and any route without a file extension.
        s.middlewares.use(async (req, res, next) => {
          const url = req.url.split("?")[0];
          if (req.method !== "GET" || (url !== "/" && extname(url) !== "" && !url.endsWith(".html"))) return next();
          try {
            if (!indexPath) indexPath = await readOutput(["index", null]);
            const source = fs.readFileSync(indexPath, "utf8");
            for (const [, src] of source.matchAll(/<script\b[^>]*\bsrc="([^"]+)"/g)) prefetch(src);
            const html = await s.transformIndexHtml(url, source, req.originalUrl);
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
  const env = server.environments.client;
  const changed = new Map(); // source file -> the Vite modules of it that changed
  await Promise.all(modules.map(async (name) => {
    stale.add(name);
    let out;
    try {
      out = await buck.get(name);
    } catch (e) {
      env.hot.send({ type: "error", err: { message: e.message, stack: "", plugin: "buck2-dev", id: name.slice(2) } });
      return;
    }
    stale.delete(name);
    if (!out.changed) return;
    const key = name.slice(2);
    const url = "/" + key + (key.includes("~") || !JS_EXTS.includes(extname(key)) ? ".js" : "");
    const mod = env.moduleGraph.getModuleById(root + url);
    if (!mod) return;
    const file = path.join(root, key.split("~")[0]);
    if (!changed.has(file)) changed.set(file, []);
    changed.get(file).push(mod);
  }));
  for (const [file, mods] of changed) {
    for (const mod of await runHotUpdate(file, mods)) env.reloadModule(mod);
  }
};

// The `hotUpdate` hooks of the app's plugins, which Vite would run for a changed file: each may
// handle the change itself, such as by sending a custom event, and say which modules still
// need an update. Plugins know modules by Vite's IDs, `file?query`, so they see them that way.
async function runHotUpdate(file, modules) {
  const hooks = serverPlugins.filter((p) => p.hotUpdate);
  if (!hooks.length) return modules;
  const env = server.environments.client;
  const real = new WeakMap();
  const asVite = (mod) => {
    const [p, q] = urlKey(mod.id.slice(root.length)).split("~");
    const proxy = new Proxy(mod, { get: (t, k) => (k === "id" ? path.join(root, p) + (q ? "?" + q : "") : Reflect.get(t, k)) });
    real.set(proxy, mod);
    return proxy;
  };
  let current = modules.map(asVite);
  const context = { environment: env };
  for (const p of hooks) {
    const handler = typeof p.hotUpdate === "function" ? p.hotUpdate : p.hotUpdate.handler;
    const result = await handler.call(context, {
      type: "update",
      file,
      timestamp: Date.now(),
      modules: current,
      read: () => fs.promises.readFile(file, "utf8"),
      server,
    });
    if (Array.isArray(result)) current = result;
  }
  return current.map((m) => real.get(m) ?? m);
}

// --- The app's plugins --------------------------------------------------------------------

// The app's plugins see the app's directory as Vite's root, as they would under the app's own
// Vite config, and its URL as the base. This server's root is the project.
const appRoot = path.join(root, args.app);
const serverPlugins = args.plugin.length ? await loadPlugins(args["plugins-dir"], args.plugin) : [];
function serverHooks(p) {
  const hook = (h) => (typeof h === "function" ? h : h?.handler);
  const wrapped = { name: p.name, enforce: p.enforce };
  if (p.configResolved) {
    wrapped.configResolved = function (config) {
      return hook(p.configResolved).call(this, Object.create(config, {
        root: { value: appRoot },
        base: { value: "/" + args.app + "/" },
      }));
    };
  }
  if (p.configureServer) wrapped.configureServer = p.configureServer;
  if (p.transformIndexHtml) wrapped.transformIndexHtml = p.transformIndexHtml;
  return wrapped;
}

// --- Start --------------------------------------------------------------------------------

server = await createServer({
  configFile: false,
  root,
  publicDir: args.public ? path.join(root, args.public) : false,
  envDir: path.join(root, args.app),
  appType: "custom",
  plugins: [buckPlugin(), ...serverPlugins.map(serverHooks)],
  oxc: false,
  optimizeDeps: { noDiscovery: true, include: [] },
  server: { port: Number(args.port), strictPort: true, watch: null, ...(positionals.includes("--host") ? { host: true } : {}) },
  clearScreen: false,
  logLevel: "info",
});
await server.listen();
server.printUrls();
