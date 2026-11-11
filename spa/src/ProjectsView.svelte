<script lang="ts">
  import { api, ApiError, type Project } from './lib/api';

  let { projects, onchange }: { projects: Project[]; onchange: () => void } = $props();

  let slug = $state('');
  let name = $state('');
  let client = $state('');
  let billable = $state(true);
  let error = $state('');

  function slugify(text: string) {
    return text.toLowerCase().normalize('NFKD').replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
  }

  async function run(job: () => Promise<unknown>) {
    error = '';
    try {
      await job();
      onchange();
      return true;
    } catch (e) {
      error = e instanceof ApiError ? e.message : String(e);
      return false;
    }
  }

  async function add() {
    const ok = await run(() =>
      api.createProject({ slug: slug || slugify(name), name, client: client || undefined, billable_default: billable })
    );
    if (ok) {
      slug = name = client = '';
      billable = true;
    }
  }
</script>

<div class="toolbar"><h2>Projects</h2></div>
{#if error}<p class="error" role="alert">{error}</p>{/if}

<table class="projects">
  <thead><tr><th>Name</th><th>Slug</th><th>Client</th><th>Billable by default</th><th></th></tr></thead>
  <tbody>
    {#each projects as p (p.slug)}
      <tr class:archived={p.archived}>
        <td>
          <input value={p.name} aria-label="Name"
            onchange={(e) => run(() => api.updateProject({ slug: p.slug, version: p.version, name: e.currentTarget.value }))} />
        </td>
        <td><code>{p.slug}</code></td>
        <td>
          <input value={p.client ?? ''} aria-label="Client"
            onchange={(e) => run(() => api.updateProject({ slug: p.slug, version: p.version, client: e.currentTarget.value }))} />
        </td>
        <td>
          <input type="checkbox" checked={p.billable_default} aria-label="Billable by default"
            onchange={(e) => run(() => api.updateProject({ slug: p.slug, version: p.version, billable_default: e.currentTarget.checked }))} />
        </td>
        <td>
          <button class="ghost small" onclick={() => run(() => api.updateProject({ slug: p.slug, version: p.version, archived: !p.archived }))}>
            {p.archived ? 'Restore' : 'Archive'}
          </button>
        </td>
      </tr>
    {/each}
  </tbody>
</table>

<form class="add-project" onsubmit={(e) => { e.preventDefault(); add(); }}>
  <h3>Add a project</h3>
  <label>Name <input bind:value={name} required placeholder="Acme" /></label>
  <label>Slug <input bind:value={slug} placeholder={slugify(name) || 'acme'} pattern="[a-z0-9]+(-[a-z0-9]+)*" /></label>
  <label>Client <input bind:value={client} /></label>
  <label class="check"><input type="checkbox" bind:checked={billable} /> Billable by default</label>
  <button class="primary">Add</button>
  <p class="muted">The slug is how agents and exports name the project, and it never changes.</p>
</form>
