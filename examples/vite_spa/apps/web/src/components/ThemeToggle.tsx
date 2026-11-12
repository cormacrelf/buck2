import { Button } from '@fieldnotes/ui';
import { useState } from 'react';
import { useT } from '@/i18n/context';

// Same key as the inline script in index.html, which applies it before paint.
const STORAGE_KEY = 'fieldnotes:theme';

type Theme = 'light' | 'dark';

export function ThemeToggle() {
  const t = useT();
  const [theme, setTheme] = useState<Theme>(() =>
    document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light',
  );

  const toggle = () => {
    const next: Theme = theme === 'dark' ? 'light' : 'dark';
    document.documentElement.dataset.theme = next;
    try {
      localStorage.setItem(STORAGE_KEY, next);
    } catch {
      // Not persisted.
    }
    setTheme(next);
  };

  return (
    <Button onClick={toggle} pressed={theme === 'dark'} aria-label={t('theme.toggle')}>
      {theme === 'dark' ? '☾' : '☀'}
    </Button>
  );
}
