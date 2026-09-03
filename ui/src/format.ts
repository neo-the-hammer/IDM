/** Human-readable byte counts, rates and durations. */

const UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'] as const;

/**
 * Formats a byte count.
 *
 * Uses the caller's locale for the decimal separator, so Persian gets Persian
 * digits and separators rather than a half-translated interface.
 */
export function bytes(value: number, locale = 'en'): string {
  if (!Number.isFinite(value) || value < 0) return '—';
  let size = value;
  let unit = 0;
  while (size >= 1024 && unit < UNITS.length - 1) {
    size /= 1024;
    unit += 1;
  }
  const digits = unit === 0 ? 0 : size < 10 ? 1 : size < 100 ? 1 : 0;
  const number = size.toLocaleString(locale, {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  });
  return `${number} ${UNITS[unit]}`;
}

export function rate(bytesPerSecond: number, locale = 'en'): string {
  if (bytesPerSecond <= 0) return '—';
  return `${bytes(bytesPerSecond, locale)}/s`;
}

export function duration(seconds: number | null, locale = 'en'): string {
  if (seconds === null || !Number.isFinite(seconds) || seconds < 0) return '—';
  const n = (value: number) => Math.floor(value).toLocaleString(locale);
  if (seconds < 60) return `${n(seconds)}s`;
  if (seconds < 3600) return `${n(seconds / 60)}m ${n(seconds % 60)}s`;
  if (seconds < 86400) return `${n(seconds / 3600)}h ${n((seconds % 3600) / 60)}m`;
  return `${n(seconds / 86400)}d ${n((seconds % 86400) / 3600)}h`;
}

export function percent(done: number, total: number | null, locale = 'en'): string {
  if (!total || total <= 0) return '—';
  return (done / total).toLocaleString(locale, {
    style: 'percent',
    minimumFractionDigits: 1,
    maximumFractionDigits: 1,
  });
}

export function timestamp(unixSeconds: number, locale = 'en'): string {
  if (!unixSeconds) return '—';
  return new Date(unixSeconds * 1000).toLocaleString(locale, {
    dateStyle: 'medium',
    timeStyle: 'short',
  });
}

/** The uppercase extension, for the little file-type badge. */
export function extension(filename: string): string {
  const dot = filename.lastIndexOf('.');
  if (dot <= 0 || dot === filename.length - 1) return '?';
  const ext = filename.slice(dot + 1);
  return ext.length <= 4 ? ext.toUpperCase() : ext.slice(0, 4).toUpperCase();
}

/**
 * Parses a rate written as `500k`, `2M`, or a plain number of bytes.
 * Returns 0 for empty input, which means unlimited.
 */
export function parseRate(input: string): number | null {
  const text = input.trim();
  if (text === '' || text.toLowerCase() === 'none') return 0;
  const match = /^([0-9]*\.?[0-9]+)\s*([kmgt]?)i?b?$/i.exec(text);
  if (!match) return null;
  const size = Number.parseFloat(match[1] ?? '');
  if (!Number.isFinite(size)) return null;
  const scale: Record<string, number> = {
    '': 1,
    k: 1024,
    m: 1024 ** 2,
    g: 1024 ** 3,
    t: 1024 ** 4,
  };
  return Math.round(size * (scale[(match[2] ?? '').toLowerCase()] ?? 1));
}
