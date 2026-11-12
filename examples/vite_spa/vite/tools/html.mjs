// Makes the module scripts of a package's `index.html` absolute in the project, as Vite does
// with a root at the package: `<script type="module" src="/src/main.tsx">` in `apps/web` loads
// `/apps/web/src/main.tsx`.
//
//   html.mjs --package apps/web --src index.html --out OUT

import fs from "node:fs";
import { parseArgs } from "node:util";

const { values: args } = parseArgs({ options: { package: { type: "string" }, src: { type: "string" }, out: { type: "string" } } });
const html = fs.readFileSync(args.src, "utf8").replace(
  /(<script\b[^>]*\btype=["']module["'][^>]*\bsrc=["'])([^"']+)/g,
  (m, head, src) => (/^(https?:)?\/\//.test(src) ? m : head + "/" + args.package + "/" + src.replace(/^\.?\//, "")),
);
fs.writeFileSync(args.out, html);
