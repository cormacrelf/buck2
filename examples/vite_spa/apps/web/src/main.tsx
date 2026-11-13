// Stylesheets first, in cascade order: package tokens, fonts, globals.
import '@fieldnotes/ui/tokens.css';
import '@fontsource/literata/latin-400.css';
import '@fontsource/literata/latin-400-italic.css';
import '@fontsource/literata/latin-600.css';
import './styles/fonts.scss';
import './styles/global.css';
import './styles/tailwind.css';

import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App.tsx';
import { RouteFallback } from './components/RouteFallback.tsx';
import { env } from './env.ts';
// Imported here, at the root, so that editing it can't be hot-swapped and
// forces a full reload. See the comment in featureFlags.ts.
import { flags } from './featureFlags.ts';
import { I18nProvider } from './i18n/I18nProvider.tsx';

document.documentElement.dataset.flags = Object.entries(flags)
  .filter(([, on]) => on)
  .map(([name]) => name)
  .join(' ');

const root = document.getElementById('root');
if (!root) throw new Error('index.html has no #root');

createRoot(root).render(
  <StrictMode>
    <I18nProvider defaultLocale={env.defaultLocale} fallback={<RouteFallback />}>
      <App />
    </I18nProvider>
  </StrictMode>,
);
