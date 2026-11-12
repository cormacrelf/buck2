import type { LatLon } from './distance.js';

export interface TrackPoint extends LatLon {
  /** Elevation in metres, when the device recorded one. */
  ele?: number;
  /** Milliseconds since the epoch, when the device recorded a time. */
  time?: number;
}

export interface Track {
  name: string;
  points: TrackPoint[];
}

// GPX is parsed with a scanner rather than DOMParser so the same code runs in a
// Web Worker (which has no DOM) and in Node.
const TRKPT_RE = /<trkpt\b([^>]*)>([\s\S]*?)<\/trkpt>|<trkpt\b([^>]*)\/>/g;
const ATTR_RE = /(\w+)\s*=\s*"([^"]*)"/g;
const NAME_RE = /<trk>[\s\S]*?<name>([\s\S]*?)<\/name>/;

function attrs(source: string): Map<string, string> {
  const out = new Map<string, string>();
  for (const [, key, value] of source.matchAll(ATTR_RE)) {
    if (key !== undefined && value !== undefined) out.set(key, value);
  }
  return out;
}

function child(body: string, tag: string): string | undefined {
  const match = new RegExp(`<${tag}>([^<]*)</${tag}>`).exec(body);
  return match?.[1]?.trim();
}

export class GpxError extends Error {
  override name = 'GpxError';
}

export function parseGpx(xml: string): Track {
  if (!xml.includes('<gpx')) throw new GpxError('not a GPX document');
  const name = NAME_RE.exec(xml)?.[1]?.trim() ?? 'Untitled track';
  const points: TrackPoint[] = [];
  for (const match of xml.matchAll(TRKPT_RE)) {
    const attrSource = match[1] ?? match[3] ?? '';
    const body = match[2] ?? '';
    const a = attrs(attrSource);
    const lat = Number(a.get('lat'));
    const lon = Number(a.get('lon'));
    if (!Number.isFinite(lat) || !Number.isFinite(lon)) {
      throw new GpxError(`track point ${points.length} has no usable lat/lon`);
    }
    const point: TrackPoint = { lat, lon };
    const ele = child(body, 'ele');
    if (ele !== undefined) point.ele = Number(ele);
    const time = child(body, 'time');
    if (time !== undefined) point.time = Date.parse(time);
    points.push(point);
  }
  return { name, points };
}
