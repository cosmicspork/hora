// Dates are local `YYYY-MM-DD` strings. Times keep the wall clock they were
// recorded at (the RFC 3339 offset hora stored), so they are read by slicing,
// not by converting to the browser's zone.

const DAY_MS = 86_400_000;

function toUtc(date: string): number {
  const [y, m, d] = date.split('-').map(Number);
  return Date.UTC(y, m - 1, d);
}

function fromUtc(ms: number): string {
  return new Date(ms).toISOString().slice(0, 10);
}

export function addDays(date: string, days: number): string {
  return fromUtc(toUtc(date) + days * DAY_MS);
}

/** Monday of the ISO week holding `date`. */
export function weekStart(date: string): string {
  const weekday = (new Date(toUtc(date)).getUTCDay() + 6) % 7;
  return addDays(date, -weekday);
}

export function weekDays(start: string): string[] {
  return Array.from({ length: 7 }, (_, i) => addDays(start, i));
}

export function monthStart(date: string): string {
  return `${date.slice(0, 7)}-01`;
}

export function monthEnd(date: string): string {
  const [y, m] = date.split('-').map(Number);
  return fromUtc(Date.UTC(y, m, 0));
}

const WEEKDAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

export function dayLabel(date: string): string {
  const d = new Date(toUtc(date));
  return `${WEEKDAYS[d.getUTCDay()]} ${d.getUTCDate()} ${MONTHS[d.getUTCMonth()]}`;
}

export function formatMinutes(minutes: number): string {
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  if (h === 0) return `${m}m`;
  return m === 0 ? `${h}h` : `${h}h ${m}m`;
}

export function hours(minutes: number): string {
  return (minutes / 60).toFixed(2);
}

/** `HH:MM` as recorded. */
export function clock(instant: string | null): string {
  return instant ? instant.slice(11, 16) : '';
}

/** The recorded offset when it differs from the browser's current one. */
export function foreignOffset(instant: string | null, now = new Date()): string | null {
  if (!instant) return null;
  const offset = instant.slice(19);
  const local = -now.getTimezoneOffset();
  const sign = local >= 0 ? '+' : '-';
  const abs = Math.abs(local);
  const mine = `${sign}${String(Math.floor(abs / 60)).padStart(2, '0')}:${String(abs % 60).padStart(2, '0')}`;
  const normalized = offset === 'Z' ? '+00:00' : offset;
  return normalized === mine ? null : normalized;
}

export function elapsedMinutes(startedAt: string, nowMs = Date.now()): number {
  return Math.max(0, Math.floor((nowMs - Date.parse(startedAt)) / 60_000));
}

/** Accepts `90`, `90m`, `1h`, `1h30m`, `1h 30m`, `1:30`, `1.5h`. */
export function parseDuration(raw: string): number | null {
  const text = raw.trim().toLowerCase().replace(/\s+/g, '');
  if (!text) return null;
  let match = /^(\d+):([0-5]\d)$/.exec(text);
  if (match) return Number(match[1]) * 60 + Number(match[2]);
  match = /^(\d+(?:\.\d+)?)h$/.exec(text);
  if (match) return Math.round(Number(match[1]) * 60);
  match = /^(?:(\d+)h)?(?:(\d+)m?)?$/.exec(text);
  if (match && (match[1] || match[2])) return Number(match[1] ?? 0) * 60 + Number(match[2] ?? 0);
  return null;
}

/** Minutes between two `HH:MM` on one date; an end before the start runs past midnight. */
export function spanMinutes(start: string, end: string): number | null {
  const a = /^(\d{2}):(\d{2})$/.exec(start);
  const b = /^(\d{2}):(\d{2})$/.exec(end);
  if (!a || !b) return null;
  let minutes = Number(b[1]) * 60 + Number(b[2]) - (Number(a[1]) * 60 + Number(a[2]));
  if (minutes <= 0) minutes += 1440;
  return minutes;
}

/**
 * A timestamp for the server, moving the end to the next day past midnight.
 * With an offset (an existing entry's recorded one) the wall-clock time keeps
 * that offset; without, the server reads it as its own local time.
 */
export function localStamp(date: string, hm: string, after?: string, offset?: string): string {
  const day = after && hm <= after ? addDays(date, 1) : date;
  return offset ? `${day}T${hm}:00${offset}` : `${day} ${hm}`;
}

/** The offset part of an RFC 3339 instant: `+05:30`, `-06:00` or `Z`. */
export function offsetOf(instant: string | null | undefined): string | undefined {
  return instant ? /(Z|[+-]\d{2}:\d{2})$/.exec(instant)?.[1] : undefined;
}
