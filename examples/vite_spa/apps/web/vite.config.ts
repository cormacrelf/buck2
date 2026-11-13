import { fileURLToPath, URL } from 'node:url';
import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { defineConfig, loadEnv } from 'vite';
import { imagetools } from 'vite-imagetools';
import pkg from './package.json' with { type: 'json' };
import { buildInfo } from './plugins/build-info.ts';
import { markdown } from './plugins/markdown.ts';
import { photo } from './plugins/photo.ts';

// Shorthands for imagetools, so call sites say what an image is for rather than
// repeating widths and formats. `?preset=card` is used by the gallery glob.
const IMAGE_PRESETS: Record<string, Record<string, string>> = {
  card: { w: '360;720', format: 'avif;webp;jpg', as: 'picture' },
};

export default defineConfig(({ command, mode }) => {
  // Every variable, not only VITE_*, so the config can read build switches that
  // must never reach client code.
  const env = loadEnv(mode, process.cwd(), '');
  const debugPanel = command === 'serve' || env.FIELDNOTES_DEBUG_PANEL === '1';

  return {
    define: {
      __APP_VERSION__: JSON.stringify(pkg.version),
      // Folded to a literal, so the debug panel and its imports drop out of a
      // normal production bundle.
      __DEBUG_PANEL__: JSON.stringify(debugPanel),
    },
    resolve: {
      alias: {
        '@': fileURLToPath(new URL('./src', import.meta.url)),
      },
    },
    plugins: [
      buildInfo({
        preloadFonts: ['src/assets/fonts/ibm-plex-mono-latin-400-normal.woff2'],
      }),
      markdown(),
      // Before imagetools: it rewrites `?photo` into imports that imagetools serves.
      photo(),
      imagetools({
        defaultDirectives: (url) => {
          const preset = url.searchParams.get('preset');
          return new URLSearchParams(preset ? IMAGE_PRESETS[preset] : undefined);
        },
      }),
      react(),
      tailwindcss(),
    ],
    // Field recordings come off GPS units as .gpx; import them as asset URLs.
    assetsInclude: ['**/*.gpx'],
    css: {
      devSourcemap: true,
      modules: {
        localsConvention: 'camelCaseOnly',
        generateScopedName: mode === 'production' ? '[hash:base64:6]' : '[name]__[local]__[hash:base64:4]',
      },
      preprocessorOptions: {
        scss: {
          // Every .scss file sees the tokens without importing them.
          additionalData: '@use "@/styles/tokens" as *;\n',
        },
      },
    },
    worker: {
      format: 'es',
    },
    server: {
      headers: {
        'X-Content-Type-Options': 'nosniff',
      },
    },
    build: {
      target: 'es2022',
      sourcemap: 'hidden',
      assetsInlineLimit: 4096,
      rolldownOptions: {
        output: {
          codeSplitting: {
            groups: [
              { name: 'react', test: /node_modules[\\/](react|react-dom|scheduler|react-router)[\\/]/ },
            ],
          },
        },
      },
    },
  };
});
