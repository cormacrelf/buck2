import { formatCoordinate, formatDistance, haversine } from '@fieldnotes/geo';
import { Icon } from '@fieldnotes/ui';
// Named import from JSON: only `sites` is bundled, not the rest of the file.
import { sites } from '@/data/sites.json';
// imagetools: three widths in three formats, returned as { sources, img }.
import hero from '@/assets/photos/ridge-dawn.jpg?w=480;800;1200&format=avif;webp;jpg&as=picture';
// Forced inline as a data: URL whatever its size.
import paperGrain from '@/assets/images/paper-grain.png?inline';
import { NoteCard } from '@/components/NoteCard';
import { ResponsivePicture } from '@/components/ResponsivePicture';
import { allNotes } from '@/content/notes';
import { env } from '@/env';
import { useT } from '@/i18n/context';
import { yearOf } from '@/lib/dates';
import styles from './HomePage.module.scss';

export default function HomePage() {
  const t = useT();
  const firstYear = Math.min(...allNotes.map((n) => yearOf(n.date)));
  const bySite = [...sites]
    .map((site) => ({ ...site, distance: haversine(env.home, site) }))
    .sort((a, b) => a.distance - b.distance);

  return (
    <>
      <section className={styles.hero}>
        <ResponsivePicture picture={hero} alt="" sizes="100vw" priority />
        <p className={styles.intro} style={{ backgroundImage: `url("${paperGrain}")` }}>
          {t('home.intro', { year: firstYear })}
        </p>
      </section>

      <section>
        <h2>{t('home.latest')}</h2>
        <ul className="grid">
          {allNotes.slice(0, 3).map((note) => (
            <NoteCard key={note.slug} note={note} />
          ))}
        </ul>
      </section>

      <section>
        <h2>{t('home.sites')}</h2>
        <ul className={styles.sites}>
          {bySite.map((site) => (
            <li key={site.id}>
              <Icon name="map-pin" />
              <strong>{site.name}</strong>
              <span className={styles.habitat}>{site.habitat}</span>
              <code className={styles.coords}>
                {formatCoordinate(site.lat, 'lat')} {formatCoordinate(site.lon, 'lon')}
              </code>
              <span>{t('home.fromHome', { distance: formatDistance(site.distance, env.units) })}</span>
            </li>
          ))}
        </ul>
      </section>
    </>
  );
}
