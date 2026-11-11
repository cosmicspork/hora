import { describe, expect, test } from 'bun:test';
import {
  addDays,
  clock,
  dayLabel,
  formatMinutes,
  localStamp,
  offsetOf,
  monthEnd,
  parseDuration,
  spanMinutes,
  weekDays,
  weekStart
} from './time';

describe('dates', () => {
  test('weeks start on Monday', () => {
    expect(weekStart('2026-11-10')).toBe('2026-11-09');
    expect(weekStart('2026-11-09')).toBe('2026-11-09');
    expect(weekStart('2026-11-15')).toBe('2026-11-09');
    expect(weekDays('2026-11-09')).toHaveLength(7);
  });
  test('arithmetic crosses months and years', () => {
    expect(addDays('2026-12-31', 1)).toBe('2027-01-01');
    expect(addDays('2026-03-01', -1)).toBe('2026-02-28');
    expect(monthEnd('2028-02-10')).toBe('2028-02-29');
  });
  test('labels', () => {
    expect(dayLabel('2026-11-10')).toBe('Tue 10 Nov');
  });
});

describe('durations', () => {
  test('parse the common shapes', () => {
    expect(parseDuration('90')).toBe(90);
    expect(parseDuration('90m')).toBe(90);
    expect(parseDuration('1h')).toBe(60);
    expect(parseDuration('1h30m')).toBe(90);
    expect(parseDuration('1h 30m')).toBe(90);
    expect(parseDuration('1:30')).toBe(90);
    expect(parseDuration('1.5h')).toBe(90);
    expect(parseDuration('')).toBeNull();
    expect(parseDuration('soon')).toBeNull();
  });
  test('format', () => {
    expect(formatMinutes(45)).toBe('45m');
    expect(formatMinutes(120)).toBe('2h');
    expect(formatMinutes(535)).toBe('8h 55m');
  });
  test('spans run past midnight', () => {
    expect(spanMinutes('17:00', '18:30')).toBe(90);
    expect(spanMinutes('23:00', '00:15')).toBe(75);
    expect(localStamp('2026-07-10', '00:00', '23:00')).toBe('2026-07-11 00:00');
    expect(localStamp('2026-07-10', '23:30', '23:00')).toBe('2026-07-10 23:30');
    expect(localStamp('2025-11-11', '21:45', '20:30', '-06:00')).toBe('2025-11-11T21:45:00-06:00');
    expect(offsetOf('2025-11-11T20:30:00-06:00')).toBe('-06:00');
    expect(offsetOf('2026-01-01T00:00:00Z')).toBe('Z');
    expect(offsetOf(null)).toBeUndefined();
  });
  test('clock reads the recorded wall time', () => {
    expect(clock('2026-07-27T14:47:57+05:30')).toBe('14:47');
    expect(clock(null)).toBe('');
  });
});
