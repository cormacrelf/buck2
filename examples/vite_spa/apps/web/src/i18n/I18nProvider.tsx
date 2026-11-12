import { type ReactNode, useEffect, useMemo, useState } from 'react';
import { I18nContext } from './context.ts';
import { isLocale, type Locale, loadMessages, type Messages } from './locales.ts';

const STORAGE_KEY = 'fieldnotes:locale';

function initialLocale(fallback: Locale): Locale {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored && isLocale(stored)) return stored;
  } catch {
    // Storage can be unavailable (private mode); use the default.
  }
  return fallback;
}

export function I18nProvider({
  defaultLocale,
  fallback,
  children,
}: {
  defaultLocale: Locale;
  fallback: ReactNode;
  children: ReactNode;
}) {
  const [locale, setLocale] = useState(() => initialLocale(defaultLocale));
  const [messages, setMessages] = useState<Messages | null>(null);

  useEffect(() => {
    let current = true;
    loadMessages(locale).then((loaded) => {
      if (current) setMessages(loaded);
    });
    document.documentElement.lang = locale;
    try {
      localStorage.setItem(STORAGE_KEY, locale);
    } catch {
      // Not persisted; fine.
    }
    return () => {
      current = false;
    };
    // `loadMessages` is a dependency so that editing a messages file reloads it. The edit gives
    // `locales.ts` a new instance, so a new `loadMessages`. Fast Refresh would re-run this effect
    // anyway, but not under StrictMode.
  }, [locale, loadMessages]);

  const value = useMemo(
    () => (messages ? { locale, messages, setLocale } : null),
    [locale, messages],
  );
  if (!value) return fallback;
  return <I18nContext value={value}>{children}</I18nContext>;
}
