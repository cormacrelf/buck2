import type { TrackStats } from '@fieldnotes/geo';
import type { StatsRequest, StatsResponse } from './protocol.ts';
import TrackStatsWorker from './trackStats.worker.ts?worker';

let worker: Worker | undefined;
let nextId = 0;
const pending = new Map<number, { resolve(s: TrackStats): void; reject(e: Error): void }>();

function getWorker(): Worker {
  if (!worker) {
    worker = new TrackStatsWorker({ name: 'track-stats' });
    worker.addEventListener('message', (event: MessageEvent<StatsResponse>) => {
      const response = event.data;
      const waiter = pending.get(response.id);
      if (!waiter) return;
      pending.delete(response.id);
      if (response.ok) waiter.resolve(response.stats);
      else waiter.reject(new Error(response.error));
    });
  }
  return worker;
}

export function trackStats(url: string): Promise<TrackStats> {
  const id = nextId++;
  // The worker resolves URLs against its own script URL; make it absolute.
  const request: StatsRequest = { id, url: new URL(url, location.href).href };
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    getWorker().postMessage(request);
  });
}
