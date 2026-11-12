import { getTaxon, groupColour, isTaxonId } from '@fieldnotes/taxa';
import { EmptyState, Icon, Tag } from '@fieldnotes/ui';
import { Link, useParams } from 'react-router';
import { Photo } from '@/components/Photo';
import { RouteFallback } from '@/components/RouteFallback';
import { findNote, loadNote, useLiveNote } from '@/content/notes';
import { findTrack } from '@/content/tracks';
import { useI18n, useT } from '@/i18n/context';
import { formatDate } from '@/lib/dates';
import { loadPhoto } from '@/lib/photo';
import { useAsync } from '@/lib/useAsync';
import styles from './NotePage.module.scss';

export default function NotePage() {
  const { slug = '' } = useParams();
  const t = useT();
  const { locale } = useI18n();
  const meta = findNote(slug);
  const loaded = useAsync(() => loadNote(slug), [slug]);
  const photo = useAsync(async () => (meta?.photo ? loadPhoto(meta.photo) : undefined), [meta?.photo]);
  // In dev, an edit to the .md file arrives here without a reload.
  const live = useLiveNote(slug);

  if (!meta) {
    return <EmptyState icon="leaf" title={t('note.notFound', { slug })} />;
  }
  const doc = live ?? (loaded.status === 'done' ? loaded.value : undefined);
  const track = meta.track ? findTrack(meta.track) : undefined;
  const taxa = meta.taxa.filter(isTaxonId).map(getTaxon);

  return (
    <article className={styles.note}>
      <Link to="/notes" className={styles.back}>
        <Icon name="arrow-left" /> {t('note.back')}
      </Link>
      <header>
        <p className={styles.date}>{formatDate(meta.date, locale)}</p>
        <h1>{doc?.meta.title ?? meta.title}</h1>
      </header>
      {photo.status === 'done' && photo.value && <Photo photo={photo.value} alt="" priority />}
      {doc && doc.headings.length > 1 && (
        <nav className={styles.toc} aria-label={t('note.contents')}>
          <ol>
            {doc.headings
              .filter((h) => h.depth === 2)
              .map((h) => (
                <li key={h.id}>
                  <a href={`#${h.id}`}>{h.text}</a>
                </li>
              ))}
          </ol>
        </nav>
      )}
      {doc ? (
        // The HTML was rendered from our own Markdown at build time.
        <div className={styles.prose} dangerouslySetInnerHTML={{ __html: doc.html }} />
      ) : (
        <RouteFallback />
      )}
      {taxa.length > 0 && (
        <aside className={styles.seen}>
          <h2>{t('note.seen')}</h2>
          <ul>
            {taxa.map((taxon) => (
              <li key={taxon.id}>
                <Tag colour={groupColour(taxon.group)}>{taxon.common}</Tag> <i>{taxon.scientific}</i>
              </li>
            ))}
          </ul>
        </aside>
      )}
      {track && (
        <p>
          <Icon name="mountain" /> <Link to={`/tracks#${track.name}`}>{t('nav.tracks')}</Link>
        </p>
      )}
    </article>
  );
}
