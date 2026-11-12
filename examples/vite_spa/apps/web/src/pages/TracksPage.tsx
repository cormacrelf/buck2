import { formatDistance, formatDuration, formatElevation, type TrackStats } from '@fieldnotes/geo';
import { Card, Icon } from '@fieldnotes/ui';
import { ElevationProfile } from '@/components/ElevationProfile';
import { RouteFallback } from '@/components/RouteFallback';
import { allTracks, type TrackRef } from '@/content/tracks';
import { env } from '@/env';
import { flags } from '@/featureFlags';
import { useT } from '@/i18n/context';
import { useAsync } from '@/lib/useAsync';
import { trackStats } from '@/workers/trackStatsClient';
import styles from './TracksPage.module.css';

// Resolved by Vite at build time like an import; no import statement needed.
const markerUrl = new URL('../assets/marker.svg', import.meta.url).href;

function Stats({ stats }: { stats: TrackStats }) {
  const t = useT();
  const rows: Array<[string, string | undefined]> = [
    [t('tracks.distance'), formatDistance(stats.distanceM, env.units)],
    [t('tracks.ascent'), formatElevation(stats.ascentM, env.units)],
    [t('tracks.descent'), formatElevation(stats.descentM, env.units)],
    [t('tracks.highest'), stats.maxEleM === undefined ? undefined : formatElevation(stats.maxEleM, env.units)],
    [t('tracks.moving'), stats.movingS === undefined ? undefined : formatDuration(stats.movingS)],
  ];
  return (
    <dl className={styles.stats}>
      {rows.map(([label, value]) =>
        value === undefined ? null : (
          <div key={label}>
            <dt>{label}</dt>
            <dd>{value}</dd>
          </div>
        ),
      )}
    </dl>
  );
}

function TrackCard({ track }: { track: TrackRef }) {
  const t = useT();
  const stats = useAsync(() => trackStats(track.url), [track.url]);
  return (
    <section id={track.name}>
      <Card
        eyebrow={
          <>
            <img src={markerUrl} alt="" width={12} height={16} /> {track.name}
          </>
        }
        title={stats.status === 'done' ? stats.value.name : track.name}
        footer={
          <a href={track.url} download={`${track.name}.gpx`}>
            <Icon name="map-pin" /> {t('tracks.download')}
          </a>
        }
      >
        {stats.status === 'pending' && <RouteFallback />}
        {stats.status === 'error' && <p role="alert">{stats.error.message}</p>}
        {stats.status === 'done' && (
          <>
            <Stats stats={stats.value} />
            {flags.elevationProfile && <ElevationProfile stats={stats.value} units={env.units} />}
          </>
        )}
      </Card>
    </section>
  );
}

export default function TracksPage() {
  const t = useT();
  return (
    <>
      <h1>{t('tracks.title')}</h1>
      <div className={styles.list}>
        {allTracks.map((track) => (
          <TrackCard key={track.name} track={track} />
        ))}
      </div>
    </>
  );
}
