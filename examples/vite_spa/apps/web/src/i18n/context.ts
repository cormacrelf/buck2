import { createContext, useCallback, useContext } from 'react';
import { format, type Locale, type MessageKey, type Messages } from './locales.ts';

export interface I18n {
  locale: Locale;
  messages: Messages;
  setLocale(locale: Locale): void;
}

export const I18nContext = createContext<I18n | null>(null);

export function useI18n(): I18n {
  const i18n = useContext(I18nContext);
  if (!i18n) throw new Error('useI18n outside <I18nProvider>');
  return i18n;
}

/** `t('home.fromHome', { distance: '3 km' })` */
export function useT() {
  const { messages } = useI18n();
  return useCallback(
    (key: MessageKey, values?: Record<string, string | number>) => format(messages[key], values),
    [messages],
  );
}
