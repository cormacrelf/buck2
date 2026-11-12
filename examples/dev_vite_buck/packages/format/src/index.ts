const UNITS = ["B", "KiB", "MiB", "GiB"] as const;

export function formatBytes(n: number): string {
  let i = 0;
  while (n >= 1024 && i < UNITS.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(i ? 1 : 0)} ${UNITS[i]}`;
}
