import type { Picture } from 'vite-imagetools';

const MIME: Record<string, string> = {
  avif: 'image/avif',
  webp: 'image/webp',
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  png: 'image/png',
};

export interface ResponsivePictureProps {
  picture: Picture;
  alt: string;
  sizes: string;
  className?: string;
  priority?: boolean;
}

/** Renders an imagetools `as=picture` import. */
export function ResponsivePicture({ picture, alt, sizes, className, priority }: ResponsivePictureProps) {
  return (
    <picture className={className}>
      {Object.entries(picture.sources).map(([format, srcset]) => (
        <source key={format} type={MIME[format]} srcSet={srcset} sizes={sizes} />
      ))}
      <img
        src={picture.img.src}
        width={picture.img.w}
        height={picture.img.h}
        alt={alt}
        loading={priority ? 'eager' : 'lazy'}
        fetchPriority={priority ? 'high' : undefined}
        decoding="async"
      />
    </picture>
  );
}
