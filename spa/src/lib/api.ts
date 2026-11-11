export type EntryState = 'draft' | 'confirmed';
export type Grouping = 'day' | 'week' | 'month';

export interface Warning { code: string; message: string; entry?: string }

export interface Entry {
  id: string;
  project: string;
  project_name: string;
  date: string;
  started_at: string | null;
  ended_at: string | null;
  minutes: number | null;
  running: boolean;
  description: string;
  billable: boolean;
  state: EntryState;
  author: string;
  refs: string[];
  import_key: string | null;
  deleted: boolean;
  version: number;
  created_at: string;
  updated_at: string;
  warnings?: Warning[];
  /** Agents' proposals waiting on the operator; absent when none. */
  pending_proposals?: number;
}

export interface Project {
  slug: string;
  name: string;
  client: string | null;
  billable_default: boolean;
  archived: boolean;
  version: number;
}

export interface AppState {
  today: string;
  utc_offset: string;
  projects: Project[];
  running: Entry | null;
  drafts: number;
  proposals: number;
}

export interface Revision {
  id: number;
  version: number;
  actor: string;
  via: string;
  action: string;
  changes: Record<string, [unknown, unknown]>;
  reason: string | null;
  at: string;
  proposal_id?: string;
  proposed_by?: string;
}

export type ProposalState = 'pending' | 'accepted' | 'rejected' | 'withdrawn';

export interface Proposal {
  id: string;
  entry_id: string;
  entry_version: number;
  action: 'update' | 'delete';
  changes: Record<string, [unknown, unknown]>;
  reason: string;
  author: string;
  state: ProposalState;
  note: string | null;
  decided_by: string | null;
  created_at: string;
  decided_at: string | null;
  /** The entry changed since; it can only be rejected. */
  stale: boolean;
  entry: Entry;
}

export interface Totals { billable_minutes: number; nonbillable_minutes: number; draft_minutes: number; entries: number }
export interface SummaryRow extends Totals { period: string; project: string }
export interface Summary { from: string; to: string; by: Grouping; rows: SummaryRow[]; total: Totals }

export interface LogInput {
  project: string;
  date?: string;
  started_at?: string;
  ended_at?: string;
  minutes?: number;
  description: string;
  billable?: boolean;
  refs?: string[];
}

export interface EditInput {
  id: string;
  version: number;
  project?: string;
  date?: string;
  started_at?: string;
  ended_at?: string;
  clear_times?: boolean;
  minutes?: number;
  description?: string;
  billable?: boolean;
  refs?: string[];
}

export class ApiError extends Error {
  constructor(public readonly code: string, message: string, public readonly status: number) {
    super(message);
  }
}

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const response = await fetch(path, {
    ...init,
    headers: { ...(init.body ? { 'Content-Type': 'application/json' } : {}), ...init.headers }
  });
  if (!response.ok) {
    let code = 'internal_error';
    let message = `Request failed (${response.status})`;
    try {
      const body = (await response.json()) as { error?: string; message?: string };
      code = body.error ?? code;
      message = body.message ?? message;
    } catch {
      // Not JSON; keep the generic message.
    }
    throw new ApiError(code, message, response.status);
  }
  return (await response.json()) as T;
}

function op<T>(name: string, args: object = {}): Promise<T> {
  return request<T>(`/api/ops/${name}`, { method: 'POST', body: JSON.stringify(args) });
}

export const api = {
  state: () => request<AppState>('/api/state'),
  list: (args: { from?: string; to?: string; project?: string; state?: EntryState }) => op<Entry[]>('time_list', args),
  history: (id: string) => op<Revision[]>('time_history', { id }),
  summary: (args: { from: string; to: string; project?: string; by: Grouping }) => op<Summary>('time_summary', args),
  log: (input: LogInput) => op<Entry>('time_log', input),
  edit: (input: EditInput) => op<Entry>('time_edit', input),
  remove: (id: string, version: number) => op<Entry>('time_delete', { id, version }),
  proposals: (args: { state?: ProposalState; entry?: string } = {}) => op<Proposal[]>('time_proposals', args),
  acceptProposal: (id: string, note?: string) => op<Proposal>('proposal_accept', { id, note }),
  rejectProposal: (id: string, note?: string) => op<Proposal>('proposal_reject', { id, note }),
  confirm: (entries: { id: string; version: number }[]) => op<Entry[]>('time_confirm', { entries }),
  start: (input: { project: string; description: string; started_at?: string }) => op<Entry>('time_start', input),
  stop: (input: { description?: string; ended_at?: string } = {}) => op<Entry>('time_stop', input),
  createProject: (input: { slug: string; name: string; client?: string; billable_default?: boolean }) =>
    op<Project>('project_create', input),
  updateProject: (input: { slug: string; version: number; name?: string; client?: string; billable_default?: boolean; archived?: boolean }) =>
    op<Project>('project_update', input)
};

export function exportUrl(format: 'csv' | 'jsonl', from: string, to: string, project?: string): string {
  const params = new URLSearchParams({ format, from, to });
  if (project) params.set('project', project);
  return `/api/export?${params}`;
}
