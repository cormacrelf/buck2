// GPX recordings. `assetsInclude` in vite.config.ts makes a plain import of a
// .gpx file return its URL, like an image, so this eager glob is a map of
// name → hashed asset URL with no file contents in the bundle.
const trackUrls = import.meta.glob<string>('./tracks/*.gpx', {
  eager: true,
  import: 'default',
});

export interface TrackRef {
  name: string;
  url: string;
}

export const allTracks: readonly TrackRef[] = Object.entries(trackUrls)
  .map(([path, url]) => ({ name: path.replace(/^.*\/|\.gpx$/g, ''), url }))
  .sort((a, b) => a.name.localeCompare(b.name));

export function findTrack(name: string): TrackRef | undefined {
  return allTracks.find((t) => t.name === name);
}
