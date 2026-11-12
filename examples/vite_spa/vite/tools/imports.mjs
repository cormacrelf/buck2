// The static imports of an ES module, as the dev server reads them to build a module's imports
// before the browser asks for them.

import { init, parse } from "es-module-lexer";

await init;

// The specifiers of the module's static imports and re-exports, but not its `import()`s.
export function staticImports(code) {
  const [imports] = parse(code);
  return imports.filter((i) => i.d === -1 && i.n !== undefined).map((i) => i.n);
}
