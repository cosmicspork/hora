<script lang="ts">
  import { api, ApiError, type Entry, type Proposal } from './lib/api';
  import { show, who } from './lib/format';
  import { clock, dayLabel, formatMinutes } from './lib/time';

  let { proposal, onopen, onchange }: {
    proposal: Proposal;
    onopen: (entry: Entry) => void;
    onchange: () => void;
  } = $props();

  let note = $state('');
  let busy = $state(false);
  let error = $state('');

  const entry = $derived(proposal.entry);
  const deleting = $derived(proposal.action === 'delete');

  async function decide(accept: boolean) {
    error = '';
    busy = true;
    try {
      const text = note.trim() || undefined;
      await (accept ? api.acceptProposal(proposal.id, text) : api.rejectProposal(proposal.id, text));
      onchange();
    } catch (e) {
      error = e instanceof ApiError ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<article class="proposal" class:stale={proposal.stale}>
  <header>
    <button class="link" onclick={() => onopen(entry)}>
      <strong>{dayLabel(entry.date)}</strong>
      {#if entry.started_at}{clock(entry.started_at)}{entry.ended_at ? `–${clock(entry.ended_at)}` : ''}{/if}
      · {entry.minutes ? formatMinutes(entry.minutes) : 'running'} · {entry.description}
    </button>
    <span class="badge draft" title="Proposed by {proposal.author}">{deleting ? 'delete' : 'change'} · {who(proposal.author)}</span>
  </header>

  {#if deleting}
    <p class="delete-note">Delete this entry. It stays in history.</p>
  {:else}
    <table class="changes">
      <thead><tr><th>Field</th><th>Now</th><th>Proposed</th></tr></thead>
      <tbody>
        {#each Object.entries(proposal.changes) as [field, [before, after]] (field)}
          <tr><td><code>{field}</code></td><td>{show(before)}</td><td><strong>{show(after)}</strong></td></tr>
        {/each}
      </tbody>
    </table>
  {/if}
  <p class="reason">“{proposal.reason}”</p>

  {#if proposal.stale}
    <p class="warn">The entry changed after this was proposed, so it can only be rejected.</p>
  {/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}

  <div class="decide">
    <input bind:value={note} placeholder="Note for the agent (optional)" aria-label="Note" />
    <button class="ghost" disabled={busy} onclick={() => decide(false)}>Reject</button>
    <button class="primary" disabled={busy || proposal.stale} onclick={() => decide(true)}>Accept</button>
  </div>
</article>

<style>
  .proposal { background: var(--paper); border: 1px solid var(--draft); border-radius: 10px; padding: 0.75rem; display: flex; flex-direction: column; gap: 0.5rem; }
  .proposal.stale { border-color: var(--line-strong); }
  header { display: flex; gap: 0.5rem; align-items: baseline; justify-content: space-between; flex-wrap: wrap; }
  .link { border: 0; background: none; padding: 0; text-align: left; color: var(--ink); cursor: pointer; flex: 1; min-width: 0; }
  .link:hover:not(:disabled) { background: none; text-decoration: underline; }
  .changes { font-size: 0.9em; }
  .changes td, .changes th { padding: 0.3rem 0.6rem; }
  .reason { margin: 0; color: var(--muted); font-style: italic; }
  .delete-note { margin: 0; color: var(--danger); }
  .decide { display: flex; gap: 0.5rem; flex-wrap: wrap; }
  .decide input { flex: 1; min-width: 10rem; }
  p.warn { margin: 0; }
</style>
