// Pre-bundles one npm package, one entry of the npm store, into ES modules for the browser:
// what Vite's dependency optimizer does, but for one package at a time, so that each package is
// its own cached action and a new dependency bundles only itself.
//
//   npm_esm.mjs --pkg DIR --out OUTDIR
//
// DIR is the package in its store entry (`<entry>/node_modules/<name>`), beside links to its
// dependencies. Each subpath that the package exports becomes `OUTDIR/<subpath>.js` (`index.js`
// for `.`), and code the subpaths share goes into chunks beside them, so that `react-dom` and
// `react-dom/client` share one copy of React DOM's internals.
//
// The package's own dependencies stay outside: an import of `react` becomes an import of
// `/@npm/<react's entry>/index.js`, which is that package's own bundle, so the page loads one
// copy of each package however many packages use it. CommonJS gets named exports, found with
// the lexer that Node uses, and a `require` of a dependency reads that dependency's bundle.

import fs from "node:fs";
import path from "node:path";
import { parseArgs } from "node:util";
import { builtinModules } from "node:module";
import * as esbuild from "esbuild";
import { init as initCjs, parse as parseCjs } from "cjs-module-lexer";
import { init as initEsm, parse as parseEsm } from "es-module-lexer";

const { values: args } = parseArgs({ options: { pkg: { type: "string" }, out: { type: "string" } } });
const pkgDir = path.resolve(args.pkg);
const outDir = path.resolve(args.out);
const pkg = JSON.parse(fs.readFileSync(path.join(pkgDir, "package.json"), "utf8"));
const CONDITIONS = ["browser", "development", "import", "module", "default", "require"];

// --- The package's dependencies, and the store entries they link to ----------------------

const modulesDir = path.dirname(pkg.name.startsWith("@") ? path.dirname(pkgDir) : pkgDir);
const deps = new Map();
for (const name of fs.readdirSync(modulesDir)) {
  const names = name.startsWith("@") ? fs.readdirSync(path.join(modulesDir, name)).map((n) => `${name}/${n}`) : [name];
  for (const n of names) {
    if (n === pkg.name) continue;
    const link = fs.readlinkSync(path.join(modulesDir, n));
    // `../../<entry>/node_modules/<name>`, one more `..` for a scoped name.
    const m = /(?:\.\.\/)+([^/]+)\/node_modules\//.exec(link);
    if (m) deps.set(n, m[1]);
  }
}
const depUrl = (spec) => {
  const parts = spec.split("/");
  const name = spec.startsWith("@") ? parts.slice(0, 2).join("/") : parts[0];
  const entry = deps.get(name);
  if (!entry) return null;
  const sub = spec.slice(name.length + 1);
  return `/@npm/${entry}/${sub || "index"}.js`;
};

// --- Entries ------------------------------------------------------------------------------

function pick(target) {
  if (typeof target === "string") return target;
  if (Array.isArray(target)) return target.map(pick).find(Boolean);
  if (target && typeof target === "object") {
    for (const c of CONDITIONS) if (c in target) { const r = pick(target[c]); if (r) return r; }
  }
  return null;
}

const entries = {};
const exp = pkg.exports;
if (exp && (typeof exp === "string" || Array.isArray(exp) || !Object.keys(exp).some((k) => k.startsWith(".")))) {
  entries["index"] = pick(exp);
} else if (exp) {
  for (const [sub, target] of Object.entries(exp)) {
    // Pattern exports (`./*`) would be one entry per file: a package that has them is better
    // served file by file. `package.json` and non-JS files are not modules.
    if (sub.includes("*") || sub === "./package.json") continue;
    const file = pick(target);
    if (file && /\.(c|m)?js$/.test(file)) entries[sub === "." ? "index" : sub.slice(2)] = file;
  }
} else {
  const browser = typeof pkg.browser === "string" ? pkg.browser : null;
  entries["index"] = browser ?? pkg.module ?? pkg.main ?? "index.js";
}

await initCjs();
await initEsm;

function resolveFile(file) {
  for (const f of [file, file + ".js", path.join(file, "index.js")]) {
    if (fs.existsSync(f) && fs.statSync(f).isFile()) return f;
  }
  return null;
}

// The names a CommonJS module exports, following `module.exports = require("./x")`.
function cjsExports(file, seen = new Set()) {
  if (seen.has(file)) return new Set();
  seen.add(file);
  const { exports, reexports } = parseCjs(fs.readFileSync(file, "utf8"));
  const names = new Set(exports);
  for (const r of reexports) {
    if (!r.startsWith(".")) continue;
    const f = resolveFile(path.resolve(path.dirname(file), r));
    if (f) for (const n of cjsExports(f, seen)) names.add(n);
  }
  return names;
}

function isEsm(file) {
  if (file.endsWith(".mjs")) return true;
  if (file.endsWith(".cjs")) return false;
  const [, , , hasModuleSyntax] = parseEsm(fs.readFileSync(file, "utf8"));
  return hasModuleSyntax;
}

const IDENT = /^[A-Za-z_$][\w$]*$/;
const entryPoints = [];
for (const [out, rel] of Object.entries(entries)) {
  const file = resolveFile(path.join(pkgDir, rel));
  if (!file) continue;
  if (isEsm(file)) {
    entryPoints.push({ in: file, out });
    continue;
  }
  const lines = [`import __cjs from ${JSON.stringify(file)};`, `export default __cjs;`];
  let i = 0;
  for (const name of cjsExports(file)) {
    if (name === "default" || name === "__esModule") continue;
    const id = `__e${i++}`;
    lines.push(`const ${id} = __cjs[${JSON.stringify(name)}];`, `export { ${id} as ${IDENT.test(name) ? name : JSON.stringify(name)} };`);
  }
  entryPoints.push({ in: `cjs-entry:${out}`, out, contents: lines.join("\n") });
}
if (!entryPoints.length) {
  process.stderr.write(`${pkg.name}: no JavaScript entry points\n`);
}

// --- Bundle -------------------------------------------------------------------------------

const requires = new Set();
const plugin = {
  name: "npm-esm",
  setup(build) {
    build.onResolve({ filter: /^cjs-entry:/ }, (a) => ({ path: a.path, namespace: "cjs-entry" }));
    build.onLoad({ filter: /.*/, namespace: "cjs-entry" }, (a) => ({
      contents: entryPoints.find((e) => e.in === a.path).contents,
      resolveDir: pkgDir,
      loader: "js",
    }));
    build.onResolve({ filter: /^[^./]/ }, (a) => {
      if (a.path.startsWith("node:") || builtinModules.includes(a.path.split("/")[0])) {
        return { path: a.path, namespace: "node-builtin" };
      }
      const name = a.path.startsWith("@") ? a.path.split("/").slice(0, 2).join("/") : a.path.split("/")[0];
      if (name === pkg.name) return undefined;
      const url = depUrl(a.path);
      if (!url) {
        // An optional or peer dependency that is not installed: an empty module, as Vite has
        // for packages it cannot find in the browser.
        return { path: a.path, namespace: "missing" };
      }
      if (a.kind === "require-call") {
        requires.add(a.path);
        return { path: a.path, external: true };
      }
      return { path: url, external: true };
    });
    build.onLoad({ filter: /.*/, namespace: "node-builtin" }, () => ({ contents: "module.exports = {};", loader: "js" }));
    build.onLoad({ filter: /.*/, namespace: "missing" }, () => ({ contents: "module.exports = {};", loader: "js" }));
  },
};

fs.mkdirSync(outDir, { recursive: true });
const result = await esbuild.build({
  entryPoints: entryPoints.map(({ in: i, out }) => ({ in: i, out })),
  bundle: true,
  splitting: true,
  format: "esm",
  platform: "browser",
  target: "es2022",
  outdir: outDir,
  chunkNames: "_chunks/[name]-[hash]",
  conditions: ["development", "browser", "module", "import"],
  mainFields: ["browser", "module", "main"],
  define: { "process.env.NODE_ENV": '"development"', global: "globalThis" },
  plugins: [plugin],
  logLevel: "error",
  absWorkingDir: pkgDir,
  write: false,
});

// esbuild leaves `require("react")` of a dependency to a `require` in scope. Each chunk gets
// one that reads the dependency's bundle: its default export, which for a CommonJS package is
// `module.exports`.
const shimImports = [...requires].sort().map((spec, i) => `import * as __req${i} from ${JSON.stringify(depUrl(spec))};`);
const shim = shimImports.length
  ? shimImports.join("\n") + "\n" +
    `function require(id) { const m = { ${[...requires].sort().map((s, i) => `${JSON.stringify(s)}: __req${i}`).join(", ")} }[id];` +
    ` if (!m) throw new Error("require(" + JSON.stringify(id) + ") is not a dependency of ${pkg.name}");` +
    ` return "default" in m ? m.default : m; }\n`
  : "";
for (const file of result.outputFiles) {
  fs.mkdirSync(path.dirname(file.path), { recursive: true });
  const text = file.text;
  fs.writeFileSync(file.path, shim && text.includes("__require") ? shim + text : text);
}
