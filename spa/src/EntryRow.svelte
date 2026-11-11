<script lang="ts">
  import type { Entry } from './lib/api';
  import { clock, elapsedMinutes, foreignOffset, formatMinutes } from './lib/time';

  let {
    entry,
    showProject,
    selectable = false,
    selected = false,
    onselect,
    onopen
  }: {
    entry: Entry;
    showProject: boolean;
    selectable?: boolean;
    selected?: boolean;
    onselect?: (checked: boolean) => void;
    onopen: () => void;
  } = $props();

  const offset = $derived(foreignOffset(entry.started_at));
  // "No start time" is common for imported or quick entries; the editor still shows it.
  const warnings = $derived((entry.warnings ?? []).filter((w) => w.code !== 'untimed'));
  const minutes = $derived(entry.minutes ?? (entry.started_at ? elapsedMinutes(entry.started_at) : 0));
</script>

<li class="entry" class:draft={entry.state === 'draft'} class:running={entry.running}>
  {#if selectable}
    <input type="checkbox" checked={selected} aria-label="Select" onchange={(e) => onselect?.(e.currentTarget.checked)} />
  {/if}
  <button class="row" onclick={onopen}>
    <span class="when">
      {#if entry.started_at}
        {clock(entry.started_at)}{entry.ended_at ? `–${clock(entry.ended_at)}` : entry.running ? '–now' : ''}
        {#if offset}<span class="offset" title="Recorded at UTC{offset}">{offset}</span>{/if}
      {:else}
        <span class="muted">no time</span>
      {/if}
    </span>
    <span class="minutes">{formatMinutes(minutes)}</span>
    <span class="what">
      {#if showProject}<span class="project">{entry.project_name}</span>{/if}
      {entry.description}
      {#each entry.refs as ref (ref)}<span class="ref">{ref}</span>{/each}
    </span>
    <span class="flags">
      {#if !entry.billable}<span class="tag">non-billable</span>{/if}
      {#if entry.state === 'draft'}<span class="badge draft" title="by {entry.author}">draft · {entry.author.replace(/^agent:/, '')}</span>{/if}
      {#if entry.pending_proposals}<span class="badge draft" title="An agent proposed a change; review it in Drafts">proposal{entry.pending_proposals > 1 ? `s ${entry.pending_proposals}` : ''}</span>{/if}
      {#if warnings.length}<span class="warn" title={warnings.map((w) => w.message).join('\n')}>⚠ {warnings.length}</span>{/if}
    </span>
  </button>
</li>
