// Shapes produced by plugins/markdown.ts. Shared with the plugin, which
// imports these types, so the build side and the app agree.

export interface NoteMeta {
  slug: string;
  title: string;
  /** ISO date, YYYY-MM-DD. */
  date: string;
  tags: string[];
  /** Base name of a photo in src/assets/photos, without extension. */
  photo?: string;
  /** Base name of a track in src/content/tracks, without extension. */
  track?: string;
  /** Ids from @fieldnotes/taxa. Checked when the note is shown, not at build. */
  taxa: string[];
  readingMinutes: number;
}

export interface NoteHeading {
  depth: number;
  id: string;
  text: string;
}

export interface NoteDocument {
  meta: NoteMeta;
  html: string;
  headings: NoteHeading[];
}
