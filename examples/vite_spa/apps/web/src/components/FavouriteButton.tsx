import { Button } from '@fieldnotes/ui';
import { toggleFavourite, useIsFavourite } from '@/store/favourites';

export function FavouriteButton({ id, label }: { id: string; label: string }) {
  const starred = useIsFavourite(id);
  return (
    <Button
      icon="star"
      pressed={starred}
      aria-label={starred ? `Unstar ${label}` : `Star ${label}`}
      onClick={() => toggleFavourite(id)}
    />
  );
}
