import { useSyncExternalStore } from 'react';
import type { NoteDocument, NoteMeta } from './types.ts';

// Front matter of every note, bundled into the importing chunk. These are the
// small `?meta` modules, so listing notes costs no note bodies.
const metaModules = import.meta.glob<NoteMeta>('./notes/*.md', {
  eager: true,
  query: '?meta',
  import: 'default',
});

// Full documents, one chunk per note, fetched when a note is opened.
const documentLoaders = import.meta.glob<NoteDocument>('./notes/*.md', {
  import: 'default',
});

export const allNotes: readonly NoteMeta[] = Object.values(metaModules).sort((a, b) =>
  b.date.localeCompare(a.date),
);

export const allTags: readonly string[] = [...new Set(allNotes.flatMap((n) => n.tags))].sort();

export function findNote(slug: string): NoteMeta | undefined {
  return allNotes.find((n) => n.slug === slug);
}

export async function loadNote(slug: string): Promise<NoteDocument | undefined> {
  const load = documentLoaders[`./notes/${slug}.md`];
  return load ? load() : undefined;
}

// --- Dev-only live editing -------------------------------------------------
//
// plugins/markdown.ts pushes a re-rendered note over the HMR socket whenever a
// .md file is saved. The browser's module cache still holds the old module,
// so the latest version of each edited note is kept here and preferred.
//
// This module is itself hot-updated when a `?meta` module changes, so the map
// lives in import.meta.hot.data to survive being re-run.

type Edited = Map<string, NoteDocument>;
const edited: Edited = (import.meta.hot?.data.edited as Edited | undefined) ?? new Map();
const listeners = new Set<() => void>();
let version = 0;

if (import.meta.hot) {
  import.meta.hot.data.edited = edited;
  import.meta.hot.on('fieldnotes:note-update', (doc) => {
    edited.set(doc.meta.slug, doc);
    version++;
    for (const listener of listeners) listener();
  });
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** The pushed version of a note, if it has been edited since page load. */
export function useLiveNote(slug: string): NoteDocument | undefined {
  useSyncExternalStore(subscribe, () => version);
  return edited.get(slug);
}
