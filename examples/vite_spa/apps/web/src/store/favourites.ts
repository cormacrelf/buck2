// Starred species, persisted to localStorage.
//
// A tiny external store read with useSyncExternalStore. When this file is
// edited, the new module takes over the old one's starred set through
// import.meta.hot.data, so a store edit doesn't wipe the user's state the way
// a full reload would.

import { useSyncExternalStore } from 'react';

const STORAGE_KEY = 'fieldnotes:favourites';

type Listener = () => void;

interface State {
  starred: ReadonlySet<string>;
}

function readStorage(): ReadonlySet<string> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    return new Set(Array.isArray(parsed) ? parsed.filter((v) => typeof v === 'string') : []);
  } catch {
    return new Set();
  }
}

let state: State = (import.meta.hot?.data.state as State | undefined) ?? { starred: readStorage() };
const listeners = new Set<Listener>();

function setState(next: State): void {
  state = next;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify([...next.starred]));
  } catch {
    // Not persisted; the in-memory state still works.
  }
  for (const listener of listeners) listener();
}

function subscribe(listener: Listener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function toggleFavourite(id: string): void {
  const starred = new Set(state.starred);
  if (!starred.delete(id)) starred.add(id);
  setState({ starred });
}

export function useFavourites(): ReadonlySet<string> {
  return useSyncExternalStore(subscribe, () => state.starred);
}

export function useIsFavourite(id: string): boolean {
  return useSyncExternalStore(subscribe, () => state.starred.has(id));
}

// No accept() here: the update goes on to the components that import this
// module, Fast Refresh re-runs them against the new exports, and the new
// module starts from the state handed over below.
if (import.meta.hot) {
  import.meta.hot.dispose((data) => {
    data.state = state;
  });
}
