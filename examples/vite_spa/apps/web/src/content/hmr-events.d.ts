// Types the custom HMR event sent by plugins/markdown.ts, so
// import.meta.hot.on('fieldnotes:note-update', …) gets a typed payload.
import 'vite/types/customEvent.d.ts';
import type { NoteDocument } from './types.ts';

declare module 'vite/types/customEvent.d.ts' {
  interface CustomEventMap {
    'fieldnotes:note-update': NoteDocument;
  }
}
