import { useEffect, useState } from 'react';
import { env } from '@/env';

/** Dev-only details. Loaded only when `__DEBUG_PANEL__` is true. */
export default function DebugPanel() {
  const [served, setServed] = useState<string>('…');

  useEffect(() => {
    // The dev server answers this from plugins/build-info.ts; a build ships it
    // as a static file.
    fetch(`${env.baseUrl}version.json`)
      .then((res) => (res.ok ? res.text() : `${res.status} ${res.statusText}`))
      .then(setServed, (err: unknown) => setServed(String(err)));
  }, []);

  return (
    <section>
      <h2>Debug</h2>
      <pre>{JSON.stringify({ env: import.meta.env, hot: Boolean(import.meta.hot) }, null, 2)}</pre>
      <p>version.json:</p>
      <pre>{served}</pre>
    </section>
  );
}
