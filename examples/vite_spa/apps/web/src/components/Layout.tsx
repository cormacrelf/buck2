import { NavLink, Outlet } from 'react-router';
import compassUrl from '@/assets/compass.svg';
import { env } from '@/env';
import { flags } from '@/featureFlags';
import { useT } from '@/i18n/context';
import type { MessageKey } from '@/i18n/locales';
import { BuildBadge } from './BuildBadge.tsx';
import styles from './Layout.module.scss';
import { LocaleSwitch } from './LocaleSwitch.tsx';
import { ThemeToggle } from './ThemeToggle.tsx';

const NAV: Array<{ to: string; label: MessageKey; enabled: boolean }> = [
  { to: '/', label: 'nav.home', enabled: true },
  { to: '/notes', label: 'nav.notes', enabled: true },
  { to: '/gallery', label: 'nav.gallery', enabled: true },
  { to: '/tracks', label: 'nav.tracks', enabled: true },
  { to: '/species', label: 'nav.species', enabled: flags.speciesChecklist },
  { to: '/about', label: 'nav.about', enabled: true },
];

export function Layout() {
  const t = useT();
  return (
    <div className={styles.shell}>
      <header className={styles.header}>
        <NavLink to="/" className={styles.brand} end>
          <img src={compassUrl} alt="" width={32} height={32} />
          <span>{env.title}</span>
        </NavLink>
        <nav className={styles.nav} aria-label="Main">
          {NAV.filter((item) => item.enabled).map((item) => (
            <NavLink
              key={item.to}
              to={item.to}
              end={item.to === '/'}
              className={({ isActive }) => (isActive ? `${styles.link} ${styles.active}` : styles.link)}
            >
              {t(item.label)}
            </NavLink>
          ))}
        </nav>
        <div className={styles.tools}>
          <LocaleSwitch />
          <ThemeToggle />
        </div>
      </header>
      <main className={styles.main}>
        <Outlet />
      </main>
      <footer className={styles.footer}>
        {flags.buildBadge && <BuildBadge />}
      </footer>
    </div>
  );
}
