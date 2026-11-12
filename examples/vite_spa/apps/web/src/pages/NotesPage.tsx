import { Button, EmptyState } from '@fieldnotes/ui';
import { useSearchParams } from 'react-router';
import { NoteCard } from '@/components/NoteCard';
import { allNotes, allTags } from '@/content/notes';
import { useT } from '@/i18n/context';

export default function NotesPage() {
  const t = useT();
  const [params, setParams] = useSearchParams();
  const tag = params.get('tag');
  const notes = tag ? allNotes.filter((n) => n.tags.includes(tag)) : allNotes;

  const choose = (next: string | null) =>
    setParams(next ? { tag: next } : {}, { replace: true });

  return (
    <>
      <h1>{t('notes.title')}</h1>
      <div role="group" aria-label={t('notes.filter')} style={{ display: 'flex', flexWrap: 'wrap', gap: 6, marginBottom: 20 }}>
        <Button pressed={tag === null} onClick={() => choose(null)}>
          {t('notes.all')}
        </Button>
        {allTags.map((name) => (
          <Button key={name} pressed={tag === name} onClick={() => choose(name)}>
            {name}
          </Button>
        ))}
      </div>
      {notes.length === 0 ? (
        <EmptyState icon="leaf" title={t('notes.empty')} />
      ) : (
        <ul className="grid">
          {notes.map((note) => (
            <NoteCard key={note.slug} note={note} />
          ))}
        </ul>
      )}
    </>
  );
}
