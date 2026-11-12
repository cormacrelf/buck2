import type { ReactNode } from 'react';
import styles from './Tag.module.css';

export interface TagProps {
  children: ReactNode;
  /** Any CSS colour; drives the dot and a tinted background. */
  colour?: string;
}

export function Tag({ children, colour }: TagProps) {
  return (
    <span className={styles.tag} style={colour ? { '--tag-colour': colour } : undefined}>
      <span className={styles.dot} aria-hidden="true" />
      {children}
    </span>
  );
}

declare module 'react' {
  interface CSSProperties {
    '--tag-colour'?: string;
  }
}
