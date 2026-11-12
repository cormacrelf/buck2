import { lazy, Suspense } from 'react';
import buildInfo from 'virtual:build-info';
// One file, two queries, two different modules: the text itself, and the URL
// of a copy emitted into the build output.
import fontLicence from '@/assets/fonts/ibm-plex-mono-OFL.txt?raw';
import fontLicenceUrl from '@/assets/fonts/ibm-plex-mono-OFL.txt?url';
import { env } from '@/env';
import { useI18n, useT } from '@/i18n/context';
import styles from './AboutPage.module.css';

// `__DEBUG_PANEL__` is replaced with a literal by `define`. When it is false
// the ternary folds away and the panel's chunk is never emitted.
const DebugPanel = __DEBUG_PANEL__ ? lazy(() => import('@/components/DebugPanel')) : null;

export default function AboutPage() {
  const t = useT();
  const { locale } = useI18n();
  const builtAt = new Date(buildInfo.builtAt).toLocaleString(locale, { timeZone: 'UTC' });

  return (
    <article className={styles.about}>
      <h1>{t('about.title')}</h1>
      <h2>{t('about.build')}</h2>
      <dl className={styles.facts}>
        <dt>Version</dt>
        <dd>{env.version}</dd>
        <dt>Commit</dt>
        <dd>{buildInfo.commit ?? '—'}</dd>
        <dt>Built</dt>
        <dd>{builtAt} UTC</dd>
        <dt>Mode</dt>
        <dd>
          {env.mode} / {buildInfo.channel}
        </dd>
        {Object.entries(buildInfo.dependencies).map(([name, version]) => (
          <div key={name} className={styles.pair}>
            <dt>{name}</dt>
            <dd>{version}</dd>
          </div>
        ))}
      </dl>

      {DebugPanel && (
        <Suspense fallback={null}>
          <DebugPanel />
        </Suspense>
      )}

      <h2>{t('about.licences')}</h2>
      <details>
        <summary>IBM Plex Mono — SIL Open Font License 1.1</summary>
        <pre className={styles.licence}>{fontLicence}</pre>
        <a href={fontLicenceUrl} download="ibm-plex-mono-OFL.txt">
          {t('about.downloadLicence')}
        </a>
      </details>
    </article>
  );
}
