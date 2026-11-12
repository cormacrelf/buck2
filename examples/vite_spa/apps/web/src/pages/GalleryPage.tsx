import { Link } from 'react-router';
import type { Picture } from 'vite-imagetools';
import { ResponsivePicture } from '@/components/ResponsivePicture';
import { allNotes } from '@/content/notes';
import { useT } from '@/i18n/context';
import styles from './GalleryPage.module.scss';

// Every photo in the folder, run through the `card` preset defined in
// vite.config.ts (two widths × three formats). Adding a .jpg here adds it to
// the gallery with no code change.
const pictures = import.meta.glob<Picture>('../assets/photos/*.jpg', {
  eager: true,
  query: { preset: 'card' },
  import: 'default',
});

const captions = new Map(allNotes.flatMap((n) => (n.photo ? [[n.photo, n] as const] : [])));

export default function GalleryPage() {
  const t = useT();
  return (
    <>
      <h1>{t('gallery.title')}</h1>
      <ul className={styles.gallery}>
        {Object.entries(pictures).map(([path, picture]) => {
          const name = path.replace(/^.*\/|\.jpg$/g, '');
          const note = captions.get(name);
          return (
            <li key={path}>
              <figure>
                <ResponsivePicture
                  picture={picture}
                  alt={note?.title ?? name}
                  sizes="(min-width: 960px) 30vw, (min-width: 560px) 45vw, 100vw"
                  className={styles.picture}
                />
                <figcaption>{note ? <Link to={`/notes/${note.slug}`}>{note.title}</Link> : name}</figcaption>
              </figure>
            </li>
          );
        })}
      </ul>
    </>
  );
}
