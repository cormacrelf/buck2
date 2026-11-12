// Compiles one module of a `vite_library` into the ES module that the browser loads, with every
// import already resolved to the URL the dev server serves it at.
//
//   transform.mjs --ctx CTX --path PATH [--query QUERY] --src SRC --out OUT
//
// PATH is the source's path in the project, SRC the file to read (the same file, or a generated
// one placed at PATH), and QUERY a Vite-style import query such as `raw`, `url`, `inline` or
// `worker`. CTX is the library's resolution context, written by `vite_library`:
//
//   {"package": "apps/web", "files": [...own source paths...],
//    "aliases": {"@/": "apps/web/src/"},
//    "workspace": {"@demo/ui": {"package": "packages/ui", "exports": {".": "packages/ui/src/index.ts"}}},
//    "npm": {"react": "react@19.3.1", ...}}           // alias -> npm store entry name
//
// A module's key is its path, with `~QUERY` when imported with a query. Its URL is `/` + key,
// with `.js` appended unless the key already ends in a JS extension, so that Vite's own
// CSS/JSON/asset plugins leave the compiled module alone. `/@npm/<entry>/<subpath>.js` is a
// pre-bundled npm package (see npm_esm.mjs).

import fs from "node:fs";
import path from "node:path";
import { parseArgs } from "node:util";
import { transformSync } from "oxc-transform";
import { init, parse } from "es-module-lexer";

const { values: args } = parseArgs({
  options: {
    ctx: { type: "string" },
    path: { type: "string" },
    query: { type: "string", default: "" },
    src: { type: "string" },
    out: { type: "string" },
  },
});

const ctx = JSON.parse(fs.readFileSync(args.ctx, "utf8"));
const files = new Set(ctx.files);
const modulePath = args.path;
const query = args.query;
const ext = extname(modulePath);

const JS_EXTS = [".ts", ".tsx", ".mts", ".js", ".jsx", ".mjs"];
const CSS_EXTS = [".css", ".scss", ".sass"];
const PROBE = [...JS_EXTS, ".json", ...JS_EXTS.map((e) => "/index" + e)];
const MIME = {
  ".svg": "image/svg+xml", ".png": "image/png", ".jpg": "image/jpeg", ".jpeg": "image/jpeg",
  ".gif": "image/gif", ".webp": "image/webp", ".avif": "image/avif", ".ico": "image/x-icon",
  ".woff": "font/woff", ".woff2": "font/woff2", ".ttf": "font/ttf", ".txt": "text/plain",
  ".md": "text/markdown",
};

function extname(p) {
  const base = p.slice(p.lastIndexOf("/") + 1);
  if (base.endsWith(".d.ts")) return ".d.ts";
  const i = base.lastIndexOf(".");
  return i <= 0 ? "" : base.slice(i);
}

export function keyUrl(key) {
  const [p, q] = key.split("~");
  return "/" + key + (q === undefined && JS_EXTS.includes(extname(p)) ? "" : ".js");
}

function fail(message) {
  process.stderr.write(`${modulePath}${query ? "?" + query : ""}: ${message}\n`);
  process.exit(1);
}

// --- Resolution -------------------------------------------------------------------------

function probe(base, candidates) {
  if (candidates.has(base)) return base;
  for (const e of PROBE) if (candidates.has(base + e)) return base + e;
  // `./x.js` naming `./x.ts`, as TypeScript allows.
  const m = /\.(m?)js$/.exec(base);
  if (m) {
    for (const e of [`.${m[1]}ts`, ".tsx"]) {
      const alt = base.slice(0, -m[0].length) + e;
      if (candidates.has(alt)) return alt;
    }
  }
  return null;
}

// The module key that `specifier` names from this module, or an absolute URL.
function resolve(specifier) {
  let [spec, q = ""] = splitQuery(specifier);
  if (/^(https?:|data:|\/@)/.test(spec)) return { url: specifier };
  for (const [from, to] of Object.entries(ctx.aliases || {})) {
    if (spec === from.replace(/\/$/, "") || spec.startsWith(from)) {
      spec = "/" + to + spec.slice(from.length);
      break;
    }
  }
  if (spec.startsWith(".") || spec.startsWith("/")) {
    const base = spec.startsWith("/")
      ? path.posix.normalize(spec.slice(1))
      : path.posix.normalize(path.posix.join(path.posix.dirname(modulePath), spec));
    const hit = probe(base, files);
    if (!hit) fail(`cannot resolve "${specifier}": no source ${base} in this library`);
    return { key: hit + (q ? "~" + q : "") };
  }
  const parts = spec.split("/");
  const name = spec.startsWith("@") ? parts.slice(0, 2).join("/") : parts[0];
  const sub = "." + spec.slice(name.length);
  const ws = ctx.workspace?.[name];
  if (ws) {
    const target = ws.exports[sub];
    if (!target) fail(`cannot resolve "${specifier}": ${name} exports no "${sub}" (it exports ${Object.keys(ws.exports).join(", ")})`);
    return { key: target + (q ? "~" + q : "") };
  }
  const entry = ctx.npm?.[name];
  if (entry) {
    if (q) fail(`"${specifier}": queries on npm imports are not supported here`);
    return { url: `/@npm/${entry}/${sub === "." ? "index" : sub.slice(2)}.js` };
  }
  fail(`cannot resolve "${specifier}": ${name} is not a dependency of ${ctx.package}`);
}

function splitQuery(s) {
  const i = s.indexOf("?");
  return i < 0 ? [s, ""] : [s.slice(0, i), s.slice(i + 1)];
}

const urlOf = (r) => r.url ?? keyUrl(r.key);

// --- import.meta.glob ---------------------------------------------------------------------

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

// Expands `import.meta.glob(...)` against the library's sources, as Vite does. The arguments
// must be literals, as Vite requires.
function expandGlobs(code) {
  let n = 0;
  const hoisted = [];
  const out = code.replace(/import\.meta\.glob\s*\(([^)]*)\)/g, (_, argText) => {
    const [patterns, opts = {}] = Function(`"use strict"; return [${argText}]`)();
    const list = Array.isArray(patterns) ? patterns : [patterns];
    const dir = path.posix.dirname(modulePath);
    const pos = [], neg = [];
    for (const p of list) {
      const neg_ = p.startsWith("!");
      const rel = neg_ ? p.slice(1) : p;
      const abs = rel.startsWith("/") ? rel.slice(1) : path.posix.normalize(path.posix.join(dir, rel));
      (neg_ ? neg : pos).push([globToRegExp(abs), rel.startsWith("/") ? null : rel]);
    }
    const matches = [...files].filter((f) => f !== modulePath && pos.some(([r]) => r.test(f)) && !neg.some(([r]) => r.test(f))).sort();
    const q = opts.query ? (typeof opts.query === "string" ? opts.query.replace(/^\?/, "") : new URLSearchParams(opts.query).toString()) : "";
    const entries = matches.map((f) => {
      const rel = "./" + path.posix.relative(dir, f);
      // A relative specifier: the import rewriting below resolves it like any other.
      const url = rel + (q ? "?" + q : "");
      if (opts.eager) {
        const id = `__glob_${n++}`;
        hoisted.push(opts.import && opts.import !== "*"
          ? `import { ${opts.import} as ${id} } from ${JSON.stringify(url)};`
          : `import * as ${id} from ${JSON.stringify(url)};`);
        return `${JSON.stringify(rel)}: ${id}`;
      }
      const pick = opts.import && opts.import !== "*" ? `.then((m) => m[${JSON.stringify(opts.import)}])` : "";
      return `${JSON.stringify(rel)}: () => import(${JSON.stringify(url)})${pick}`;
    });
    return `({${entries.join(", ")}})`;
  });
  return hoisted.length ? hoisted.join("\n") + "\n" + out : out;
}

// `new URL("./x", import.meta.url)`, which Vite rewrites to the URL that serves the file.
function rewriteNewUrl(code) {
  return code.replace(/new\s+URL\s*\(\s*(['"])([^'"]+)\1\s*,\s*import\.meta\.url\s*\)/g, (m, _q, spec) => {
    if (!spec.startsWith(".")) return m;
    const r = resolve(spec);
    const [p] = r.key.split("~");
    const url = JS_EXTS.includes(extname(p)) ? keyUrl(r.key) : "/" + p;
    return `new URL(${JSON.stringify(url)}, import.meta.url)`;
  });
}

// --- React Fast Refresh -------------------------------------------------------------------
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

// --- Compilers ----------------------------------------------------------------------------

async function compileScript(source) {
  const lang = { ".ts": "ts", ".mts": "ts", ".tsx": "tsx", ".jsx": "jsx" }[ext] ?? "js";
  const jsx = lang === "tsx" || lang === "jsx";
  const result = transformSync(modulePath, source, {
    lang,
    sourceType: "module",
    sourcemap: true,
    jsx: { runtime: "automatic", development: true, refresh: jsx },
  });
  if (result.errors.length) fail(result.errors.map((e) => e.codeframe ?? e.message).join("\n"));
  let code = expandGlobs(rewriteNewUrl(result.code));
  await init;
  const [imports] = parse(code);
  let rewritten = "";
  let last = 0;
  for (const imp of imports) {
    // Static imports, dynamic imports with a literal, and re-exports; not `import.meta`.
    if (imp.n === undefined || imp.d === -2) continue;
    const url = urlOf(resolve(imp.n));
    const quoted = imp.d > -1;
    const start = quoted ? imp.s + 1 : imp.s;
    const end = quoted ? imp.e - 1 : imp.e;
    rewritten += code.slice(last, start) + url;
    last = end;
  }
  code = rewritten + code.slice(last);
  code = addRefreshWrapper(code, "/" + modulePath);
  const map = { ...result.map, sources: ["/" + modulePath], sourcesContent: [source] };
  return code + "\n//# sourceMappingURL=data:application/json;base64," + Buffer.from(JSON.stringify(map)).toString("base64") + "\n";
}

async function compileStyle(source) {
  const { transform } = await import("lightningcss");
  const isModule = /\.module\.\w+$/.test(modulePath);
  let css = source;
  if (ext === ".scss" || ext === ".sass") {
    const sass = await import("sass");
    // Partials are inputs of this action at their paths in the project, which is the working
    // directory, so Sass finds them as it would in the source tree.
    css = sass.compile(args.src, { style: "expanded", sourceMap: false }).css;
  }
  const result = transform({
    filename: modulePath,
    code: Buffer.from(css),
    cssModules: isModule ? { pattern: "[local]_[hash]" } : false,
    analyzeDependencies: true,
    drafts: { customMedia: true },
  });
  let text = result.code.toString();
  const imports = [];
  for (const dep of result.dependencies ?? []) {
    if (dep.type === "url") {
      let url = dep.url;
      if (!/^(https?:|data:|\/|#)/.test(url)) {
        const p = path.posix.normalize(path.posix.join(path.posix.dirname(modulePath), url));
        if (!files.has(p)) fail(`url(${dep.url}): no source ${p} in this library`);
        url = "/" + p;
      }
      text = text.replaceAll(dep.placeholder, url);
    } else if (dep.type === "import") {
      imports.push(urlOf(resolve(dep.url)));
    }
  }
  if (query === "inline" || query === "raw") return `export default ${JSON.stringify(text)};\n`;
  const lines = imports.map((u) => `import ${JSON.stringify(u)};`);
  lines.push(
    `import { updateStyle as __vite__updateStyle, removeStyle as __vite__removeStyle } from "/@vite/client";`,
    `const __vite__id = ${JSON.stringify("/" + modulePath)};`,
    `const __vite__css = ${JSON.stringify(text)};`,
    `__vite__updateStyle(__vite__id, __vite__css);`,
  );
  if (isModule) {
    const classes = Object.fromEntries(
      // lightningcss gives the exports in hash map order; sorted, the module is the same each build.
      Object.entries(result.exports ?? {}).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)).map(([k, v]) => [k, [v.name, ...v.composes.map((c) => c.name)].join(" ")]),
    );
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

function compileJson(source) {
  const value = JSON.parse(source);
  const lines = [`const __json = ${JSON.stringify(value)};`, `export default __json;`];
  if (value && typeof value === "object" && !Array.isArray(value)) {
    for (const k of Object.keys(value)) if (/^[A-Za-z_$][\w$]*$/.test(k)) lines.push(`export const ${k} = __json[${JSON.stringify(k)}];`);
  }
  return lines.join("\n") + "\n";
}

function compileAsset() {
  if (query === "raw") return `export default ${JSON.stringify(fs.readFileSync(args.src, "utf8"))};\n`;
  if (query === "inline") {
    const mime = MIME[ext] ?? "application/octet-stream";
    const data = mime === "image/svg+xml"
      ? "data:image/svg+xml," + encodeURIComponent(fs.readFileSync(args.src, "utf8"))
      : `data:${mime};base64,` + fs.readFileSync(args.src).toString("base64");
    return `export default ${JSON.stringify(data)};\n`;
  }
  // The source file itself is the asset: Vite serves files under its root as they are.
  return `export default ${JSON.stringify("/" + modulePath)};\n`;
}

function compileWorker() {
  return [
    `export default function WorkerWrapper(options) {`,
    `  return new Worker(${JSON.stringify(keyUrl(modulePath))}, { type: "module", name: options?.name });`,
    `}`,
  ].join("\n") + "\n";
}

let code;
if (query === "worker") code = compileWorker();
else if (query === "url") code = `export default ${JSON.stringify(JS_EXTS.includes(ext) ? keyUrl(modulePath) : "/" + modulePath)};\n`;
else if (query === "raw" && !CSS_EXTS.includes(ext)) code = compileAsset();
else if (JS_EXTS.includes(ext)) code = await compileScript(fs.readFileSync(args.src, "utf8"));
else if (CSS_EXTS.includes(ext)) code = await compileStyle(fs.readFileSync(args.src, "utf8"));
else if (ext === ".json") code = compileJson(fs.readFileSync(args.src, "utf8"));
else code = compileAsset();

fs.mkdirSync(path.dirname(args.out), { recursive: true });
fs.writeFileSync(args.out, code);
