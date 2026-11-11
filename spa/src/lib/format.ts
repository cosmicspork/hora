/** A short, readable form of a revision or proposal value. */
export function show(value: unknown): string {
  if (value === null || value === undefined) return '∅';
  if (Array.isArray(value)) return value.length ? value.join(', ') : '∅';
  const text = String(value);
  return text.length > 60 ? `${text.slice(0, 57)}…` : text;
}

/** `agent:claude-code` → `claude-code`. */
export function who(actor: string): string {
  return actor.replace(/^agent:/, '');
}
