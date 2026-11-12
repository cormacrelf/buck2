import styles from './RouteFallback.module.css';

/** Shown while a lazily loaded route's chunk downloads. */
export function RouteFallback() {
  return <div className={styles.fallback} aria-busy="true" />;
}
