// Build metadata for the app.
//
// - `virtual:build-info` is a module that exists only inside the bundler. It
//   carries the app version, git commit, build time and a few dependency
//   versions, all decided when Vite starts.
// - The same record is served at /version.json by the dev server and emitted
//   as dist/version.json by a build, so a deployed copy can be identified
//   without loading the app.
// - transformIndexHtml stamps the commit into a <meta> tag and adds
//   <link rel="preload"> for the fonts the first paint needs. In a build the
//   href has to be the hashed output name, so that part reads the bundle.

import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, relative, resolve } from 'node:path';
import type { HtmlTagDescriptor, Plugin, ResolvedConfig } from 'vite';

const VIRTUAL_ID = 'virtual:build-info';
const RESOLVED_ID = `\0${VIRTUAL_ID}`;

export interface BuildInfo {
  version: string;
  commit: string | null;
  builtAt: string;
  mode: string;
  channel: string;
  dependencies: Record<string, string>;
}

export interface BuildInfoOptions {
  /** Font files, relative to the Vite root, to preload from index.html. */
  preloadFonts?: string[];
  /** Packages whose installed versions are reported. */
  reportDependencies?: string[];
}

function gitCommit(cwd: string): string | null {
  try {
    return execFileSync('git', ['rev-parse', '--short=12', 'HEAD'], {
      cwd,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim();
  } catch {
    return null;
  }
}

function installedVersion(root: string, name: string): string {
  const require = createRequire(join(root, 'package.json'));
  try {
    const manifest = require(`${name}/package.json`) as { version: string };
    return manifest.version;
  } catch {
    return 'unknown';
  }
}

function collect(config: ResolvedConfig, deps: string[]): BuildInfo {
  const pkg = JSON.parse(readFileSync(resolve(config.root, 'package.json'), 'utf8')) as {
    version: string;
  };
  // SOURCE_DATE_EPOCH pins the timestamp, so two builds of one commit agree.
  const epoch = process.env.SOURCE_DATE_EPOCH;
  const builtAt = epoch ? new Date(Number(epoch) * 1000) : new Date();
  return {
    version: pkg.version,
    commit: process.env.FIELDNOTES_COMMIT ?? gitCommit(config.root),
    builtAt: builtAt.toISOString(),
    mode: config.mode,
    channel: config.env.VITE_RELEASE_CHANNEL ?? 'local',
    dependencies: Object.fromEntries(deps.map((d) => [d, installedVersion(config.root, d)])),
  };
}

export function buildInfo(options: BuildInfoOptions = {}): Plugin {
  const { preloadFonts = [], reportDependencies = ['react', 'react-router', 'vite'] } = options;
  let config: ResolvedConfig;
  let info: BuildInfo;

  return {
    name: 'fieldnotes:build-info',

    configResolved(resolved) {
      config = resolved;
      info = collect(resolved, reportDependencies);
    },

    resolveId: {
      filter: { id: /^virtual:build-info$/ },
      handler(id) {
        return id === VIRTUAL_ID ? RESOLVED_ID : undefined;
      },
    },

    load: {
      filter: { id: /^\0virtual:build-info$/ },
      handler(id) {
        if (id !== RESOLVED_ID) return;
        const lines = Object.entries(info).map(
          ([key, value]) => `export const ${key} = ${JSON.stringify(value)};`,
        );
        lines.push(`export default { ${Object.keys(info).join(', ')} };`);
        return lines.join('\n');
      },
    },

    configureServer(server) {
      server.middlewares.use('/version.json', (req, res, next) => {
        if (req.method !== 'GET' && req.method !== 'HEAD') return next();
        res.setHeader('Content-Type', 'application/json');
        res.setHeader('Cache-Control', 'no-store');
        res.end(req.method === 'HEAD' ? undefined : JSON.stringify(info, null, 2));
      });
    },

    generateBundle() {
      this.emitFile({
        type: 'asset',
        fileName: 'version.json',
        source: `${JSON.stringify(info, null, 2)}\n`,
      });
    },

    transformIndexHtml: {
      order: 'post',
      handler(_html, ctx) {
        const tags: HtmlTagDescriptor[] = [
          {
            tag: 'meta',
            attrs: { name: 'fieldnotes-build', content: `${info.version}+${info.commit ?? 'unknown'}` },
            injectTo: 'head',
          },
        ];
        for (const font of preloadFonts) {
          let href: string | undefined;
          if (ctx.bundle) {
            // Find the hashed file the source font became.
            const abs = resolve(config.root, font);
            const asset = Object.values(ctx.bundle).find(
              (chunk) =>
                chunk.type === 'asset' &&
                chunk.originalFileNames.some((name) => resolve(config.root, name) === abs),
            );
            if (!asset) {
              config.logger.warn(`preload font ${font} is not in the bundle; is it referenced from CSS?`);
              continue;
            }
            href = config.base + asset.fileName;
          } else {
            href = config.base + relative(config.root, resolve(config.root, font));
          }
          tags.push({
            tag: 'link',
            attrs: { rel: 'preload', as: 'font', type: 'font/woff2', href, crossorigin: '' },
            injectTo: 'head',
          });
        }
        return tags;
      },
    },
  };
}
