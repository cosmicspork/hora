<script lang="ts">
  import { api, ApiError, type Entry, type Proposal } from './lib/api';
  import { show, who } from './lib/format';
  import { clock, dayLabel, formatMinutes } from './lib/time';
  import { Banner, Row } from '@tracon/ui/svelte';

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

<!-- Waiting on you: an amber bar and wash, as every proposal in Tracon. A
     stale one can only be rejected, so it drops to the quiet treatment. -->
<article class="proposal" class:stale={proposal.stale}>
  <span class="bar" aria-hidden="true"></span>
  <div class="body">
    <header>
      <Row onclick={() => onopen(entry)} title="Open the entry">
        <span class="what">
          <strong>{dayLabel(entry.date)}</strong>
          {#if entry.started_at}<span class="mono-text">{clock(entry.started_at)}{entry.ended_at ? `–${clock(entry.ended_at)}` : ''}</span>{/if}
          · {entry.minutes ? formatMinutes(entry.minutes) : 'running'} · {entry.description}
        </span>
      </Row>
      <span class="badge draft" title="Proposed by {proposal.author}{proposal.session ? ` in Tracon session ${proposal.session}` : ''}">{deleting ? 'delete' : 'change'} · {who(proposal.author)}</span>
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
      <Banner tone="dim">The entry changed after this was proposed, so it can only be rejected.</Banner>
    {/if}
    {#if error}<Banner tone="crit" role="alert">{error}</Banner>{/if}

    <div class="decide">
      <input bind:value={note} placeholder="Note for the agent (optional)" aria-label="Note" />
      <button class="lnk d" disabled={busy} onclick={() => decide(false)}>Reject</button>
      <button class="btn" disabled={busy || proposal.stale} onclick={() => decide(true)}>Accept</button>
    </div>
  </div>
</article>

<style>
  .proposal {
    display: grid;
    grid-template-columns: 3px minmax(0, 1fr);
    background: linear-gradient(90deg, var(--wash-wait), var(--s1) 42%);
    border-radius: 4px;
    overflow: hidden;
  }
  .bar { background: var(--wait); }
  .proposal.stale { background: linear-gradient(90deg, var(--wash-dim), var(--s1) 42%); }
  .proposal.stale .bar { background: var(--dim); }
  .body { display: flex; flex-direction: column; gap: 8px; padding: 4px 14px 12px 0; min-width: 0; }
  header { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  header :global(.tr-line) { flex: 1; min-width: 0; }
  header :global(.tr-row) { background: transparent; padding: 8px 0 8px 14px; }
  header :global(.tr-row:hover) { background: transparent; text-decoration: underline; }
  .what { overflow-wrap: anywhere; }
  .mono-text { font-family: var(--mono); font-size: 12.5px; color: var(--ink2); }
  .changes, .reason, .delete-note, .decide { margin-left: 14px; }
  .changes { font-size: 13px; background: transparent; width: calc(100% - 14px); }
  .changes td, .changes th { padding: 4px 10px 4px 0; }
  .reason { margin-top: 0; margin-bottom: 0; color: var(--ink2); font-style: italic; }
  .delete-note { margin-top: 0; margin-bottom: 0; color: var(--crit); }
  .decide { display: flex; gap: 14px; flex-wrap: wrap; align-items: center; }
  .decide input { flex: 1; min-width: 10rem; }
  .proposal > .body > :global(.banner) { margin-left: 14px; }
</style>
