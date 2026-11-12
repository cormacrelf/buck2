import type { ReactNode } from 'react';
import styles from './EmptyState.module.css';
import { Icon } from './Icon.tsx';
import type { IconName } from './icons/index.ts';

export interface EmptyStateProps {
  icon: IconName;
  title: string;
  children?: ReactNode;
}

export function EmptyState({ icon, title, children }: EmptyStateProps) {
  return (
    <div className={styles.empty} role="status">
      <Icon name={icon} size={36} className={styles.icon} />
      <p className={styles.title}>{title}</p>
      {children && <div className={styles.detail}>{children}</div>}
    </div>
  );
}
