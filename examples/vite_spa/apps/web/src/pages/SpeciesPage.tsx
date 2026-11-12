import { GROUPS, type GroupId, searchTaxa, TAXON_COUNT } from '@fieldnotes/taxa';
import { Button, EmptyState, Tag } from '@fieldnotes/ui';
import { useDeferredValue, useState } from 'react';
import { FavouriteButton } from '@/components/FavouriteButton';
import { useT } from '@/i18n/context';
import { useFavourites } from '@/store/favourites';
import styles from './SpeciesPage.module.scss';

const GROUP_IDS = Object.keys(GROUPS) as GroupId[];

export default function SpeciesPage() {
  const t = useT();
  const [query, setQuery] = useState('');
  const [group, setGroup] = useState<GroupId | null>(null);
  const [starredOnly, setStarredOnly] = useState(false);
  const deferredQuery = useDeferredValue(query);
  const starred = useFavourites();

  const shown = searchTaxa(deferredQuery).filter(
    (taxon) => (group === null || taxon.group === group) && (!starredOnly || starred.has(taxon.id)),
  );

  return (
    <>
      <h1>{t('species.title')}</h1>
      <div className={styles.controls}>
        <input
          type="search"
          value={query}
          placeholder={t('species.search')}
          aria-label={t('species.search')}
          onChange={(event) => setQuery(event.target.value)}
        />
        <div className={styles.groups}>
          {GROUP_IDS.map((id) => (
            <Button key={id} pressed={group === id} onClick={() => setGroup(group === id ? null : id)}>
              <Tag colour={GROUPS[id].colour}>{GROUPS[id].label}</Tag>
            </Button>
          ))}
        </div>
        <Button icon="star" pressed={starredOnly} onClick={() => setStarredOnly(!starredOnly)}>
          {t('species.favourites')}
        </Button>
        <span className={styles.count}>{t('species.count', { shown: shown.length, total: TAXON_COUNT })}</span>
      </div>
      {shown.length === 0 ? (
        <EmptyState icon="leaf" title={t('species.empty')} />
      ) : (
        <table className={styles.table}>
          <tbody>
            {shown.map((taxon) => (
              <tr key={taxon.id}>
                <td>
                  <FavouriteButton id={taxon.id} label={taxon.common} />
                </td>
                <td>{taxon.common}</td>
                <td>
                  <i>{taxon.scientific}</i>
                </td>
                <td>
                  <Tag colour={GROUPS[taxon.group].colour}>{GROUPS[taxon.group].label}</Tag>
                </td>
                <td className={styles[taxon.status]}>{taxon.status}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </>
  );
}
