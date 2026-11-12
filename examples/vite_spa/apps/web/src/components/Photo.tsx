import { useState } from 'react';
import type { Photo as PhotoData } from '@/lib/photo';
import styles from './Photo.module.scss';

export interface PhotoProps {
  photo: PhotoData;
  alt: string;
  /** The `sizes` attribute; defaults to the content column width. */
  sizes?: string;
  priority?: boolean;
}

export function Photo({ photo, alt, sizes = '(min-width: 46rem) 46rem, 100vw', priority }: PhotoProps) {
  const [loaded, setLoaded] = useState(false);
  return (
    <picture
      className={styles.frame}
      style={{ backgroundImage: loaded ? undefined : `url("${photo.placeholder}")` }}
    >
      {photo.sources.map((source) => (
        <source key={source.type} type={source.type} srcSet={source.srcset} sizes={sizes} />
      ))}
      <img
        className={loaded ? `${styles.img} ${styles.loaded}` : styles.img}
        src={photo.src}
        alt={alt}
        width={photo.width}
        height={photo.height}
        loading={priority ? 'eager' : 'lazy'}
        decoding="async"
        onLoad={() => setLoaded(true)}
      />
    </picture>
  );
}
