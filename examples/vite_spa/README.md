# Vite SPA example

A small React app (a field-notes journal: notes, photos, GPS tracks, a species
checklist) built with Vite and pnpm. It exists to be a realistic test case for
building and serving a Vite app with Buck2, so it uses the Vite features that
larger apps depend on rather than the minimum needed to render a page.

There are no Buck files here yet. Everything below runs with pnpm and Vite.

## Layout

```
pnpm-workspace.yaml     workspace + version catalog
apps/web/               the app
  index.html            entry HTML
  vite.config.ts        plugins, alias, CSS, worker, build options
  plugins/              three Vite plugins written for this app
  public/               copied to dist/ as-is
  src/                  React + TypeScript source, content, assets
packages/ui/            React components, consumed as TypeScript source
packages/geo/           GPX parsing and distance maths, compiled with tsc to dist/
packages/taxa/          species list; TypeScript generated from taxa.json
packages/terrain/       heightmap for the Terrain page; TypeScript generated from terrain.json
```

## Running it

Needs Node 24 and pnpm 11. From this directory:

```
pnpm install
pnpm dev          # generate taxa, build geo, then tsc --watch for geo alongside vite
pnpm build        # generate taxa, build geo, typecheck the app, vite build -> apps/web/dist
pnpm typecheck    # generate taxa, build geo, tsc --noEmit in every package
```

`apps/web` and `packages/ui` can't be typechecked or bundled until
`packages/taxa/src/generated/` and `packages/geo/dist/` exist. Both are
gitignored; `build:packages` makes them.

Versions at the time of writing: Vite 8.3 (Rolldown and Oxc), React 19.3,
react-router 8.5, TypeScript 7.0, vite-imagetools 12, Sass 1.108.

## Features and where they are

The right-hand column is what a Buck2 build would have to model to do that
piece without Vite.

| Feature | Exercised in | What Buck2 would have to model |
| --- | --- | --- |
| TS/TSX transpile with React Fast Refresh | `apps/web/src/**/*.tsx`, `@vitejs/plugin-react` in `vite.config.ts` | Per-file transpile action (TS strip, JSX, refresh registration in dev). Dev HTML also needs the refresh preamble that the plugin injects. |
| Strict typecheck, separate from transpile | `apps/web/tsconfig.json`, `tsconfig.node.json`, `tsc --noEmit` in `build` | A whole-program typecheck action that sees `packages/ui` source, `packages/geo/dist/*.d.ts` and the generated taxa file. Not per-file. |
| Path alias `@` → `src`, mirrored in tsconfig `paths` | `vite.config.ts` `resolve.alias`, `tsconfig.json` | Import resolution that applies the alias, in JS and in Sass (`additionalData` uses `@/styles/tokens`). |
| Route splitting with `React.lazy` + `import()` | `src/App.tsx` | Chunk graph in the bundle action; one chunk per page plus shared chunks. |
| Dynamic import with a variable | `src/i18n/locales.ts` (``import(`./messages/${locale}.json`)``) | Expanding `./messages/*.json` at build time: the importing file's output depends on a directory listing. |
| `import.meta.glob`, eager with `query`/`import` | `src/content/notes.ts` (`?meta`), `src/content/tracks.ts`, `src/pages/GalleryPage.tsx` (`query: { preset }`) | Same as above: transform output depends on which files match the glob, so adding a file changes it. |
| `import.meta.glob`, lazy | `src/content/notes.ts` (note bodies), `src/lib/photo.ts` (`?photo`) | A chunk per matched file, each produced by whatever plugin handles that query. |
| JSON import with named exports | `src/pages/HomePage.tsx` (`{ sites }` from `src/data/sites.json`), `src/i18n/messages/*.json` | JSON→ESM transform action. |
| Global CSS | `src/styles/global.css`, `packages/ui/src/tokens.css` | CSS transform action and CSS bundling in import order. |
| CSS from npm with font files | `src/main.tsx` (`@fontsource/literata/latin-*.css`) | Resolving CSS and `url()` targets inside `node_modules`; the .woff/.woff2 become hashed assets. |
| CSS modules (`.module.css`, `.module.scss`) | `src/components/*.module.*`, `src/pages/*.module.*`, `packages/ui/src/*.module.css` | Per-file action emitting scoped CSS and a JS class-map module. Scoped names depend on path and mode (`css.modules.generateScopedName`, `localsConvention: camelCaseOnly`). |
| Sass with injected `additionalData` | `vite.config.ts`, `src/styles/_tokens.scss`, every `.scss` file | Every `.scss` action depends on `_tokens.scss` without importing it. |
| PostCSS config | `apps/web/postcss.config.js` (autoprefixer), `browserslist` in `apps/web/package.json` | Config discovered from the project root; every CSS action depends on it and on the browserslist data. |
| `url()` in CSS | `src/components/Layout.module.scss` (SVG, under 4 KiB so inlined), `src/styles/fonts.scss` (woff2, emitted) | Asset pipeline action; inline-vs-emit is decided by size (`build.assetsInlineLimit`). |
| Image import as URL | `src/components/Layout.tsx` (`compass.svg`) | Asset copy + content hash; a JS module exporting the URL. |
| `?raw` and `?url` on one file | `src/pages/AboutPage.tsx` (`ibm-plex-mono-OFL.txt`) | Two different modules from one input, keyed by query. |
| `?inline` | `src/pages/HomePage.tsx` (`paper-grain.png?inline`) | A module exporting a `data:` URL. |
| `?raw` SVG icons in a library | `packages/ui/src/icons/index.ts` | As `?raw`, in a package compiled as part of the app. |
| `new URL('…', import.meta.url)` | `src/pages/TracksPage.tsx` (`marker.svg`) | Finding asset references by scanning JS, not only imports. |
| Local font file | `src/styles/fonts.scss`, `src/assets/fonts/` | Asset action, plus the HTML preload below needing its hashed name. |
| `public/` | `apps/web/public/` | Verbatim copy to the output root, unhashed; dev serves it at `/`. |
| Custom asset type | `assetsInclude: ['**/*.gpx']` in `vite.config.ts`, `src/content/tracks.ts` | Treat `.gpx` as an asset (URL export) instead of trying to parse it as JS. |
| Responsive images, `as=picture` | `src/pages/HomePage.tsx` (`?w=480;800;1200&format=avif;webp;jpg&as=picture`) | An image-transform action per (image, width, format); a module listing the outputs. Dev serves them at `/@imagetools/<hash>`. |
| imagetools preset via `defaultDirectives` | `vite.config.ts` (`IMAGE_PRESETS`), `src/pages/GalleryPage.tsx` | Directive expansion from config before the image actions run. |
| Virtual module | `plugins/build-info.ts` (`virtual:build-info`), used by `src/components/BuildBadge.tsx`, `src/pages/AboutPage.tsx` | A generated module as a build output. Inputs: `package.json`, git HEAD, installed dependency versions, `SOURCE_DATE_EPOCH` (otherwise the time, so not reproducible). |
| Dev middleware + `generateBundle` asset | `plugins/build-info.ts` (`/version.json`) | A route in the dev server and an extra file in the output. |
| `transformIndexHtml` reading the bundle | `plugins/build-info.ts` (font preload, build meta tag) | The HTML action runs after bundling and depends on hashed asset names. |
| `%VITE_*%` in HTML | `apps/web/index.html` | Env substitution into the HTML. |
| `transform` hook for a new file type | `plugins/markdown.ts` (`.md`, `.md?meta`) | Per-file, per-query transform action (uses `marked`); validation errors fail the build. |
| Custom HMR (`hotUpdate`, custom event, `import.meta.hot.on`, `hot.data`) | `plugins/markdown.ts`, `src/content/notes.ts`, `src/content/hmr-events.d.ts` | Dev server re-renders the note and pushes it to the page; only `?meta` modules go through normal HMR. |
| Plugin that generates imports for another plugin | `plugins/photo.ts` (`?photo`, `addWatchFile`, sharp metadata + placeholder) | A generated module whose dependencies (the imagetools variants) are only known after running it. |
| Env files and modes | `.env`, `.env.development`, `.env.production`, `src/env.ts`, `src/vite-env.d.ts` | Mode picks the files; each `import.meta.env.VITE_*` read is replaced in every module that reads it. |
| `loadEnv` in config | `vite.config.ts` (`FIELDNOTES_DEBUG_PANEL`) | Config evaluation depends on env files and the process environment. |
| `define` | `vite.config.ts` (`__APP_VERSION__`, `__DEBUG_PANEL__`), `src/env.ts`, `src/pages/AboutPage.tsx` | Global constant replacement in every module; with `__DEBUG_PANEL__` false the `DebugPanel` chunk is not emitted. |
| Web Worker via `?worker` | `src/workers/trackStatsClient.ts`, `src/workers/trackStats.worker.ts` | A second bundle with its own entry and module graph (it pulls in `@fieldnotes/geo`). `worker.format: 'es'`. |
| Package consumed from source | `packages/ui` (`exports` → `./src/*.ts`) | Its TSX and CSS modules are transpiled as app sources. |
| Package that must be compiled first | `packages/geo` (`tsc` → `dist/` with `.d.ts`, `exports` → `dist`) | A tsc action before anything that imports it. In dev, Vite serves `dist/` through `/@fs/`; editing `src/` does nothing until tsc reruns, and a change to `dist/` triggers a full reload. |
| Generated package | `packages/taxa` (`scripts/generate.mjs` turns `taxa.json` into `src/generated/taxa.ts`) | A codegen action whose output is consumed as source by the app and its typecheck. |
| Fast Refresh vs full reload | Components (state kept) vs `src/featureFlags.ts` (imported by `main.tsx`, which accepts nothing) | Import-graph propagation to the nearest HMR boundary, or a full reload when there is none. |
| Store that hands state to its replacement | `src/store/favourites.ts` (`hot.dispose` + `hot.data`) | The dev server keeps module instances and runs dispose/accept in order. |
| Manual chunk group | `vite.config.ts` (`rolldownOptions.output.codeSplitting`, `react` group) | Chunk assignment rules in the bundle action. |
| Hidden source maps | `build.sourcemap: 'hidden'` | Maps emitted beside chunks with no `sourceMappingURL` comment. |
| Dependency pre-bundling (dev only) | implicit: `react`, `react-dom`, `react-router` | Dev server serves CJS packages as ESM from `node_modules/.vite/deps`; a Buck dev server needs an equivalent. |
| pnpm `catalog:` and `workspace:` specifiers | `pnpm-workspace.yaml`, every `package.json` | Reading dependency versions from the catalog and linking workspace packages. |

## HMR behaviour

Checked against the dev server's HMR socket:

- Editing a component (`src/pages/HomePage.tsx`) sends a `js-update` for that file.
- Editing a note (`src/content/notes/*.md`) sends a `fieldnotes:note-update`
  custom event with the re-rendered note, then `js-update`s for the pages that
  list notes.
- Editing `src/featureFlags.ts` sends `full-reload`.
- Editing `src/store/favourites.ts` sends `js-update`s for the components that
  import it, and the starred set survives.
- Editing `packages/geo/src/*.ts` sends nothing. Editing `packages/geo/dist/*.js`
  (what `tsc --watch` does in `pnpm dev`) sends `full-reload`.

## Not covered

Some things large Vite apps also do that this example leaves out: Tailwind or
other utility-CSS plugins, file-based route generation, Babel plugins such as
the React Compiler, WebAssembly, `server.proxy`, and tests (Vitest).

## With buck2

The same app also runs under `buck2 dev`, with buck2 compiling every module:

```sh
buck2 dev vite/dev.bxl:dev -- --app //apps/web:web
```

Open <http://localhost:5173/> (or set `PORT`). It needs `node` on `PATH` and network
access the first time, to fetch the npm packages in `pnpm-lock.yaml`. It does not need
`pnpm install`, `pnpm build:packages` or `tsc --watch`.

### What buck2 does and what Vite does

Vite still serves the page. It runs its HMR client and module graph, rewrites imports for
HMR (`?t=`, `import.meta.hot`), injects `import.meta.env`, and fills in `%VITE_*%`. It
compiles nothing and watches nothing: oxc, the dependency optimizer and the file watcher are
turned off.

Every module is a buck2 action, run when the browser first asks for the module and cached
after that:

- **Sources**: `vite/tools/transform.mjs` compiles one file with oxc-transform
  (TypeScript, JSX, React refresh) and resolves its imports to the URLs that serve them.
  Each `?raw`, `?url`, `?inline` and `?worker` import is a module of its own.
- **Stylesheets**: Sass, then lightningcss for CSS modules, `url()`s and the
  `browserslist` prefixes that autoprefixer adds under Vite.
- **`import.meta.glob`** and `import(\`./messages/${locale}.json\`)` are expanded against
  the package's sources, which BUCK globs. A new note changes the glob, and the module that
  globs it is built again.
- **Images**: `?w=…&format=…&as=picture` and `?preset=card` are modules that only name their
  images, at `/@image/…` URLs, with the sizes that sharp will give them. The bxl handler
  declares the module's action when the query is first asked for, since only the importer
  knows it. Each image is resized and encoded with sharp in an action of its own when the
  browser asks for its URL, as vite-imagetools does in dev, so only the images a page shows
  are encoded, and not before the page's modules are built.
- **npm packages**: `vite_npm` pre-bundles each store entry with esbuild, one action per
  package, with its dependencies left as imports of their own bundles. Vite's optimizer
  bundles every dependency together, and does it again when one is added. Stylesheets in npm
  packages (`@fontsource/literata`) become modules too.
- **`packages/geo`** is compiled from source per file, as the app's modules are, so an edit
  there reaches the page without `tsc` writing `dist/`.
- **`packages/taxa`**: `scripts/generate.mjs` runs as a genrule, and an edit to
  `taxa.json` is an HMR update of the pages that use it.
- **`packages/terrain`** shows that buck2 builds only what the open page needs. Its genrule is
  made to take five seconds, and only the Terrain page imports the package and the d3 packages
  it draws with. Nothing waits for any of it until the browser opens that page, and the page
  shows how long it waited. A second visit is instant. Change `seed` in `terrain.json` to wait
  again, as an HMR update.

The app's own plugins run unmodified. Their build hooks run inside the actions: `markdown`
for `.md` and `photo` for `.jpg`, as listed in `apps/web/BUCK`. Their server hooks run in
the dev server: `buildInfo`'s `/version.json` and its `index.html` tags, and `markdown`'s
`hotUpdate`, which sends `fieldnotes:note-update`. `virtual:build-info` is the exception. Its
`load` reads git and the clock, which a cached action must not, so `apps/web/BUCK` builds it
from the lockfile and `package.json` instead.

`vite.config.ts` is not read. What it configures is said again in `apps/web/BUCK` and
`apps/web/vite.bzl`: aliases, `define`, the CSS module naming, Sass `additionalData`, the
imagetools preset.

### Changes, as measured

With `buck2 dev --debounce-ms 10`, from saving a file to the HMR message on Vite's
websocket:

| Edit | Result | Time |
| --- | --- | --- |
| A component | `js-update` of it | ~115ms |
| `packages/ui` component or CSS module | `js-update` of the component | ~120ms |
| `taxa.json` | `js-update` of the two pages using it | ~140ms |
| A note's body | the plugin's `fieldnotes:note-update` event, no module update | ~145ms |
| A note's title | the event, and `js-update`s of the lists | ~140ms |
| `packages/geo/src` | `full-reload` (the worker imports it, as Vite does for `dist/`) | ~115ms |
| `featureFlags.ts` | `full-reload` | ~125ms |
| `_tokens.scss` | every `.scss` module rebuilt; `js-update` only of `NotePage`, whose CSS changed | ~1.6s |
| Whitespace in a CSS module | nothing: the compiled module is the same | |

The first load of all 138 modules builds everything and takes about 8 seconds. After that,
a reload takes about 100ms.

### Limits

- The transforms run in a persistent worker (`vite.persistent_worker` in `.buckconfig`),
  which `buck2 dev` keeps for the session, so oxc, lightningcss and Sass load once. The
  images that image modules name are encoded by a new `node` each, outside the worker.
- Each module is a dynamic output of `buck2 dev`, and each change is looked up in each one's
  graph, so the time per change grows with the modules loaded: about 70ms with 30 modules
  in `examples/dev_vite_buck`, against about 115ms here.
- The modules the handler declares (images, `?photo`, `?meta`) count as built from every
  source of their package, so any edit in `apps/web` builds them again. They come back the
  same, so nothing reaches the page.
- There is no production build through these modules yet; `pnpm build` is still Vite's.
