import { useEffect, useState } from 'react';
import { useT } from '@/i18n/context';
import { RouteFallback } from './RouteFallback';

/** Shown while the terrain page loads, with how long it has taken so far. */
export function TerrainFallback() {
  const t = useT();
  const [start] = useState(() => performance.now());
  const [now, setNow] = useState(start);
  useEffect(() => {
    const id = setInterval(() => setNow(performance.now()), 100);
    return () => clearInterval(id);
  }, []);
  return (
    <>
      <RouteFallback />
      <p>{t('terrain.waiting', { seconds: ((now - start) / 1000).toFixed(1) })}</p>
    </>
  );
}
