<script lang="ts">
  import { untrack } from 'svelte';
  import { api, ApiError, type Entry, type Project } from './lib/api';
  import { addDays, dayLabel, elapsedMinutes, formatMinutes, weekDays, weekStart } from './lib/time';
  import EntryRow from './EntryRow.svelte';

  let { today, projects, version, onadd, onopen }: {
    today: string;
    projects: Project[];
    version: number;
    onadd: (date: string) => void;
    onopen: (entry: Entry) => void;
  } = $props();

  let start = $state(untrack(() => weekStart(today)));
  let entries = $state<Entry[]>([]);
  let error = $state('');
  const days = $derived(weekDays(start));
  const showProject = $derived(projects.filter((p) => !p.archived).length > 1);

  $effect(() => {
    void version;
    const from = start;
    api.list({ from, to: addDays(from, 6) })
      .then((list) => { entries = list; error = ''; })
      .catch((e) => (error = e instanceof ApiError ? e.message : String(e)));
  });

  function minutesOf(list: Entry[]) {
    let billable = 0, other = 0, draft = 0;
    for (const e of list) {
      const m = e.minutes ?? (e.started_at ? elapsedMinutes(e.started_at) : 0);
      if (e.state === 'draft') draft += m;
      else if (e.billable) billable += m;
      else other += m;
    }
    return { billable, other, draft, all: billable + other + draft };
  }

  const totals = $derived(minutesOf(entries));
</script>

<div class="toolbar">
  <button class="ghost" onclick={() => (start = addDays(start, -7))} aria-label="Previous week">←</button>
  <button class="ghost" onclick={() => (start = weekStart(today))} disabled={start === weekStart(today)}>This week</button>
  <button class="ghost" onclick={() => (start = addDays(start, 7))} aria-label="Next week">→</button>
  <h2>{dayLabel(days[0])} to {dayLabel(days[6])}</h2>
  <span class="spacer"></span>
  <span class="totals">
    <span title="Confirmed billable">{formatMinutes(totals.billable)} billable</span>
    {#if totals.other}<span title="Confirmed non-billable">{formatMinutes(totals.other)} other</span>{/if}
    {#if totals.draft}<span class="draft-text" title="Unconfirmed">{formatMinutes(totals.draft)} draft</span>{/if}
  </span>
</div>
{#if error}<p class="error" role="alert">{error}</p>{/if}

{#each days as day (day)}
  {@const list = entries.filter((e) => e.date === day)}
  <section class="day" class:today={day === today}>
    <header>
      <h3>{dayLabel(day)}</h3>
      {#if list.length}<span class="muted">{formatMinutes(minutesOf(list).all)}</span>{/if}
      <span class="spacer"></span>
      {#if day <= today}<button class="ghost small" onclick={() => onadd(day)}>+ Add</button>{/if}
    </header>
    {#if list.length}
      <ul class="entries">
        {#each list as entry (entry.id)}
          <EntryRow {entry} {showProject} onopen={() => onopen(entry)} />
        {/each}
      </ul>
    {/if}
  </section>
{/each}
