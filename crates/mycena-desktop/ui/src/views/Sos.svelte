<script>
  // SPDX-License-Identifier: AGPL-3.0-or-later
  // Copyright (C) 2026 Mycena Project
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { onMount } from 'svelte';

  const CRISIS_MODES = [
    { value: 0, label: 'Follow auto (internet + nearby SOS)' },
    { value: 1, label: 'Force on' },
    { value: 2, label: 'Force off' },
  ];

  const RESOURCE_CATEGORIES = [
    { value: 'water', label: 'Water' },
    { value: 'food', label: 'Food' },
    { value: 'shelter', label: 'Shelter' },
    { value: 'medicine', label: 'Medicine' },
    { value: 'power', label: 'Power' },
  ];

  let listenSos = $state(false);
  let crisisMode = $state(0);
  let crisisActive = $state(false);
  let geohash = $state('');
  let message = $state('');
  let resourceCategory = $state('water');
  let resourceDescription = $state('');
  let volunteerSlots = $state(2);
  let volunteerDescription = $state('');
  let volunteerTasks = $state([]);
  let posts = $state([]);
  let resources = $state([]);
  let saved = $state(false);
  let posting = $state(false);

  async function refreshCrisisAuto() {
    const settings = await invoke('get_settings');
    const internet = settings.internet_enabled !== false;
    const prefix = geohash.trim().length >= 4 ? geohash.trim().slice(0, 4) : null;
    await invoke('refresh_crisis_auto', {
      internetReachable: internet,
      localGeohashPrefix: prefix,
    });
    crisisActive = await invoke('is_crisis_mode_active');
  }

  async function refresh() {
    posts = await invoke('list_sos_posts');
    resources = await invoke('list_resource_map_entries');
    volunteerTasks = await invoke('list_volunteer_tasks');
    const settings = await invoke('get_settings');
    listenSos = settings.listen_sos ?? false;
    crisisActive = await invoke('is_crisis_mode_active');
    await refreshCrisisAuto();
  }

  async function saveListen() {
    await invoke('set_listen_sos', { enabled: listenSos });
    saved = true;
    setTimeout(() => (saved = false), 1500);
    await refreshCrisisAuto();
  }

  async function saveCrisisMode() {
    await invoke('set_crisis_manual_mode', { mode: crisisMode });
    crisisActive = await invoke('is_crisis_mode_active');
  }

  async function postSos() {
    if (!geohash.trim() || !message.trim()) return;
    posting = true;
    try {
      await invoke('post_sos', {
        geohash: geohash.trim(),
        message: message.trim(),
        ttlSecs: 3600,
      });
      message = '';
      await refresh();
    } finally {
      posting = false;
    }
  }

  async function postResource() {
    if (!geohash.trim() || !resourceDescription.trim()) return;
    posting = true;
    try {
      await invoke('post_resource_map', {
        geohash: geohash.trim(),
        category: resourceCategory,
        description: resourceDescription.trim(),
        ttlSecs: 21600,
      });
      resourceDescription = '';
      await refresh();
    } finally {
      posting = false;
    }
  }

  async function postVolunteerTask() {
    if (!geohash.trim() || !volunteerDescription.trim()) return;
    posting = true;
    try {
      await invoke('post_volunteer_task', {
        geohash: geohash.trim(),
        category: resourceCategory,
        description: volunteerDescription.trim(),
        slotsNeeded: volunteerSlots,
        ttlSecs: 21600,
      });
      volunteerDescription = '';
      await refresh();
    } finally {
      posting = false;
    }
  }

  async function claimVolunteerTask(taskId) {
    posting = true;
    try {
      await invoke('claim_volunteer_task', { taskId });
      await refresh();
    } finally {
      posting = false;
    }
  }

  function openMeshAid() {
    window.dispatchEvent(
      new CustomEvent('mycena-open-miniapp', {
        detail: { appId: 'io.github.mycena_project.meshaid' },
      }),
    );
  }

  onMount(() => {
    refresh();
    const unsubs = [];
    listen('sos-updated', refresh).then((fn) => unsubs.push(fn));
    listen('settings-updated', refresh).then((fn) => unsubs.push(fn));
    const interval = setInterval(refreshCrisisAuto, 30_000);
    return () => {
      unsubs.forEach((fn) => fn());
      clearInterval(interval);
    };
  });
</script>

<div class="sos">
  <header>
    <h3>Emergency SOS</h3>
    <p class="disclaimer">
      Broadcast a short help request to nearby mesh peers. <strong>Not a replacement for emergency services.</strong>
    </p>
  </header>

  <section>
    <h4>Crisis mode</h4>
    {#if crisisActive}
      <p class="hint crisis-active">Crisis mode active — emergency traffic prioritized; background sync paused.</p>
    {/if}
    <label for="crisis-mode">Mode</label>
    <select id="crisis-mode" bind:value={crisisMode} onchange={saveCrisisMode}>
      {#each CRISIS_MODES as m}
        <option value={m.value}>{m.label}</option>
      {/each}
    </select>
    <p class="hint">Auto activates when internet is down, SOS listening is on, and nearby SOS posts exist.</p>
  </section>

  <section>
    <h4>Listen for nearby SOS</h4>
    <label class="checkbox-row">
      <input type="checkbox" bind:checked={listenSos} onchange={saveListen} />
      Opt in to receive SOS and resource-map posts (default off)
    </label>
    {#if saved}
      <p class="hint ok">Listening preference saved.</p>
    {/if}
  </section>

  <section>
    <h4>Post SOS</h4>
    <label for="sos-geohash">Geohash (approximate area, 4–12 chars)</label>
    <input id="sos-geohash" bind:value={geohash} placeholder="u4pru" maxlength="12" />
    <label for="sos-message">Short message</label>
    <textarea
      id="sos-message"
      bind:value={message}
      placeholder="Need help nearby — describe briefly"
      maxlength="280"
      rows="3"
    ></textarea>
    <button type="button" class="primary" disabled={posting} onclick={postSos}>
      {posting ? 'Posting…' : 'Post SOS (1 hour)'}
    </button>
  </section>

  <section>
    <h4>Nearby SOS ({posts.length})</h4>
    {#if !listenSos}
      <p class="hint">Enable listening above to receive posts from the mesh.</p>
    {:else if posts.length === 0}
      <p class="hint">No SOS posts stored yet.</p>
    {:else}
      <ul class="posts">
        {#each posts as p}
          <li>
            <strong>{p.geohash}</strong>
            <span>{p.message}</span>
            <span class="trust trust-{p.trust?.status ?? 'unverified'}">
              {p.trust?.display_label ?? '1 source(s), unverified'} · trust {Math.round((p.trust?.trust_score ?? 0) * 100)}%
            </span>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section>
    <h4>Resource map</h4>
    <p class="hint">Report water, food, shelter, medicine, or power nearby (6 h TTL).</p>
    <label for="resource-category">Category</label>
    <select id="resource-category" bind:value={resourceCategory}>
      {#each RESOURCE_CATEGORIES as c}
        <option value={c.value}>{c.label}</option>
      {/each}
    </select>
    <label for="resource-desc">Description</label>
    <textarea
      id="resource-desc"
      bind:value={resourceDescription}
      placeholder="Fountain at plaza, open shelter, etc."
      maxlength="200"
      rows="2"
    ></textarea>
    <button type="button" class="primary" disabled={posting} onclick={postResource}>
      {posting ? 'Posting…' : 'Report resource (6 hours)'}
    </button>
  </section>

  <section>
    <h4>Nearby resources ({resources.length})</h4>
    {#if !listenSos}
      <p class="hint">Enable listening above to receive resource pins.</p>
    {:else if resources.length === 0}
      <p class="hint">No resource pins stored yet.</p>
    {:else}
      <ul class="posts">
        {#each resources as r}
          <li>
            <strong>{r.category} @ {r.geohash}</strong>
            <span>{r.description}</span>
            <span class="trust trust-{r.trust?.status ?? 'unverified'}">
              {r.trust?.display_label ?? '1 source(s), unverified'} · trust {Math.round((r.trust?.trust_score ?? 0) * 100)}%
            </span>
            <span class="hint">Expires {new Date(Number(r.expires_at_ms)).toLocaleString()}</span>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section>
    <h4>Volunteer tasks</h4>
    <p class="hint">Request help at a resource pin (e.g. staffing a shelter). Peers claim open slots.</p>
    <label for="volunteer-slots">Volunteers needed</label>
    <input id="volunteer-slots" type="number" min="1" max="50" bind:value={volunteerSlots} />
    <label for="volunteer-desc">Task description</label>
    <textarea
      id="volunteer-desc"
      bind:value={volunteerDescription}
      placeholder="Need 2 people to distribute water"
      maxlength="200"
      rows="2"
    ></textarea>
    <button type="button" class="primary" disabled={posting} onclick={postVolunteerTask}>
      {posting ? 'Posting…' : 'Post volunteer task (12 hours)'}
    </button>
  </section>

  <section>
    <h4>Open volunteer tasks ({volunteerTasks.length})</h4>
    {#if !listenSos}
      <p class="hint">Enable listening above to receive volunteer tasks.</p>
    {:else if volunteerTasks.length === 0}
      <p class="hint">No volunteer tasks stored yet.</p>
    {:else}
      <ul class="posts">
        {#each volunteerTasks as t}
          <li>
            <strong>{t.category} @ {t.geohash}</strong>
            <span>{t.description}</span>
            <span class="hint">
              {t.claims_count}/{t.slots_needed} claimed · {t.slots_open} open
            </span>
            {#if t.slots_open > 0}
              <button type="button" class="secondary" disabled={posting} onclick={() => claimVolunteerTask(t.task_id)}>
                Claim slot
              </button>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section class="meshaid">
    <h4>Coordination threads</h4>
    <p class="hint">
      MeshAid remains available for structured help requests (medical, resolved threads, etc.).
    </p>
    <button type="button" class="secondary" onclick={openMeshAid}>Open MeshAid mini-app</button>
  </section>
</div>

<style>
  .sos {
    padding: 16px;
    max-width: 640px;
    display: flex;
    flex-direction: column;
    gap: 16px;
    color: var(--text);
  }
  header h3 {
    margin: 0 0 8px;
    font-size: 18px;
  }
  .disclaimer {
    margin: 0;
    font-size: 13px;
    color: var(--text-secondary);
  }
  section {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 16px;
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  h4 {
    margin: 0;
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-secondary);
  }
  label {
    font-size: 13px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .checkbox-row {
    flex-direction: row;
    align-items: center;
    gap: 8px;
  }
  input,
  textarea,
  select {
    padding: 8px 10px;
    border-radius: 6px;
    border: 1px solid var(--border);
    background: var(--surface);
    color: var(--text);
    font: inherit;
  }
  .hint {
    margin: 0;
    font-size: 12px;
    color: var(--text-muted);
  }
  .hint.ok {
    color: var(--accent);
  }
  .trust {
    display: block;
    font-size: 12px;
    margin-top: 2px;
  }
  .trust-confirmed {
    color: var(--accent);
  }
  .trust-conflicting {
    color: #c62828;
  }
  .trust-partial {
    color: #ef6c00;
  }
  .trust-unverified {
    color: var(--text-muted);
  }
  .hint.crisis-active {
    color: #dc2626;
    font-weight: 600;
  }
  button {
    align-self: flex-start;
    padding: 8px 14px;
    border-radius: 6px;
    border: none;
    cursor: pointer;
    font-size: 13px;
  }
  button.primary {
    background: #dc2626;
    color: white;
  }
  button.secondary {
    background: var(--surface);
    border: 1px solid var(--border);
    color: var(--text);
  }
  button:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }
  .posts {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .posts li {
    padding: 10px;
    border-radius: 6px;
    background: var(--surface);
    border: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .posts strong {
    font-family: ui-monospace, monospace;
    font-size: 12px;
  }
  .meshaid {
    border-color: var(--border);
  }
</style>
