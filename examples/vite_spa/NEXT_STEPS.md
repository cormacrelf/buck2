# Next steps

Ways to exercise the dev server setup further.

The README's "Not covered" list (Tailwind, file-based routes, Babel/React Compiler, WebAssembly, `server.proxy`, Vitest) is a start, but some features would stress our setup much harder than others. These are the ones I'd pick, roughly in order of what they'd teach us:

1. **WebAssembly from a Rust crate built by buck2.** A small crate compiled to wasm with wasm-bindgen, imported by a page. Editing the Rust would rebuild it and hot-reload the page. This is the case Vite on its own can't do well and buck2 can: a module whose build is a different toolchain's whole graph. It also tests whether the wasm module's JS glue gets HMR or a reload.
2. **File-based routing**, with TanStack Router's generator or a plain `import.meta.glob('./pages/*.tsx')`. The generated route tree depends on a directory listing rather than on files, so adding or deleting a page must invalidate it. That puts the earlier "new file not seen" problem in front of everyone, and with watchman now on we'd see whether it's really fixed.
3. **vanilla-extract (`.css.ts`).** The stylesheet is produced by running a TypeScript module, and its imports, in node at build time. That's a transform action which evaluates a module graph rather than compiling one file. The resulting CSS depends on everything the `.css.ts` imports, so it tests how dependency tracking works for evaluated code.
4. **A backend behind `server.proxy`**, built by buck2: a small Rust or node API that `buck2 dev` rebuilds and restarts when its sources change, with the frontend calling `/api`. This is the full-stack story. One dev session has a long-running process alongside the module actions, and a code change on either side shows up live.
5. **Type errors in the overlay.** Per-package `tsc` as buck2 actions in the dev session, with errors pushed to Vite's error overlay, like vite-plugin-checker. It tests sending diagnostics that aren't build failures, and incremental typechecking across package boundaries.
6. **Scale.** A generated app with a few thousand modules and a deep import graph, to measure cold page load, one-file HMR latency, and the cost of the Tailwind candidate set. Every other result so far comes from an app of about 100 sources, so this would tell us the most about whether the design holds up.

Two smaller ones are cheap to add: CSS modules that `composes:` from another file (a dependency between CSS files), and `import()` with a template string, like `` import(`./locales/${lang}.json`) ``, which Vite turns into a glob.

I'd do 1 and 2 first. Wasm is the best demo of why buck2 is under the dev server at all, and file-based routing exercises the weakest part of the setup.
