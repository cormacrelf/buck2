import type { TrackStats } from '@fieldnotes/geo';

export interface StatsRequest {
  id: number;
  /** Absolute URL of a GPX file. */
  url: string;
}

export type StatsResponse =
  | { id: number; ok: true; stats: TrackStats }
  | { id: number; ok: false; error: string };
