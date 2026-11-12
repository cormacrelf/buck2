// Compiles one module of a `vite_library` into the ES module that the browser loads, with every
// import already resolved to the URL the dev server serves it at.
//
//   transform.mjs --ctx CTX --path PATH [--query QUERY] --src SRC --out OUT
//                 [--assets DIR] [--plugins-dir DIR --plugin FILE:EXPORT[:JSON]...]
//
// PATH is the source's path in the project, SRC the file to read (the same file, or a generated
// one placed at PATH), and QUERY a Vite-style import query such as `raw`, `url`, `inline`,
// `worker`, or `w=400;800&format=webp&as=srcset`. Files that the module makes besides itself,
// such as resized images, go into DIR, which the dev server serves at `/DIR`.
//
// CTX is the library's configuration and resolution context, written by `vite_library`:
//
//   {"package": "apps/web", "files": [...own source paths...],
//    "aliases": {"@/": "apps/web/src/"},
//    "workspace": {"@demo/ui": {"package": "packages/ui", "exports": {".": "packages/ui/src/index.ts"}}},
//    "npm": {"react": "react@19.3.1", ...},              // alias -> npm store entry name
//    "virtual": {"virtual:build-info": "apps/web/@virtual/build-info.js"},
//    "define": {"__APP_VERSION__": "\"1.4.0\""},
//    "css": {"modules": {"pattern": "[name]__[local]__[hash]", "camelCaseOnly": true},
//            "scssAdditionalData": "@use \"@/styles/tokens\" as *;\n",
//            "browserslist": ["defaults"]},
//    "imagePresets": {"card": {"w": "360;720", "format": "avif;webp;jpg", "as": "picture"}}}
//
// A module's key is its path, with `~QUERY` when imported with a query. Its URL is `/` + key,
// with `.js` appended unless the key already ends in a JS extension, so that Vite's own
// CSS/JSON/asset plugins leave the compiled module alone. `/@npm/<entry>/<subpath>.js` is a
// pre-bundled npm package (see npm_esm.mjs).
//
// `--plugin` runs the build hooks (`resolveId`, `load`, `transform`) of a Vite plugin on the
// module, as Vite would, before compiling what they return as JavaScript. FILE is relative to
// the plugins directory, a staged package with the plugins' npm dependencies beside it.

import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { parseArgs } from "node:util";
import { transformSync } from "oxc-transform";
import { init, parse } from "es-module-lexer";
import { loadPlugins } from "./plugins.mjs";

export class TransformError extends Error {}

const JS_EXTS = [".ts", ".tsx", ".mts", ".js", ".jsx", ".mjs"];
const CSS_EXTS = [".css", ".scss", ".sass"];
const IMAGE_EXTS = [".jpg", ".jpeg", ".png", ".webp", ".avif", ".tiff", ".gif"];
const PROBE = [...JS_EXTS, ".json", ...JS_EXTS.map((e) => "/index" + e)];
const IMAGE_DIRECTIVES = ["w", "h", "format", "as", "preset", "quality"];
const MIME = {
  ".svg": "image/svg+xml", ".png": "image/png", ".jpg": "image/jpeg", ".jpeg": "image/jpeg",
  ".gif": "image/gif", ".webp": "image/webp", ".avif": "image/avif", ".ico": "image/x-icon",
  ".woff": "font/woff", ".woff2": "font/woff2", ".ttf": "font/ttf", ".txt": "text/plain",
  ".md": "text/markdown",
};

export function extname(p) {
  const base = p.slice(p.lastIndexOf("/") + 1);
  if (base.endsWith(".d.ts")) return ".d.ts";
  const i = base.lastIndexOf(".");
  return i <= 0 ? "" : base.slice(i);
}

export function keyUrl(key) {
  const [p, q] = key.split("~");
  return "/" + key + (q === undefined && JS_EXTS.includes(extname(p)) ? "" : ".js");
}

function splitQuery(s) {
  const i = s.indexOf("?");
  return i < 0 ? [s, ""] : [s.slice(0, i), s.slice(i + 1)];
}

const sortedEntries = (o) => Object.entries(o).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
const camelCase = (s) => s.replace(/-+(\w)/g, (_, c) => c.toUpperCase());

function globToRegExp(glob) {
  let re = "";
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i];
    if (c === "*" && glob[i + 1] === "*") { re += ".*"; i++; if (glob[i + 1] === "/") i++; }
    else if (c === "*") re += "[^/]*";
    else if (c === "{") { const j = glob.indexOf("}", i); re += "(" + glob.slice(i + 1, j).split(",").map(escape).join("|") + ")"; i = j; }
    else re += escape(c);
  }
  return new RegExp("^" + re + "$");
  function escape(s) { return s.replace(/[.+^$()|[\]\\?]/g, "\\$&"); }
}

// The wrapper that @vitejs/plugin-react adds after the refresh transform.
function addRefreshWrapper(code, id) {
  const hasRefresh = /\$RefreshReg\$\(/.test(code);
  if (!hasRefresh && !/extends\s+(?:React\.)?(?:Pure)?Component/.test(code)) return code;
  let out = code + `

import * as RefreshRuntime from "/@react-refresh";
const inWebWorker = typeof WorkerGlobalScope !== 'undefined' && self instanceof WorkerGlobalScope;
if (import.meta.hot && !inWebWorker) {
  if (!window.$RefreshReg$) {
    throw new Error("buck2 dev can't detect the React refresh preamble.");
  }
  RefreshRuntime.__hmr_import(import.meta.url).then((currentExports) => {
    RefreshRuntime.registerExportsForReactRefresh(${JSON.stringify(id)}, currentExports);
    import.meta.hot.accept((nextExports) => {
      if (!nextExports) return;
      const invalidateMessage = RefreshRuntime.validateRefreshBoundaryAndEnqueueUpdate(${JSON.stringify(id)}, currentExports, nextExports);
      if (invalidateMessage) import.meta.hot.invalidate(invalidateMessage);
    });
  });
}
`;
  if (hasRefresh) out += `function $RefreshReg$(type, id) { return RefreshRuntime.register(type, ${JSON.stringify(id)} + ' ' + id) }
function $RefreshSig$() { return RefreshRuntime.createSignatureFunctionForTransform(); }
`;
  return out;
}

// Compiles one module. `argv` is the command line above; a module that cannot be compiled
// throws a `TransformError` that says why.
export async function transform(argv) {
  const { values: args } = parseArgs({
    args: argv,
    options: {
      ctx: { type: "string" },
      path: { type: "string" },
      query: { type: "string", default: "" },
      src: { type: "string" },
      out: { type: "string" },
      assets: { type: "string" },
      "plugins-dir": { type: "string" },
      plugin: { type: "string", multiple: true, default: [] },
    },
  });
  const job = new Module(args);
  // The action declares the assets directory whether or not the module makes any.
  if (args.assets) fs.mkdirSync(args.assets, { recursive: true });
  const code = await job.compile();
  fs.mkdirSync(path.dirname(args.out), { recursive: true });
  fs.writeFileSync(args.out, code);
}

class Module {
  constructor(args) {
    this.args = args;
    this.ctx = JSON.parse(fs.readFileSync(args.ctx, "utf8"));
    this.files = new Set(this.ctx.files);
    this.path = args.path;
    this.query = args.query;
    this.ext = extname(this.path);
    this.root = process.cwd();
  }

  fail(message) {
    throw new TransformError(`${this.path}${this.query ? "?" + this.query : ""}: ${message}`);
  }

  async compile() {
    const q = this.query;
    const params = new URLSearchParams(q);
    if (IMAGE_EXTS.includes(this.ext) && [...params.keys()].some((k) => IMAGE_DIRECTIVES.includes(k))) {
      return this.compileImage(params);
    }
    if (this.args.plugin.length) {
      const hosted = await this.runPlugins();
      if (hosted != null) return this.compileScript(hosted, "js");
    }
    if (q === "worker") return this.compileWorker();
    if (q === "url") return `export default ${JSON.stringify(JS_EXTS.includes(this.ext) ? keyUrl(this.path) : "/" + this.path)};\n`;
    if (q === "raw" && !CSS_EXTS.includes(this.ext)) return this.compileAsset();
    if (q && !["inline", "raw"].includes(q)) this.fail(`no plugin or built-in handles the query "${q}"`);
    const source = () => fs.readFileSync(this.args.src, "utf8");
    if (JS_EXTS.includes(this.ext)) return this.compileScript(source());
    if (CSS_EXTS.includes(this.ext)) return this.compileStyle(source());
    if (this.ext === ".json") return this.compileJson(source());
    return this.compileAsset();
  }

  // --- Resolution -----------------------------------------------------------------------

  probe(base) {
    if (this.files.has(base)) return base;
    for (const e of PROBE) if (this.files.has(base + e)) return base + e;
    // `./x.js` naming `./x.ts`, as TypeScript allows.
    const m = /\.(m?)js$/.exec(base);
    if (m) {
      for (const e of [`.${m[1]}ts`, ".tsx"]) {
        const alt = base.slice(0, -m[0].length) + e;
        if (this.files.has(alt)) return alt;
      }
    }
    return null;
  }

  // The module key that `specifier` names from this module, or an absolute URL.
  resolve(specifier) {
    let [spec, q = ""] = splitQuery(specifier);
    const withQuery = (key) => ({ key: key + (q ? "~" + q : "") });
    if (/^(https?:|data:|\/@)/.test(spec)) return { url: specifier };
    if (this.ctx.virtual?.[spec]) return withQuery(this.ctx.virtual[spec]);
    // A path in the file system, as plugins write them: the project is the working directory.
    if (spec.startsWith(this.root + "/")) spec = spec.slice(this.root.length);
    for (const [from, to] of Object.entries(this.ctx.aliases || {})) {
      if (spec === from.replace(/\/$/, "") || spec.startsWith(from)) {
        spec = "/" + to + spec.slice(from.length);
        break;
      }
    }
    if (spec.startsWith(".") || spec.startsWith("/")) {
      const base = spec.startsWith("/")
        ? path.posix.normalize(spec.slice(1))
        : path.posix.normalize(path.posix.join(path.posix.dirname(this.path), spec));
      const hit = this.probe(base);
      if (!hit) this.fail(`cannot resolve "${specifier}": no source ${base} in this library`);
      return withQuery(hit);
    }
    const parts = spec.split("/");
    const name = spec.startsWith("@") ? parts.slice(0, 2).join("/") : parts[0];
    const sub = "." + spec.slice(name.length);
    const ws = this.ctx.workspace?.[name];
    if (ws) {
      const target = ws.exports[sub];
      if (!target) this.fail(`cannot resolve "${specifier}": ${name} exports no "${sub}" (it exports ${Object.keys(ws.exports).join(", ")})`);
      return withQuery(target);
    }
    const entry = this.ctx.npm?.[name];
    if (entry) {
      if (q) this.fail(`"${specifier}": queries on npm imports are not supported here`);
      return { url: `/@npm/${entry}/${sub === "." ? "index" : sub.slice(2)}.js` };
    }
    this.fail(`cannot resolve "${specifier}": ${name} is not a dependency of ${this.ctx.package}`);
  }

  urlOf(specifier) {
    const r = this.resolve(specifier);
    return r.url ?? keyUrl(r.key);
  }

  // --- import.meta.glob and variable dynamic imports --------------------------------------

  // `import(\`./messages/${locale}.json\`)`, which Vite turns into a lookup in a glob.
  expandDynamicImports(code) {
    let used = false;
    const out = code.replace(/import\s*\(\s*`((?:\.\.?\/)[^`]*\$\{[^`]*)`\s*\)/g, (_, template) => {
      used = true;
      const glob = template.replace(/\$\{[^}]*\}/g, "*");
      return `__variableDynamicImport(import.meta.glob(${JSON.stringify(glob)}), \`${template}\`)`;
    });
    if (!used) return code;
    return out + `
function __variableDynamicImport(glob, path) {
  const load = glob[path];
  return load ? load() : Promise.reject(new Error("Unknown variable dynamic import: " + path));
}
`;
  }

  // Expands `import.meta.glob(...)` against the library's sources, as Vite does. The arguments
  // must be literals, as Vite requires.
  expandGlobs(code) {
    let n = 0;
    const hoisted = [];
    const out = code.replace(/import\.meta\.glob\s*(?:<[^>]*>)?\s*\(([^)]*)\)/g, (_, argText) => {
      const [patterns, opts = {}] = Function(`"use strict"; return [${argText}]`)();
      const list = Array.isArray(patterns) ? patterns : [patterns];
      const dir = path.posix.dirname(this.path);
      const pos = [], neg = [];
      for (const p of list) {
        const negated = p.startsWith("!");
        let rel = negated ? p.slice(1) : p;
        for (const [from, to] of Object.entries(this.ctx.aliases || {})) {
          if (rel.startsWith(from)) rel = "/" + to + rel.slice(from.length);
        }
        const abs = rel.startsWith("/") ? rel.slice(1) : path.posix.normalize(path.posix.join(dir, rel));
        (negated ? neg : pos).push(globToRegExp(abs));
      }
      const matches = [...this.files].filter((f) => f !== this.path && pos.some((r) => r.test(f)) && !neg.some((r) => r.test(f))).sort();
      const q = opts.query ? (typeof opts.query === "string" ? opts.query.replace(/^\?/, "") : new URLSearchParams(opts.query).toString()) : "";
      const entries = matches.map((f) => {
        let rel = path.posix.relative(dir, f);
        if (!rel.startsWith(".")) rel = "./" + rel;
        // A relative specifier: the import rewriting below resolves it like any other.
        const spec = rel + (q ? "?" + q : "");
        if (opts.eager) {
          const id = `__glob_${n++}`;
          hoisted.push(opts.import && opts.import !== "*"
            ? `import { ${opts.import} as ${id} } from ${JSON.stringify(spec)};`
            : `import * as ${id} from ${JSON.stringify(spec)};`);
          return `${JSON.stringify(rel)}: ${id}`;
        }
        const pick = opts.import && opts.import !== "*" ? `.then((m) => m[${JSON.stringify(opts.import)}])` : "";
        return `${JSON.stringify(rel)}: () => import(${JSON.stringify(spec)})${pick}`;
      });
      return `({${entries.join(", ")}})`;
    });
    return hoisted.length ? hoisted.join("\n") + "\n" + out : out;
  }

  // `new URL("./x", import.meta.url)`, which Vite rewrites to the URL that serves the file.
  rewriteNewUrl(code) {
    return code.replace(/new\s+URL\s*\(\s*(['"])([^'"]+)\1\s*,\s*import\.meta\.url\s*\)/g, (m, _q, spec) => {
      if (!spec.startsWith(".")) return m;
      const r = this.resolve(spec);
      const [p] = r.key.split("~");
      const url = JS_EXTS.includes(extname(p)) ? keyUrl(r.key) : "/" + p;
      return `new URL(${JSON.stringify(url)}, import.meta.url)`;
    });
  }

  // --- Vite plugins -----------------------------------------------------------------------

  // Runs the build hooks of the plugins, as Vite's plugin container does for one module: the
  // first `resolveId` and `load` that answer, then every `transform`. The code they give, or
  // null when none of them handles the module.
  async runPlugins() {
    const plugins = await loadPlugins(this.args["plugins-dir"], this.args.plugin);
    const abs = path.join(this.root, this.path);
    let id = abs + (this.query ? "?" + this.query : "");
    const self = this;
    const context = {
      async resolve(source, importer) {
        const r = self.resolve(source.startsWith(".") && importer ? path.join(path.dirname(importer), source) : source);
        return r.key ? { id: path.join(self.root, r.key.split("~")[0]) } : null;
      },
      error(e) { self.fail(typeof e === "string" ? e : e.message); },
      warn() {},
      addWatchFile() {},
      meta: { watchMode: true },
      environment: { name: "client", mode: "dev" },
    };
    let handled = false;
    for (const p of plugins) {
      const r = await callHook(p.resolveId, context, id, [id, undefined, {}]);
      if (r) { id = typeof r === "string" ? r : r.id; handled = true; break; }
    }
    let code = null;
    for (const p of plugins) {
      const r = await callHook(p.load, context, id, [id]);
      if (r != null) { code = typeof r === "string" ? r : r.code; break; }
    }
    for (const p of plugins) {
      if (!hookMatches(p.transform, id)) continue;
      if (code == null) code = fs.readFileSync(this.args.src, "utf8");
      const r = await callHook(p.transform, context, id, [code, id]);
      if (r != null) { code = typeof r === "string" ? r : r.code; handled = true; }
    }
    return handled || code != null ? code : null;
  }

  // --- Compilers --------------------------------------------------------------------------

  async compileScript(source, lang = { ".ts": "ts", ".mts": "ts", ".tsx": "tsx", ".jsx": "jsx" }[this.ext] ?? "js") {
    const jsx = lang === "tsx" || lang === "jsx";
    const result = transformSync(this.path, source, {
      lang,
      sourceType: "module",
      sourcemap: true,
      jsx: { runtime: "automatic", development: true, refresh: jsx },
      define: this.ctx.define ?? {},
    });
    if (result.errors.length) this.fail(result.errors.map((e) => e.codeframe ?? e.message).join("\n"));
    let code = this.expandGlobs(this.expandDynamicImports(this.rewriteNewUrl(result.code)));
    await init;
    const [imports] = parse(code);
    let rewritten = "";
    let last = 0;
    for (const imp of imports) {
      // Static imports, dynamic imports with a literal, and re-exports; not `import.meta`.
      if (imp.n === undefined || imp.d === -2) continue;
      const url = this.urlOf(imp.n);
      const quoted = imp.d > -1;
      const start = quoted ? imp.s + 1 : imp.s;
      const end = quoted ? imp.e - 1 : imp.e;
      rewritten += code.slice(last, start) + url;
      last = end;
    }
    code = rewritten + code.slice(last);
    code = addRefreshWrapper(code, "/" + this.path);
    const map = { ...result.map, sources: ["/" + this.path], sourcesContent: [source] };
    return code + "\n//# sourceMappingURL=data:application/json;base64," + Buffer.from(JSON.stringify(map)).toString("base64") + "\n";
  }

  async compileStyle(source) {
    const { transform, browserslistToTargets } = await import("lightningcss");
    const css = this.ctx.css ?? {};
    const isModule = /\.module\.\w+$/.test(this.path);
    let text = source;
    if (this.ext === ".scss" || this.ext === ".sass") {
      const sass = await import("sass");
      const root = this.root;
      const aliases = Object.entries(this.ctx.aliases || {});
      // Sources are inputs of this action at their paths in the project, which is the working
      // directory, so Sass finds partials as it would in the source tree. Aliases resolve as
      // they do for imports.
      const importer = {
        findFileUrl(url) {
          for (const [from, to] of aliases) {
            if (url.startsWith(from)) return pathToFileURL(path.join(root, to, url.slice(from.length)));
          }
          return null;
        },
      };
      text = sass.compileString((css.scssAdditionalData ?? "") + source, {
        url: pathToFileURL(path.join(root, this.path)),
        syntax: this.ext === ".sass" ? "indented" : "scss",
        importers: [importer],
        style: "expanded",
      }).css;
    }
    let targets;
    if (css.browserslist?.length) {
      const { default: browserslist } = await import("browserslist");
      targets = browserslistToTargets(browserslist(css.browserslist));
    }
    let result;
    try {
      result = transform({
        filename: this.path,
        code: Buffer.from(text),
        cssModules: isModule ? { pattern: css.modules?.pattern ?? "[local]_[hash]" } : false,
        analyzeDependencies: true,
        targets,
      });
    } catch (e) {
      this.fail(`${e.message}${e.loc ? ` at line ${e.loc.line}` : ""}`);
    }
    text = result.code.toString();
    const imports = [];
    for (const dep of result.dependencies ?? []) {
      if (dep.type === "url") {
        let url = dep.url;
        if (!/^(https?:|data:|\/|#)/.test(url)) {
          const p = path.posix.normalize(path.posix.join(path.posix.dirname(this.path), url));
          if (!this.files.has(p)) this.fail(`url(${dep.url}): no source ${p} in this library`);
          url = "/" + p;
        }
        text = text.replaceAll(dep.placeholder, url);
      } else if (dep.type === "import") {
        imports.push(this.urlOf(dep.url));
      }
    }
    if (this.query === "inline" || this.query === "raw") return `export default ${JSON.stringify(text)};\n`;
    return styleModule("/" + this.path, text, imports, isModule ? classMap(result.exports, css.modules?.camelCaseOnly) : null);
  }

  compileJson(source) {
    const value = JSON.parse(source);
    const lines = [`const __json = ${JSON.stringify(value)};`, `export default __json;`];
    if (value && typeof value === "object" && !Array.isArray(value)) {
      for (const k of Object.keys(value)) if (/^[A-Za-z_$][\w$]*$/.test(k)) lines.push(`export const ${k} = __json[${JSON.stringify(k)}];`);
    }
    return lines.join("\n") + "\n";
  }

  compileAsset() {
    const q = this.query;
    if (q === "raw") return `export default ${JSON.stringify(fs.readFileSync(this.args.src, "utf8"))};\n`;
    if (q === "inline") {
      const mime = MIME[this.ext] ?? "application/octet-stream";
      const data = mime === "image/svg+xml"
        ? "data:image/svg+xml," + encodeURIComponent(fs.readFileSync(this.args.src, "utf8"))
        : `data:${mime};base64,` + fs.readFileSync(this.args.src).toString("base64");
      return `export default ${JSON.stringify(data)};\n`;
    }
    // The source file itself is the asset: Vite serves files under its root as they are.
    return `export default ${JSON.stringify("/" + this.path)};\n`;
  }

  compileWorker() {
    return [
      `export default function WorkerWrapper(options) {`,
      `  return new Worker(${JSON.stringify(keyUrl(this.path))}, { type: "module", name: options?.name });`,
      `}`,
    ].join("\n") + "\n";
  }

  // What vite-imagetools does for `?w=400;800&format=avif;webp&as=picture`: resize and convert
  // with sharp, write the images to the assets directory, and export their URLs.
  async compileImage(params) {
    if (!this.args.assets) this.fail("an image query needs --assets");
    const preset = params.get("preset");
    const directives = { ...(preset ? this.ctx.imagePresets?.[preset] ?? this.fail(`no image preset "${preset}"`) : {}) };
    for (const [k, v] of params) if (k !== "preset") directives[k] = v;
    const { default: sharp } = await import("sharp");
    const meta = await sharp(this.args.src).metadata();
    const list = (v) => (v ? String(v).split(";").filter(Boolean) : []);
    const widths = list(directives.w).map(Number);
    if (!widths.length) widths.push(meta.width);
    const formats = list(directives.format);
    if (!formats.length) formats.push(meta.format === "jpeg" ? "jpg" : meta.format);
    const quality = directives.quality ? Number(directives.quality) : undefined;
    const base = path.basename(this.path, this.ext);
    fs.mkdirSync(this.args.assets, { recursive: true });
    const images = {};
    for (const format of formats) {
      images[format] = [];
      for (const w of widths) {
        const width = Math.min(w, meta.width);
        const name = `${base}-${width}.${format}`;
        const info = await sharp(this.args.src)
          .resize({ width })
          .toFormat(format === "jpg" ? "jpeg" : format, quality ? { quality } : {})
          .toFile(path.join(this.args.assets, name));
        images[format].push({ src: "/" + path.posix.join(this.args.assets, name), w: info.width, h: info.height });
      }
    }
    const srcset = (imgs) => imgs.map((i) => `${i.src} ${i.w}w`).join(", ");
    let value;
    switch (directives.as) {
      case "srcset":
        value = formats.length === 1 ? srcset(images[formats[0]]) : formats.map((f) => srcset(images[f]));
        break;
      case "picture": {
        const fallback = images[formats[formats.length - 1]];
        const img = fallback[fallback.length - 1];
        value = { sources: Object.fromEntries(formats.slice(0, -1).map((f) => [f, srcset(images[f])])), img: { src: img.src, w: img.w, h: img.h } };
        break;
      }
      case "metadata":
        value = images[formats[0]].map((i) => ({ ...i, format: formats[0] }));
        break;
      default: {
        const all = formats.flatMap((f) => images[f].map((i) => i.src));
        value = all.length === 1 ? all[0] : all;
      }
    }
    return `export default ${JSON.stringify(value)};\n`;
  }
}

function hookMatches(hook, id) {
  if (!hook) return false;
  const filter = typeof hook === "object" ? hook.filter?.id : undefined;
  if (!filter) return true;
  const test = (f) => (f instanceof RegExp ? f.test(id) : typeof f === "string" ? f === id : false);
  if (filter instanceof RegExp || typeof filter === "string") return test(filter);
  if (Array.isArray(filter)) return filter.some(test);
  const include = filter.include == null ? true : [].concat(filter.include).some(test);
  const exclude = filter.exclude == null ? false : [].concat(filter.exclude).some(test);
  return include && !exclude;
}

async function callHook(hook, context, id, args) {
  if (!hookMatches(hook, id)) return null;
  const handler = typeof hook === "function" ? hook : hook.handler;
  return (await handler.apply(context, args)) ?? null;
}

function classMap(exports, camelCaseOnly) {
  const classes = {};
  for (const [k, v] of sortedEntries(exports ?? {})) {
    // lightningcss gives the exports in hash map order; sorted, the module is the same each build.
    classes[camelCaseOnly ? camelCase(k) : k] = [v.name, ...v.composes.map((c) => c.name)].join(" ");
  }
  return classes;
}

// A stylesheet as a module that Vite's client puts into the page and replaces on HMR, as
// Vite's own CSS plugin writes it.
export function styleModule(id, text, imports, classes) {
  const lines = imports.map((u) => `import ${JSON.stringify(u)};`);
  lines.push(
    `import { updateStyle as __vite__updateStyle, removeStyle as __vite__removeStyle } from "/@vite/client";`,
    `const __vite__id = ${JSON.stringify(id)};`,
    `const __vite__css = ${JSON.stringify(text)};`,
    `__vite__updateStyle(__vite__id, __vite__css);`,
  );
  if (classes) {
    lines.push(`const __vite__classes = ${JSON.stringify(classes)};`, `export default __vite__classes;`);
    for (const k of Object.keys(classes)) if (/^[A-Za-z_$][\w$]*$/.test(k)) lines.push(`export const ${k} = __vite__classes[${JSON.stringify(k)}];`);
  } else {
    // A plain stylesheet replaces itself. A CSS module's importer decides, since the class
    // names it exports may have changed.
    lines.push(`import.meta.hot.accept();`);
  }
  lines.push(`import.meta.hot.prune(() => __vite__removeStyle(__vite__id));`);
  return lines.join("\n") + "\n";
}

if (import.meta.main) {
  try {
    await transform(process.argv.slice(2));
  } catch (e) {
    process.stderr.write((e instanceof TransformError ? e.message : e.stack) + "\n");
    process.exit(1);
  }
}
