import { formatDistance, formatElevation, type TrackStats, type UnitSystem } from '@fieldnotes/geo';
import styles from './ElevationProfile.module.css';

const W = 600;
const H = 140;
const PAD = 4;

export function ElevationProfile({ stats, units }: { stats: TrackStats; units: UnitSystem }) {
  const { profile, minEleM, maxEleM, distanceM } = stats;
  if (profile.length < 2 || minEleM === undefined || maxEleM === undefined) return null;
  const span = Math.max(1, maxEleM - minEleM);
  const x = (d: number) => PAD + (d / distanceM) * (W - 2 * PAD);
  const y = (e: number) => H - PAD - ((e - minEleM) / span) * (H - 2 * PAD);
  const line = profile.map(([d, e], i) => `${i === 0 ? 'M' : 'L'}${x(d).toFixed(1)},${y(e).toFixed(1)}`).join('');
  const area = `${line}L${x(distanceM).toFixed(1)},${H}L${PAD},${H}Z`;

  return (
    <figure className={styles.figure}>
      <svg viewBox={`0 0 ${W} ${H}`} className={styles.chart} role="img" aria-label="Elevation profile">
        <path d={area} className={styles.area} />
        <path d={line} className={styles.line} />
      </svg>
      <figcaption className={styles.caption}>
        <span>{formatElevation(minEleM, units)}</span>
        <span>{formatDistance(distanceM, units)}</span>
        <span>{formatElevation(maxEleM, units)}</span>
      </figcaption>
    </figure>
  );
}
