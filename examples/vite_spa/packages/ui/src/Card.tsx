import type { ReactNode } from 'react';
import styles from './Card.module.css';

export interface CardProps {
  title: ReactNode;
  eyebrow?: ReactNode;
  media?: ReactNode;
  footer?: ReactNode;
  children?: ReactNode;
  /** Element to render; `li` when the card sits in a list. */
  as?: 'article' | 'section' | 'li';
}

export function Card({ title, eyebrow, media, footer, children, as: Tag = 'article' }: CardProps) {
  return (
    <Tag className={styles.card}>
      {media && <div className={styles.media}>{media}</div>}
      <div className={styles.body}>
        {eyebrow && <p className={styles.eyebrow}>{eyebrow}</p>}
        <h3 className={styles.title}>{title}</h3>
        {children}
      </div>
      {footer && <footer className={styles.footer}>{footer}</footer>}
    </Tag>
  );
}
