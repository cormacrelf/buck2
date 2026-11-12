import { formatElevation } from '@fieldnotes/geo';
import { HEIGHT, HEIGHTS, NAME, SEA_LEVEL, WIDTH } from '@fieldnotes/terrain';
import { Card } from '@fieldnotes/ui';
import { contours } from 'd3-contour';
import { geoPath } from 'd3-geo';
import { interpolateBlues, interpolateYlGn } from 'd3-scale-chromatic';
import { useMemo } from 'react';
import { env } from '@/env';
import { useT } from '@/i18n/context';
import styles from './TerrainPage.module.css';

// Only this page imports these. Under buck2 the generated heightmap takes a five second build
// step, and the d3 packages are bundled when the page first asks for them.

const STEP = 25; // metres between contours
const INDEX = 100; // every fourth contour is drawn heavier

function colour(metres: number): string {
  if (metres < SEA_LEVEL) return interpolateBlues(0.35 + 0.5 * (1 - metres / SEA_LEVEL));
  return interpolateYlGn(0.8 - 0.75 * ((metres - SEA_LEVEL) / (1000 - SEA_LEVEL)));
}

export default function TerrainPage() {
  const t = useT();
  const bands = useMemo(() => {
    const thresholds = Array.from({ length: 1000 / STEP }, (_, i) => i * STEP);
    const path = geoPath();
    return contours()
      .size([WIDTH, HEIGHT])
      .thresholds(thresholds)(HEIGHTS as number[])
      .map((c) => ({ value: c.value, d: path(c) ?? '' }));
  }, []);
  const highest = Math.max(...HEIGHTS);
  const land = HEIGHTS.filter((h) => h >= SEA_LEVEL).length / HEIGHTS.length;

  return (
    <>
      <h1>{t('terrain.title')}</h1>
      <Card title={NAME}>
        <svg className={styles.map} viewBox={`0 0 ${WIDTH} ${HEIGHT}`} role="img" aria-label={NAME}>
          {bands.map((b) => (
            <path
              key={b.value}
              d={b.d}
              fill={colour(b.value)}
              stroke={b.value >= SEA_LEVEL && b.value % INDEX === 0 ? 'rgb(0 0 0 / 0.35)' : 'rgb(0 0 0 / 0.12)'}
              strokeWidth={b.value % INDEX === 0 ? 0.3 : 0.15}
            />
          ))}
        </svg>
        <dl className={styles.stats}>
          <div>
            <dt>{t('terrain.highest')}</dt>
            <dd>{formatElevation(highest - SEA_LEVEL, env.units)}</dd>
          </div>
          <div>
            <dt>{t('terrain.land')}</dt>
            <dd>{Math.round(land * 100)}%</dd>
          </div>
        </dl>
      </Card>
    </>
  );
}
