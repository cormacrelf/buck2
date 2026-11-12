import { Card, Tag } from '@fieldnotes/ui';
import { Link } from 'react-router';
import type { NoteMeta } from '@/content/types';
import { useI18n, useT } from '@/i18n/context';
import { formatDate } from '@/lib/dates';

export function NoteCard({ note }: { note: NoteMeta }) {
  const t = useT();
  const { locale } = useI18n();
  return (
    <Card
      as="li"
      eyebrow={formatDate(note.date, locale)}
      title={<Link to={`/notes/${note.slug}`}>{note.title}</Link>}
      footer={t('notes.readingTime', { minutes: note.readingMinutes })}
    >
      <div style={{ display: 'flex', flexWrap: 'wrap', gap: 6 }}>
        {note.tags.map((tag) => (
          <Tag key={tag}>{tag}</Tag>
        ))}
      </div>
    </Card>
  );
}
