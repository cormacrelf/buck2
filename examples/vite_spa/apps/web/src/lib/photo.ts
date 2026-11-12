/** What a `?photo` import resolves to (see plugins/photo.ts). */
export interface Photo {
  /** JPEG fallback for browsers that take neither source. */
  src: string;
  sources: Array<{ type: string; srcset: string }>;
  width: number;
  height: number;
  /** A ~24px blurred WebP as a data: URL, shown while the real image loads. */
  placeholder: string;
}

// Lazily loaded so a note page fetches only the photo it shows. Each entry
// is a chunk holding the generated module, which in turn imports the
// imagetools variants.
const photoLoaders = import.meta.glob<Photo>('../assets/photos/*.jpg', {
  query: '?photo',
  import: 'default',
});

export function hasPhoto(name: string): boolean {
  return `../assets/photos/${name}.jpg` in photoLoaders;
}

export async function loadPhoto(name: string): Promise<Photo | undefined> {
  const load = photoLoaders[`../assets/photos/${name}.jpg`];
  return load ? load() : undefined;
}
