import { GROUPS, type GroupId, TAXA, type Taxon, type TaxonId } from './generated/taxa.ts';

export {
  GROUPS,
  type Group,
  type GroupId,
  type Status,
  TAXA,
  TAXON_COUNT,
  type Taxon,
  type TaxonId,
} from './generated/taxa.ts';

const byId = new Map<string, Taxon>(TAXA.map((t) => [t.id, t]));

export function isTaxonId(value: string): value is TaxonId {
  return byId.has(value);
}

export function getTaxon(id: TaxonId): Taxon {
  // The generated union guarantees every TaxonId is in TAXA.
  return byId.get(id)!;
}

export function taxaInGroup(group: GroupId): Taxon[] {
  return TAXA.filter((t) => t.group === group);
}

/** Case-insensitive match on common or scientific name. */
export function searchTaxa(query: string): Taxon[] {
  const q = query.trim().toLowerCase();
  if (q === '') return [...TAXA];
  return TAXA.filter(
    (t) => t.common.toLowerCase().includes(q) || t.scientific.toLowerCase().includes(q),
  );
}

export function groupColour(group: GroupId): string {
  return GROUPS[group].colour;
}
