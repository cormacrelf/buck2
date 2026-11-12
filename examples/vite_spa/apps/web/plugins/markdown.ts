// Turns field notes written in Markdown into JavaScript modules.
//
//   import { meta, html, headings } from './notes/tarn.md';
//   import { meta } from './notes/tarn.md?meta';
//
// The `?meta` form carries only the front matter, so an index page can import
// every note eagerly without pulling every body into the main chunk.
//
// HMR: when a note's text changes, the rendered document is pushed to the
// browser as a `fieldnotes:note-update` event and the page swaps it in without
// re-importing anything. The `?meta` modules still go through ordinary HMR, so
// lists of notes refresh too.

import { basename } from 'node:path';
import { Marked, type Tokens } from 'marked';
import type { Plugin } from 'vite';
import type { NoteDocument, NoteHeading, NoteMeta } from '../src/content/types.ts';

const MD_RE = /\.md(\?meta)?$/;
const FRONT_MATTER_RE = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/;

class NoteError extends Error {}

// A deliberately small YAML subset: `key: value`, `key: [a, b]`, quoted
// strings. Notes are written by hand and this keeps the parser obvious.
function parseFrontMatter(source: string): Record<string, string | string[]> {
  const out: Record<string, string | string[]> = {};
  for (const [i, raw] of source.split(/\r?\n/).entries()) {
    const line = raw.trim();
    if (line === '' || line.startsWith('#')) continue;
    const colon = line.indexOf(':');
    if (colon < 1) throw new NoteError(`front matter line ${i + 1}: expected "key: value"`);
    const key = line.slice(0, colon).trim();
    let value = line.slice(colon + 1).trim();
    if (value.startsWith('[') && value.endsWith(']')) {
      out[key] = value
        .slice(1, -1)
        .split(',')
        .map((v) => unquote(v.trim()))
        .filter((v) => v !== '');
    } else {
      value = unquote(value);
      out[key] = value;
    }
  }
  return out;
}

function unquote(value: string): string {
  return /^(['"]).*\1$/.test(value) ? value.slice(1, -1) : value;
}

function slugify(text: string): string {
  return text
    .toLowerCase()
    .replace(/<[^>]+>/g, '')
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-|-$/g, '');
}

function asString(fm: Record<string, string | string[]>, key: string): string | undefined {
  const value = fm[key];
  if (Array.isArray(value)) throw new NoteError(`front matter "${key}" must be a single value`);
  return value;
}

function asList(fm: Record<string, string | string[]>, key: string): string[] {
  const value = fm[key];
  if (value === undefined) return [];
  return Array.isArray(value) ? value : [value];
}

export function renderNote(source: string, file: string): NoteDocument {
  const match = FRONT_MATTER_RE.exec(source);
  if (!match) throw new NoteError('missing front matter');
  const fm = parseFrontMatter(match[1] ?? '');
  const body = source.slice(match[0].length);

  const title = asString(fm, 'title');
  const date = asString(fm, 'date');
  if (!title) throw new NoteError('front matter needs a "title"');
  if (!date || !/^\d{4}-\d{2}-\d{2}$/.test(date)) {
    throw new NoteError('front matter needs a "date" as YYYY-MM-DD');
  }

  const words = body.split(/\s+/).filter(Boolean).length;
  const meta: NoteMeta = {
    slug: basename(file, '.md'),
    title,
    date,
    tags: asList(fm, 'tags'),
    taxa: asList(fm, 'taxa'),
    readingMinutes: Math.max(1, Math.round(words / 200)),
  };
  const photo = asString(fm, 'photo');
  if (photo) meta.photo = photo;
  const track = asString(fm, 'track');
  if (track) meta.track = track;

  const headings: NoteHeading[] = [];
  const marked = new Marked({
    gfm: true,
    renderer: {
      heading(token: Tokens.Heading) {
        const inner = this.parser.parseInline(token.tokens);
        const id = slugify(token.text);
        headings.push({ depth: token.depth, id, text: token.text });
        return `<h${token.depth} id="${id}">${inner}</h${token.depth}>\n`;
      },
    },
  });
  const html = marked.parse(body, { async: false });
  return { meta, html, headings };
}

export function markdown(): Plugin {
  return {
    name: 'fieldnotes:markdown',

    transform: {
      filter: { id: MD_RE },
      handler(source, id) {
        const [file = id, query] = id.split('?');
        let doc: NoteDocument;
        try {
          doc = renderNote(source, file);
        } catch (err) {
          if (err instanceof NoteError) this.error(`${file}: ${err.message}`);
          throw err;
        }
        const code =
          query === 'meta'
            ? `export const meta = ${JSON.stringify(doc.meta)};\nexport default meta;\n`
            : [
                `export const meta = ${JSON.stringify(doc.meta)};`,
                `export const html = ${JSON.stringify(doc.html)};`,
                `export const headings = ${JSON.stringify(doc.headings)};`,
                'export default { meta, html, headings };',
                '',
              ].join('\n');
        return { code, map: { mappings: '' } };
      },
    },

    async hotUpdate({ type, file, modules, read, timestamp }) {
      if (type !== 'update' || !file.endsWith('.md') || this.environment.name !== 'client') return;
      let doc: NoteDocument;
      try {
        doc = renderNote(await read(), file);
      } catch {
        // Let the normal path run the transform and report the error overlay.
        return;
      }
      // The full module now holds stale text. Invalidate it so the next fresh
      // import (a reload, or a navigation in a new tab) gets the new body.
      const seen = new Set<(typeof modules)[number]>();
      for (const mod of modules) {
        if (!mod.id?.endsWith('?meta')) {
          this.environment.moduleGraph.invalidateModule(mod, seen, timestamp, true);
        }
      }
      this.environment.hot.send({
        type: 'custom',
        event: 'fieldnotes:note-update',
        data: doc,
      });
      return modules.filter((mod) => mod.id?.endsWith('?meta'));
    },
  };
}
