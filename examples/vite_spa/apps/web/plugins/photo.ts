// `import p from './tarn.jpg?photo'` gives a Photo: AVIF and WebP srcsets, a
// JPEG fallback, the intrinsic size and a tiny blurred placeholder.
//
// This plugin converts no images itself. It answers `?photo` with a generated
// module that imports the individual variants (`?w=…&format=…`) and leaves
// those to vite-imagetools, which owns the conversion, the dev serving and the
// emitted files. Only the placeholder and the dimensions are computed here.

import type { Plugin } from 'vite';

const PREFIX = '\0photo:';
const IMAGE_RE = /\.(jpe?g|png|webp)$/;
const WIDTHS = [400, 800, 1200];
const FALLBACK_WIDTH = 800;

async function describe(file: string) {
  // sharp is native; load it only when a photo is actually requested.
  const { default: sharp } = await import('sharp');
  const { width = 0, height = 0 } = await sharp(file).metadata();
  const tiny = await sharp(file).resize(24).blur(1).webp({ quality: 40 }).toBuffer();
  return { width, height, placeholder: `data:image/webp;base64,${tiny.toString('base64')}` };
}

export function photo(): Plugin {
  return {
    name: 'fieldnotes:photo',
    enforce: 'pre',

    resolveId: {
      filter: { id: /\?photo$/ },
      async handler(source, importer, options) {
        const path = source.slice(0, -'?photo'.length);
        if (!IMAGE_RE.test(path)) return;
        const resolved = await this.resolve(path, importer, { ...options, skipSelf: true });
        if (!resolved) return;
        return PREFIX + resolved.id;
      },
    },

    load: {
      filter: { id: /^\0photo:/ },
      async handler(id) {
        const file = id.slice(PREFIX.length);
        // The virtual module has no file of its own; tie it to the source image
        // so editing the image re-runs this load.
        this.addWatchFile(file);
        const { width, height, placeholder } = await describe(file);
        const widths = WIDTHS.filter((w) => w <= width);
        if (widths.length === 0) widths.push(width);
        const variant = (query: string) => JSON.stringify(`${file}?${query}`);
        return [
          `import avif from ${variant(`w=${widths.join(';')}&format=avif&as=srcset`)};`,
          `import webp from ${variant(`w=${widths.join(';')}&format=webp&as=srcset`)};`,
          `import src from ${variant(`w=${Math.min(FALLBACK_WIDTH, width)}&format=jpg`)};`,
          `export default {`,
          `  src,`,
          `  sources: [{ type: 'image/avif', srcset: avif }, { type: 'image/webp', srcset: webp }],`,
          `  width: ${width},`,
          `  height: ${height},`,
          `  placeholder: ${JSON.stringify(placeholder)},`,
          `};`,
        ].join('\n');
      },
    },
  };
}
