import { type DependencyList, useEffect, useState } from 'react';

export type AsyncState<T> =
  | { status: 'pending' }
  | { status: 'done'; value: T }
  | { status: 'error'; error: Error };

/** Runs `load` when `deps` change and tracks its result; stale results are dropped. */
export function useAsync<T>(load: () => Promise<T>, deps: DependencyList): AsyncState<T> {
  const [state, setState] = useState<AsyncState<T>>({ status: 'pending' });
  useEffect(() => {
    let current = true;
    setState({ status: 'pending' });
    load().then(
      (value) => current && setState({ status: 'done', value }),
      (error: unknown) =>
        current && setState({ status: 'error', error: error instanceof Error ? error : new Error(String(error)) }),
    );
    return () => {
      current = false;
    };
    // `load` is a new closure each render; the caller's deps decide reruns.
  }, deps);
  return state;
}
