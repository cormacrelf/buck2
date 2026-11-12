import { type Bounds, boundsOf, haversine } from './distance.js';
import type { Track, TrackPoint } from './gpx.js';

export interface TrackStats {
  name: string;
  pointCount: number;
  distanceM: number;
  /** Total climb, ignoring wiggles smaller than the hysteresis threshold. */
  ascentM: number;
  descentM: number;
  minEleM?: number;
  maxEleM?: number;
  durationS?: number;
  movingS?: number;
  bounds?: Bounds;
  /** Distance/elevation pairs, downsampled for a profile chart. */
  profile: Array<[distanceM: number, eleM: number]>;
}

export interface StatsOptions {
  /** Elevation change below this is treated as GPS noise. */
  hysteresisM?: number;
  /** Segments slower than this count as stopped. */
  movingThresholdMps?: number;
  /** Upper bound on profile samples. */
  profileSamples?: number;
}

function elevationTotals(points: readonly TrackPoint[], hysteresis: number) {
  let ascent = 0;
  let descent = 0;
  let anchor: number | undefined;
  for (const { ele } of points) {
    if (ele === undefined) continue;
    if (anchor === undefined) {
      anchor = ele;
      continue;
    }
    const delta = ele - anchor;
    if (delta >= hysteresis) {
      ascent += delta;
      anchor = ele;
    } else if (delta <= -hysteresis) {
      descent -= delta;
      anchor = ele;
    }
  }
  return { ascent, descent };
}

export function computeStats(track: Track, options: StatsOptions = {}): TrackStats {
  const { hysteresisM = 3, movingThresholdMps = 0.4, profileSamples = 120 } = options;
  const { points } = track;
  let distance = 0;
  let moving = 0;
  const cumulative: number[] = [0];
  for (let i = 1; i < points.length; i++) {
    const prev = points[i - 1]!;
    const here = points[i]!;
    const step = haversine(prev, here);
    distance += step;
    cumulative.push(distance);
    if (prev.time !== undefined && here.time !== undefined) {
      const seconds = (here.time - prev.time) / 1000;
      if (seconds > 0 && step / seconds >= movingThresholdMps) moving += seconds;
    }
  }

  const elevations = points.flatMap((p) => (p.ele === undefined ? [] : [p.ele]));
  const { ascent, descent } = elevationTotals(points, hysteresisM);
  const first = points[0]?.time;
  const last = points.at(-1)?.time;

  const stride = Math.max(1, Math.ceil(points.length / profileSamples));
  const profile: TrackStats['profile'] = [];
  for (let i = 0; i < points.length; i += stride) {
    const ele = points[i]!.ele;
    if (ele !== undefined) profile.push([cumulative[i]!, ele]);
  }

  const stats: TrackStats = {
    name: track.name,
    pointCount: points.length,
    distanceM: distance,
    ascentM: ascent,
    descentM: descent,
    profile,
  };
  if (elevations.length > 0) {
    stats.minEleM = Math.min(...elevations);
    stats.maxEleM = Math.max(...elevations);
  }
  if (first !== undefined && last !== undefined) {
    stats.durationS = (last - first) / 1000;
    stats.movingS = moving;
  }
  const bounds = boundsOf(points);
  if (bounds) stats.bounds = bounds;
  return stats;
}
