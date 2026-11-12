// Loads an app's own Vite plugins, for transform.mjs to run their build hooks in an action and
// for the dev server to run their server hooks.
//
// `dir` is the plugins' package as buck2 stages it (`nodejs_library`), with their npm
// dependencies linked beside it, so their imports resolve as they do in the app. Each spec is
// `FILE:EXPORT[:JSON]`: the plugin is `EXPORT(...JSON)` from `dir/FILE`, where JSON is an array of
// arguments. Plugins are usually TypeScript. Node does not strip types from files under
// `node_modules`, where the staged package is, so a module hook compiles them with oxc.

import fs from "node:fs";
import path from "node:path";
import { registerHooks } from "node:module";
import { fileURLToPath, pathToFileURL } from "node:url";
import { transformSync } from "oxc-transform";

let hooked = false;
const cache = new Map();

function hookTypeScript() {
  if (hooked) return;
  hooked = true;
  registerHooks({
    load(url, context, nextLoad) {
      if (!/\.(m?ts|tsx)$/.test(url) || !url.startsWith("file:")) return nextLoad(url, context);
      const file = fileURLToPath(url);
      const { code, errors } = transformSync(file, fs.readFileSync(file, "utf8"), {
        lang: file.endsWith(".tsx") ? "tsx" : "ts",
        sourceType: "module",
      });
      if (errors.length) throw new Error(`${file}: ${errors[0].message}`);
      return { format: "module", source: code, shortCircuit: true };
    },
  });
}

// `[file, export, args]` of each spec.
export function parseSpecs(specs) {
  return specs.map((spec) => {
    const [file, name, ...rest] = spec.split(":");
    return [file, name, rest.length ? JSON.parse(rest.join(":")) : []];
  });
}

// The plugins of `specs`, flattened as Vite flattens arrays of plugins.
export async function loadPlugins(dir, specs) {
  const key = dir + "\0" + specs.join("\0");
  if (!cache.has(key)) {
    cache.set(key, (async () => {
      hookTypeScript();
      const plugins = [];
      for (const [file, name, args] of parseSpecs(specs)) {
        const mod = await import(pathToFileURL(path.resolve(dir, file)).href);
        if (typeof mod[name] !== "function") throw new Error(`${file} exports no function "${name}"`);
        plugins.push(...[await mod[name](...args)].flat(Infinity).filter(Boolean));
      }
      // Vite runs `enforce: "pre"` plugins first and `"post"` last.
      const order = (p) => (p.enforce === "pre" ? 0 : p.enforce === "post" ? 2 : 1);
      return plugins.sort((a, b) => order(a) - order(b));
    })());
  }
  return cache.get(key);
}
