<script lang="ts">
  import { SvelteSet } from 'svelte/reactivity';
  import { api, ApiError, type Entry, type Project, type Proposal } from './lib/api';
  import { dayLabel, formatMinutes } from './lib/time';
  import EntryRow from './EntryRow.svelte';
  import ProposalCard from './ProposalCard.svelte';

  let { today, projects, version, onopen, onchange }: {
    today: string;
    projects: Project[];
    version: number;
    onopen: (entry: Entry) => void;
    onchange: () => void;
  } = $props();

  let drafts = $state<Entry[]>([]);
  let proposals = $state<Proposal[]>([]);
  let loaded = $state(false);
  let error = $state('');
  let busy = $state(false);
  const selected = new SvelteSet<string>();
  const showProject = $derived(projects.length > 1);
  const confirmable = $derived(drafts.filter((d) => !d.running));
  const dates = $derived([...new Set(drafts.map((d) => d.date))]);
  const selectedMinutes = $derived(
    drafts.filter((d) => selected.has(d.id)).reduce((sum, d) => sum + (d.minutes ?? 0), 0)
  );

  $effect(() => {
    void version;
    api.proposals({ state: 'pending' })
      .then((list) => (proposals = list))
      .catch((e) => (error = e instanceof ApiError ? e.message : String(e)));
    api.list({ from: '2000-01-01', to: today, state: 'draft' })
      .then((list) => {
        drafts = list;
        loaded = true;
        for (const id of [...selected]) if (!list.some((d) => d.id === id)) selected.delete(id);
      })
      .catch((e) => (error = e instanceof ApiError ? e.message : String(e)));
  });

  function toggleAll(checked: boolean) {
    selected.clear();
    if (checked) for (const d of confirmable) selected.add(d.id);
  }

  async function confirmSelected() {
    error = '';
    busy = true;
    try {
      await api.confirm(drafts.filter((d) => selected.has(d.id)).map((d) => ({ id: d.id, version: d.version })));
      selected.clear();
      onchange();
    } catch (e) {
      error = e instanceof ApiError ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<div class="toolbar">
  <h2>Drafts</h2>
  <span class="muted">Entries agents proposed. Check them against their refs, edit what's off, then confirm.</span>
  <span class="spacer"></span>
  {#if confirmable.length}
    <label class="check">
      <input type="checkbox" checked={selected.size > 0 && selected.size === confirmable.length} onchange={(e) => toggleAll(e.currentTarget.checked)} />
      All
    </label>
    <button class="primary" disabled={!selected.size || busy} onclick={confirmSelected}>
      Confirm {selected.size || ''}{selected.size ? ` (${formatMinutes(selectedMinutes)})` : ''}
    </button>
  {/if}
</div>
{#if error}<p class="error" role="alert">{error}</p>{/if}
{#if proposals.length}
  <section class="proposals">
    <h3>Proposed changes <span class="muted">to confirmed entries. Accepting applies the change as your edit.</span></h3>
    {#each proposals as proposal (proposal.id)}
      <ProposalCard {proposal} {onopen} {onchange} />
    {/each}
  </section>
{/if}
{#if loaded && !drafts.length}<p class="empty">No drafts waiting.</p>{/if}

{#each dates as day (day)}
  <section class="day">
    <header><h3>{dayLabel(day)} <span class="muted">{day.slice(0, 4)}</span></h3></header>
    <ul class="entries">
      {#each drafts.filter((d) => d.date === day) as entry (entry.id)}
        <EntryRow
          {entry}
          {showProject}
          selectable={!entry.running}
          selected={selected.has(entry.id)}
          onselect={(checked) => (checked ? selected.add(entry.id) : selected.delete(entry.id))}
          onopen={() => onopen(entry)}
        />
      {/each}
    </ul>
  </section>
{/each}

<style>
  .proposals { display: flex; flex-direction: column; gap: 0.6rem; margin-bottom: 1rem; }
  .proposals h3 { margin: 0; font-size: 1rem; }
  .proposals h3 .muted { font-weight: normal; font-size: 0.9em; }
</style>
