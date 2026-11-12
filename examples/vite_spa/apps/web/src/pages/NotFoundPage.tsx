import { EmptyState } from '@fieldnotes/ui';
import { Link } from 'react-router';
import { useT } from '@/i18n/context';

export default function NotFoundPage() {
  const t = useT();
  return (
    <EmptyState icon="map-pin" title={t('notFound.title')}>
      <p>{t('notFound.body')}</p>
      <Link to="/">{t('nav.home')}</Link>
    </EmptyState>
  );
}
