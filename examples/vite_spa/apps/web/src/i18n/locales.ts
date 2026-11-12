import type en from './messages/en.json';

export type Messages = typeof en;
export type MessageKey = keyof Messages;

export const LOCALES = ['en', 'de'] as const;
export type Locale = (typeof LOCALES)[number];

export function isLocale(value: string): value is Locale {
  return (LOCALES as readonly string[]).includes(value);
}

/**
 * The import path has a variable in it. Vite turns that into a lookup over
 * every file matching `./messages/*.json`, each in its own chunk, so only the
 * active locale is downloaded.
 */
export async function loadMessages(locale: Locale): Promise<Messages> {
  const mod = (await import(`./messages/${locale}.json`)) as { default: Messages };
  return mod.default;
}

export function format(template: string, values: Record<string, string | number> = {}): string {
  return template.replace(/\{(\w+)\}/g, (whole, key: string) =>
    key in values ? String(values[key]) : whole,
  );
}
