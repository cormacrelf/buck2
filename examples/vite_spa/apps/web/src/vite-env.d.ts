// Types for everything Vite and the plugins in ../plugins add to the module
// system. `vite/client` (from tsconfig "types") covers ?raw, ?url, ?inline,
// ?worker, CSS modules, images and import.meta.env/hot/glob.

interface ViteTypeOptions {
  // Reject import.meta.env keys that aren't declared below.
  strictImportMetaEnv: unknown;
}

interface ImportMetaEnv {
  readonly VITE_APP_TITLE: string;
  readonly VITE_DEFAULT_LOCALE: string;
  readonly VITE_HOME_LAT: string;
  readonly VITE_HOME_LON: string;
  readonly VITE_RELEASE_CHANNEL: string;
  readonly VITE_UNITS?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

/** From `define` in vite.config.ts. */
declare const __APP_VERSION__: string;
declare const __DEBUG_PANEL__: boolean;

declare module 'virtual:build-info' {
  export const version: string;
  export const commit: string | null;
  export const builtAt: string;
  export const mode: string;
  export const channel: string;
  export const dependencies: Record<string, string>;
  const info: {
    version: string;
    commit: string | null;
    builtAt: string;
    mode: string;
    channel: string;
    dependencies: Record<string, string>;
  };
  export default info;
}

declare module '*.md' {
  import type { NoteDocument, NoteHeading, NoteMeta } from '@/content/types';
  export const meta: NoteMeta;
  export const html: string;
  export const headings: NoteHeading[];
  const doc: NoteDocument;
  export default doc;
}

declare module '*.md?meta' {
  import type { NoteMeta } from '@/content/types';
  export const meta: NoteMeta;
  export default meta;
}

declare module '*?photo' {
  const photo: import('@/lib/photo').Photo;
  export default photo;
}

declare module '*.gpx' {
  const url: string;
  export default url;
}

// vite-imagetools: the query decides the shape of the default export.
declare module '*&as=picture' {
  const picture: import('vite-imagetools').Picture;
  export default picture;
}

declare module '*&as=srcset' {
  const srcset: string;
  export default srcset;
}

declare module '*?preset=card' {
  const picture: import('vite-imagetools').Picture;
  export default picture;
}
