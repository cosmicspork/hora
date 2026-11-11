<script lang="ts">
  import { untrack } from 'svelte';
  import { api, ApiError, exportUrl, type Grouping, type Project, type Summary } from './lib/api';
  import { hours, monthEnd, monthStart } from './lib/time';

  let { today, projects, version }: { today: string; projects: Project[]; version: number } = $props();

  let from = $state(untrack(() => monthStart(today)));
  let to = $state(untrack(() => monthEnd(today)));
  let by = $state<Grouping>('week');
  let project = $state('');
  let summary = $state<Summary | null>(null);
  let error = $state('');

  const names = $derived(new Map(projects.map((p) => [p.slug, p.name])));

  $effect(() => {
    void version;
    if (!from || !to) return;
    api.summary({ from, to, by, project: project || undefined })
      .then((s) => { summary = s; error = ''; })
      .catch((e) => (error = e instanceof ApiError ? e.message : String(e)));
  });
</script>

<div class="toolbar wrap">
  <h2>Summary</h2>
  <label>From <input type="date" bind:value={from} /></label>
  <label>To <input type="date" bind:value={to} /></label>
  <label>By
    <select bind:value={by}>
      <option value="day">Day</option>
      <option value="week">Week</option>
      <option value="month">Month</option>
    </select>
  </label>
  <label>Project
    <select bind:value={project}>
      <option value="">All</option>
      {#each projects as p (p.slug)}<option value={p.slug}>{p.name}</option>{/each}
    </select>
  </label>
  <span class="spacer"></span>
  <a class="button ghost" href={exportUrl('csv', from, to, project || undefined)} download>CSV</a>
  <a class="button ghost" href={exportUrl('jsonl', from, to, project || undefined)} download>JSONL</a>
</div>
{#if error}<p class="error" role="alert">{error}</p>{/if}

{#if summary}
  <table class="summary">
    <thead>
      <tr><th>Period</th><th>Project</th><th class="num">Billable h</th><th class="num">Other h</th><th class="num">Draft h</th><th class="num">Entries</th></tr>
    </thead>
    <tbody>
      {#each summary.rows as row (`${row.period}/${row.project}`)}
        <tr>
          <td>{row.period}</td>
          <td>{names.get(row.project) ?? row.project}</td>
          <td class="num">{hours(row.billable_minutes)}</td>
          <td class="num">{hours(row.nonbillable_minutes)}</td>
          <td class="num draft-text">{row.draft_minutes ? hours(row.draft_minutes) : ''}</td>
          <td class="num">{row.entries}</td>
        </tr>
      {:else}
        <tr><td colspan="6" class="empty">No time in this range.</td></tr>
      {/each}
    </tbody>
    <tfoot>
      <tr>
        <th colspan="2">Total</th>
        <th class="num">{hours(summary.total.billable_minutes)}</th>
        <th class="num">{hours(summary.total.nonbillable_minutes)}</th>
        <th class="num draft-text">{summary.total.draft_minutes ? hours(summary.total.draft_minutes) : ''}</th>
        <th class="num">{summary.total.entries}</th>
      </tr>
    </tfoot>
  </table>
  {#if summary.total.draft_minutes}
    <p class="muted">Draft hours aren't counted as billable or other until confirmed.</p>
  {/if}
{/if}
