const formatters = new Map<string, Intl.DateTimeFormat>();

function formatter(locale: string): Intl.DateTimeFormat {
  let f = formatters.get(locale);
  if (!f) {
    f = new Intl.DateTimeFormat(locale, { day: 'numeric', month: 'long', year: 'numeric', timeZone: 'UTC' });
    formatters.set(locale, f);
  }
  return f;
}

/** Formats an ISO `YYYY-MM-DD` date for display. */
export function formatDate(isoDate: string, locale: string): string {
  return formatter(locale).format(new Date(`${isoDate}T00:00:00Z`));
}

export function yearOf(isoDate: string): number {
  return Number(isoDate.slice(0, 4));
}
