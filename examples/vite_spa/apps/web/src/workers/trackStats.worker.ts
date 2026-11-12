// Parses GPX and computes track statistics off the main thread. Imported by
// trackStatsClient.ts with `?worker`, so Vite bundles it (and the compiled
// @fieldnotes/geo it imports) as a separate worker chunk.
import { computeStats, GpxError, parseGpx } from '@fieldnotes/geo';
import type { StatsRequest, StatsResponse } from './protocol.ts';

const cache = new Map<string, Promise<StatsResponse>>();

async function handle({ id, url }: StatsRequest): Promise<StatsResponse> {
  try {
    const res = await fetch(url);
    if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
    const stats = computeStats(parseGpx(await res.text()));
    return { id, ok: true, stats };
  } catch (err) {
    const error = err instanceof GpxError ? `bad GPX: ${err.message}` : String(err);
    return { id, ok: false, error };
  }
}

self.addEventListener('message', (event: MessageEvent<StatsRequest>) => {
  const { id, url } = event.data;
  let pending = cache.get(url);
  if (!pending) {
    pending = handle({ id, url });
    cache.set(url, pending);
  }
  pending.then((response) => postMessage({ ...response, id }));
});
