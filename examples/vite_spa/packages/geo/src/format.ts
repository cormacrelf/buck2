export type UnitSystem = 'metric' | 'imperial';

const M_PER_MILE = 1609.344;
const FT_PER_M = 3.280839895;

export function formatDistance(metres: number, units: UnitSystem = 'metric'): string {
  if (units === 'imperial') {
    const miles = metres / M_PER_MILE;
    return miles < 0.2 ? `${Math.round(metres * FT_PER_M)} ft` : `${miles.toFixed(1)} mi`;
  }
  return metres < 1000 ? `${Math.round(metres)} m` : `${(metres / 1000).toFixed(1)} km`;
}

export function formatElevation(metres: number, units: UnitSystem = 'metric'): string {
  return units === 'imperial'
    ? `${Math.round(metres * FT_PER_M)} ft`
    : `${Math.round(metres)} m`;
}

export function formatDuration(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.round((seconds % 3600) / 60);
  return h > 0 ? `${h} h ${m.toString().padStart(2, '0')} min` : `${m} min`;
}

/** Degrees as `41°24.204′N` style degrees and decimal minutes. */
export function formatCoordinate(value: number, axis: 'lat' | 'lon'): string {
  const hemisphere = axis === 'lat' ? (value >= 0 ? 'N' : 'S') : value >= 0 ? 'E' : 'W';
  const abs = Math.abs(value);
  const degrees = Math.floor(abs);
  const minutes = (abs - degrees) * 60;
  return `${degrees}°${minutes.toFixed(3)}′${hemisphere}`;
}
