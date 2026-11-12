import { useI18n, useT } from '@/i18n/context';
import { isLocale, LOCALES } from '@/i18n/locales';

export function LocaleSwitch() {
  const { locale, setLocale } = useI18n();
  const t = useT();
  return (
    <select
      aria-label={t('locale.label')}
      value={locale}
      onChange={(event) => {
        if (isLocale(event.target.value)) setLocale(event.target.value);
      }}
    >
      {LOCALES.map((l) => (
        <option key={l} value={l}>
          {l.toUpperCase()}
        </option>
      ))}
    </select>
  );
}
