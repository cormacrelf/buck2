// The one place that reads import.meta.env. Vite replaces each access with a
// literal at build time, so the rest of the app gets plain typed values.
import type { UnitSystem } from '@fieldnotes/geo';
import type { Locale } from '@/i18n/locales';

function number(name: string, raw: string): number {
  const value = Number(raw);
  if (!Number.isFinite(value)) throw new Error(`${name} must be a number, got "${raw}"`);
  return value;
}

const units: UnitSystem = import.meta.env.VITE_UNITS === 'imperial' ? 'imperial' : 'metric';

export const env = {
  title: import.meta.env.VITE_APP_TITLE,
  channel: import.meta.env.VITE_RELEASE_CHANNEL,
  defaultLocale: import.meta.env.VITE_DEFAULT_LOCALE as Locale,
  home: {
    lat: number('VITE_HOME_LAT', import.meta.env.VITE_HOME_LAT),
    lon: number('VITE_HOME_LON', import.meta.env.VITE_HOME_LON),
  },
  units,
  mode: import.meta.env.MODE,
  dev: import.meta.env.DEV,
  baseUrl: import.meta.env.BASE_URL,
  version: __APP_VERSION__,
} as const;
