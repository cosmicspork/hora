<script lang="ts">
  import { api, ApiError, type Entry, type Project } from './lib/api';
  import { clock, elapsedMinutes, formatMinutes } from './lib/time';

  let { running, projects, onchange, onopen }: {
    running: Entry | null;
    projects: Project[];
    onchange: () => void;
    onopen: (entry: Entry) => void;
  } = $props();

  const active = $derived(projects.filter((p) => !p.archived));
  let project = $state('');
  let description = $state('');
  let error = $state('');
  let busy = $state(false);
  let now = $state(Date.now());

  $effect(() => {
    if (!project && active.length) project = active[0].slug;
  });
  $effect(() => {
    const timer = setInterval(() => (now = Date.now()), 15_000);
    return () => clearInterval(timer);
  });

  const elapsed = $derived(running?.started_at ? elapsedMinutes(running.started_at, now) : 0);

  async function act(job: () => Promise<unknown>) {
    error = '';
    busy = true;
    try {
      await job();
      description = '';
      onchange();
    } catch (e) {
      error = e instanceof ApiError ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<section class="timer" class:live={!!running}>
  {#if running}
    <span class="pulse" aria-hidden="true"></span>
    <button class="timer-what" onclick={() => onopen(running)}>
      <strong>{running.project_name}</strong> {running.description}
      <span class="muted">since {clock(running.started_at)}</span>
      {#if running.state === 'draft'}<span class="badge draft">draft · {running.author}</span>{/if}
    </button>
    <span class="elapsed" class:long={elapsed > 600}>{formatMinutes(elapsed)}</span>
    <button class="primary" onclick={() => act(() => api.stop())} disabled={busy}>Stop</button>
  {:else if active.length}
    <form class="timer-start" onsubmit={(e) => { e.preventDefault(); act(() => api.start({ project, description })); }}>
      <select bind:value={project} aria-label="Project">
        {#each active as p (p.slug)}<option value={p.slug}>{p.name}</option>{/each}
      </select>
      <input bind:value={description} placeholder="What are you working on?" required aria-label="Description" />
      <button class="primary" disabled={busy}>Start</button>
    </form>
  {:else}
    <span class="muted">Add a project to start tracking.</span>
  {/if}
  {#if elapsed > 600}<span class="warn">Running over 10 hours. Forgot to stop it? Open it to correct the start, or stop it and fix the end.</span>{/if}
  {#if error}<span class="error" role="alert">{error}</span>{/if}
</section>
