<script lang="ts">
  import { api, ApiError, type AppState, type Entry } from './lib/api';
  import TimerBar from './TimerBar.svelte';
  import WeekView from './WeekView.svelte';
  import DraftsView from './DraftsView.svelte';
  import SummaryView from './SummaryView.svelte';
  import ProjectsView from './ProjectsView.svelte';
  import EntryEditor from './EntryEditor.svelte';

  type View = 'week' | 'drafts' | 'summary' | 'projects';
  const views: View[] = ['week', 'drafts', 'summary', 'projects'];

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

<header class="top">
  <a class="brand" href="#week"><img src="/hora.svg" alt="" width="22" height="22" /> hora</a>
  <nav>
    <a href="#week" class:active={view === 'week'}>Week</a>
    <a href="#drafts" class:active={view === 'drafts'}>Drafts{#if app && app.drafts + app.proposals}<span class="count" title="{app.drafts} drafts, {app.proposals} proposed changes">{app.drafts + app.proposals}</span>{/if}</a>
    <a href="#summary" class:active={view === 'summary'}>Summary</a>
    <a href="#projects" class:active={view === 'projects'}>Projects</a>
  </nav>
</header>

<main>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if app}
    <TimerBar running={app.running} projects={app.projects} onchange={changed} onopen={(entry) => (editing = { entry, date: entry.date })} />
    {#if !app.projects.length && view !== 'projects'}
      <p class="empty">No projects yet. <a href="#projects">Add one</a> to start.</p>
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
  {/if}
</main>

{#if editing && app}
  {#key editing}
    <EntryEditor entry={editing.entry} date={editing.date} projects={app.projects} onclose={() => (editing = null)} onsaved={changed} />
  {/key}
{/if}
