import { channel, commit, version } from 'virtual:build-info';
import styles from './BuildBadge.module.css';

export function BuildBadge() {
  return (
    <p className={styles.badge}>
      v{version}
      {commit && <span className={styles.commit}>{commit}</span>}
      <span className={styles.channel} data-channel={channel}>
        {channel}
      </span>
    </p>
  );
}
