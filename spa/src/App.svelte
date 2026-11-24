<script lang="ts">
  import { api, ApiError, type AppState, type Entry } from './lib/api';
  import TimerBar from './TimerBar.svelte';
  import WeekView from './WeekView.svelte';
  import DraftsView from './DraftsView.svelte';
  import SummaryView from './SummaryView.svelte';
  import ProjectsView from './ProjectsView.svelte';
  import EntryEditor from './EntryEditor.svelte';
  import { Banner, Empty, Segmented } from '@tracon/ui/svelte';

  type View = 'week' | 'drafts' | 'summary' | 'projects';
  const views: View[] = ['week', 'drafts', 'summary', 'projects'];
  const options: { value: View; label: string }[] = [
    { value: 'week', label: 'Week' },
    { value: 'drafts', label: 'Drafts' },
    { value: 'summary', label: 'Summary' },
    { value: 'projects', label: 'Projects' }
  ];

  function fromHash(): View {
    const name = location.hash.slice(1) as View;
    return views.includes(name) ? name : 'week';
  }

  let view = $state<View>(fromHash());
  let app = $state<AppState | null>(null);
  let error = $state('');
  // Bumped after every change so views reload what they show.
  let version = $state(0);
  let editing = $state<{ entry: Entry | null; date: string } | null>(null);
  const waiting = $derived(app ? app.drafts + app.proposals : 0);

  async function refresh() {
    try {
      app = await api.state();
      error = '';
    } catch (e) {
      error = e instanceof ApiError ? e.message : 'hora is not answering. Is `hora serve` running?';
    }
  }

  function reload() {
    version += 1;
    refresh();
  }

  function changed() {
    editing = null;
    reload();
  }

  $effect(() => {
    refresh();
    const onhash = () => (view = fromHash());
    // Agents change data behind the page; catch up when it regains focus.
    const onfocus = () => reload();
    window.addEventListener('hashchange', onhash);
    window.addEventListener('focus', onfocus);
    // The draft count and running timer change when agents call in.
    const poll = setInterval(refresh, 30_000);
    return () => {
      clearInterval(poll);
      window.removeEventListener('hashchange', onhash);
      window.removeEventListener('focus', onfocus);
    };
  });
</script>

<main>
  {#if error}<Banner tone="crit" role="alert">{error}</Banner>{/if}
  {#if app}
    <TimerBar running={app.running} projects={app.projects} onchange={changed} onopen={(entry) => (editing = { entry, date: entry.date })} />
  {/if}
  <nav class="views">
    <Segmented {options} value={view} label="View" onchange={(v) => (location.hash = v)} />
    {#if waiting}
      <a class="waiting" href="#drafts" title="{app?.drafts} drafts, {app?.proposals} proposed changes">{waiting} waiting on you</a>
    {/if}
  </nav>
  {#if app}
    {#if !app.projects.length && view !== 'projects'}
      <Empty>No projects yet. <a class="lnk" href="#projects">Add one</a> to start.</Empty>
    {:else if view === 'week'}
      <WeekView today={app.today} projects={app.projects} {version}
        onadd={(date) => (editing = { entry: null, date })}
        onopen={(entry) => (editing = { entry, date: entry.date })} />
    {:else if view === 'drafts'}
      <DraftsView today={app.today} projects={app.projects} {version} onchange={changed}
        onopen={(entry) => (editing = { entry, date: entry.date })} />
    {:else if view === 'summary'}
      <SummaryView today={app.today} projects={app.projects} {version} />
    {:else}
      <ProjectsView projects={app.projects} onchange={changed} />
    {/if}
  {:else if !error}
    <Empty loading>Loading…</Empty>
  {/if}
</main>

{#if editing && app}
  {#key editing}
    <EntryEditor entry={editing.entry} date={editing.date} projects={app.projects} onclose={() => (editing = null)} onsaved={changed} />
  {/key}
{/if}
