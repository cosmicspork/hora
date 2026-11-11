<script lang="ts">
  import { untrack } from 'svelte';
  import { api, ApiError, type EditInput, type Entry, type Project, type Revision } from './lib/api';
  import { clock, formatMinutes, localStamp, offsetOf, parseDuration, spanMinutes } from './lib/time';
  import { show } from './lib/format';

  let {
    entry = null,
    date: presetDate,
    projects,
    onclose,
    onsaved
  }: {
    entry?: Entry | null;
    date: string;
    projects: Project[];
    onclose: () => void;
    onsaved: () => void;
  } = $props();

  // The editor is keyed per entry, so it snapshots its props once.
  const active = untrack(() => projects.filter((p) => !p.archived || p.slug === entry?.project));
  const original = untrack(() => ({
    project: entry?.project ?? active[0]?.slug ?? '',
    date: entry?.date ?? presetDate,
    start: clock(entry?.started_at ?? null),
    end: clock(entry?.ended_at ?? null),
    duration: entry?.minutes ? formatMinutes(entry.minutes) : '',
    description: entry?.description ?? '',
    billable: entry?.billable ?? active[0]?.billable_default ?? true,
    refs: (entry?.refs ?? []).join('\n')
  }));

  let project = $state(original.project);
  let date = $state(original.date);
  let start = $state(original.start);
  let end = $state(original.end);
  let duration = $state(original.duration);
  let description = $state(original.description);
  let billable = $state(original.billable);
  let refs = $state(original.refs);
  let error = $state('');
  let busy = $state(false);
  let confirmDelete = $state(false);
  let history = $state<Revision[] | null>(null);

  const running = untrack(() => entry?.running ?? false);
  // Edited times keep the offset the entry was recorded in, not the browser's.
  const offset = untrack(() => offsetOf(entry?.started_at));
  const span = $derived(start && end ? spanMinutes(start, end) : null);

  $effect(() => {
    const current = untrack(() => entry);
    if (current) {
      api.history(current.id).then((revisions) => (history = revisions)).catch(() => (history = []));
    }
  });

  function chooseProject(slug: string) {
    project = slug;
    if (!entry) billable = projects.find((p) => p.slug === slug)?.billable_default ?? billable;
  }

  function refList(): string[] {
    return refs.split(/\s+/).map((r) => r.trim()).filter(Boolean);
  }

  function minutesFromForm(): number | undefined | 'invalid' {
    if (!duration.trim()) return undefined;
    return parseDuration(duration) ?? 'invalid';
  }

  async function save(andConfirm = false) {
    error = '';
    const minutes = minutesFromForm();
    if (minutes === 'invalid') {
      error = 'Duration reads like 90, 1h30m, 1:30 or 1.5h.';
      return;
    }
    if (end && !start) {
      error = 'An end time needs a start time.';
      return;
    }
    busy = true;
    try {
      let saved: Entry;
      if (!entry) {
        if (minutes === undefined && !(start && end)) {
          error = 'Give a duration, or a start and an end.';
          return;
        }
        saved = await api.log({
          project,
          date,
          description,
          billable,
          refs: refList(),
          minutes,
          started_at: start ? localStamp(date, start) : undefined,
          ended_at: end ? localStamp(date, end, start) : undefined
        });
      } else {
        const changes: EditInput = { id: entry.id, version: entry.version };
        if (project !== original.project) changes.project = project;
        if (description !== original.description) changes.description = description;
        if (billable !== original.billable) changes.billable = billable;
        if (refs !== original.refs) changes.refs = refList();
        const timesChanged = start !== original.start || end !== original.end;
        const dateChanged = date !== original.date;
        if (timesChanged || dateChanged) {
          if (!start && !end) {
            if (original.start) changes.clear_times = true;
            changes.date = date;
          } else {
            if (original.end && !end && !running) {
              error = 'Clear both times or keep both.';
              return;
            }
            changes.started_at = localStamp(date, start, undefined, offset);
            if (end && !running) changes.ended_at = localStamp(date, end, start, offset);
          }
        }
        if (duration !== original.duration && minutes !== undefined) changes.minutes = minutes;
        saved = Object.keys(changes).length > 2 ? await api.edit(changes) : entry;
        if (andConfirm) await api.confirm([{ id: saved.id, version: saved.version }]);
      }
      onsaved();
    } catch (e) {
      error = e instanceof ApiError ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  async function remove() {
    if (!entry) return;
    busy = true;
    try {
      await api.remove(entry.id, entry.version);
      onsaved();
    } catch (e) {
      error = e instanceof ApiError ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  function onkeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') onclose();
  }
</script>

<svelte:window {onkeydown} />

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="dialog" role="dialog" aria-modal="true" aria-label={entry ? 'Edit entry' : 'Add entry'}>
    <header>
      <h2>{entry ? (running ? 'Running timer' : 'Edit entry') : 'Add time'}</h2>
      {#if entry}
        <span class="badge {entry.state}">{entry.state}</span>
        <span class="author">by {entry.author}</span>
      {/if}
    </header>

    <form onsubmit={(e) => { e.preventDefault(); save(); }}>
      <div class="grid">
        <label>Project
          <select value={project} onchange={(e) => chooseProject(e.currentTarget.value)}>
            {#each active as p (p.slug)}<option value={p.slug}>{p.name}</option>{/each}
          </select>
        </label>
        <label>Date <input type="date" bind:value={date} required /></label>
        <label>Start <input type="time" bind:value={start} /></label>
        {#if !running}
          <label>End <input type="time" bind:value={end} /></label>
          <label>Duration
            <input bind:value={duration} placeholder={span ? formatMinutes(span) : '1h30m'} />
          </label>
        {/if}
        <label class="check"><input type="checkbox" bind:checked={billable} /> Billable</label>
      </div>
      <label>Description <textarea bind:value={description} rows="3" required></textarea></label>
      <label>Refs <span class="hint">workboard:&lt;id&gt;, pr:repo#12, commit:repo@sha, jira:KEY, or a URL; one per line</span>
        <textarea bind:value={refs} rows="2" class="mono"></textarea>
      </label>

      {#if entry?.pending_proposals}
        <p class="warn">An agent proposed a change to this entry. <a href="#drafts" onclick={onclose}>Review it in Drafts</a>; saving here makes it stale.</p>
      {/if}
      {#if entry?.warnings?.length}
        <ul class="warnings">
          {#each entry.warnings as w, i (i)}<li>{w.message}</li>{/each}
        </ul>
      {/if}
      {#if error}<p class="error" role="alert">{error}</p>{/if}

      <div class="actions">
        {#if entry && !confirmDelete}
          <button type="button" class="danger ghost" onclick={() => (confirmDelete = true)} disabled={busy}>Delete</button>
        {:else if entry}
          <button type="button" class="danger" onclick={remove} disabled={busy}>Really delete</button>
          <button type="button" class="ghost" onclick={() => (confirmDelete = false)}>Keep</button>
        {/if}
        <span class="spacer"></span>
        <button type="button" class="ghost" onclick={onclose}>Cancel</button>
        {#if entry?.state === 'draft' && !running}
          <button type="button" onclick={() => save(true)} disabled={busy}>Save and confirm</button>
        {/if}
        <button type="submit" class="primary" disabled={busy}>Save</button>
      </div>
    </form>

    {#if entry}
      <section class="history">
        <h3>History</h3>
        {#if history === null}
          <p class="muted">Loading…</p>
        {:else}
          <ol>
            {#each history as r (r.id)}
              <li>
                <div><strong>{r.action}</strong> by {r.actor}{#if r.proposed_by}, proposed by {r.proposed_by}{/if} <span class="muted">via {r.via} · {new Date(r.at).toLocaleString()}</span></div>
                {#if r.action !== 'create' && r.action !== 'start'}
                  <ul class="changes">
                    {#each Object.entries(r.changes) as [field, [before, after]] (field)}
                      <li><code>{field}</code> {show(before)} → {show(after)}</li>
                    {/each}
                  </ul>
                {/if}
                {#if r.reason}<div class="reason">“{r.reason}”</div>{/if}
              </li>
            {/each}
          </ol>
        {/if}
      </section>
    {/if}
  </div>
</div>
