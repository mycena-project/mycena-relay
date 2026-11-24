<script>
  // SPDX-License-Identifier: AGPL-3.0-or-later
  // Copyright (C) 2026 Mycena Project
  import { invoke } from '@tauri-apps/api/core';
  import { getVersion } from '@tauri-apps/api/app';
  import { listen } from '@tauri-apps/api/event';
  import { onMount } from 'svelte';
  import { loadDataHandlingCopy } from '../lib/dataHandlingCopy.js';
  import DataHandling from './DataHandling.svelte';

  let { peerId } = $props();

  const dataHandlingCopy = loadDataHandlingCopy();
  let showDataHandling = $state(false);

  let displayName = $state('');
  let energyState = $state('Active');
  let bootstrapPeers = $state('');
  let rendezvousEnabled = $state(false);
  let internetEnabled = $state(true);
  let maxConnectivity = $state(false);
  let bleBootstrap = $state(false);
  let connectivityMode = $state("internet");
  let connectivityDetail = $state("");
  let accountId = $state("");
  let transportEpoch = $state(0);
  let crowdMode = $state(false);
  let localGeohashPrefix = $state('');
  let crowdDensityActive = $state(false);
  let storeStats = $state({ count: 0, oldest_ms: 0 });
  let gcResult = $state(null);
  let saved = $state(false);
  let appVersion = $state('');
  let dangerError = $state('');
  let networkError = $state('');
  let showTechnicalIds = $state(false);

  const energyOptions = ['Active', 'Intermittent', 'Passive'];

  async function load() {
    const settings = await invoke('get_settings');
    displayName = settings.display_name ?? '';
    energyState = settings.energy_state ?? 'Active';
    rendezvousEnabled = settings.rendezvous_enabled ?? true;
    internetEnabled = settings.internet_enabled ?? true;
    maxConnectivity = settings.max_connectivity ?? false;
    bleBootstrap = settings.ble_bootstrap ?? false;
    connectivityMode = settings.connectivity_mode ?? "internet";
    connectivityDetail = settings.connectivity_detail ?? "";
    accountId = settings.account_id ?? "";
    transportEpoch = settings.transport_epoch ?? 0;
    crowdMode = settings.crowd_mode ?? false;
    localGeohashPrefix = settings.local_geohash_prefix ?? '';
    const m = await invoke('get_metrics');
    crowdDensityActive = m.crowd_density_active ?? false;
    storeStats = await invoke('get_store_stats');
    const custom = await invoke('get_custom_bootstrap_peers');
    bootstrapPeers = custom.join('\n');
  }

  /** Network toggles apply immediately (same as Android). */
  async function applyInternet(enabled) {
    networkError = '';
    try {
      await invoke('set_internet_enabled', { enabled });
      if (!enabled) rendezvousEnabled = false;
    } catch (e) {
      networkError = e instanceof Error ? e.message : String(e);
      internetEnabled = !enabled;
    }
  }

  async function applyRendezvous(enabled) {
    networkError = '';
    try {
      await invoke('set_rendezvous_enabled', { enabled });
    } catch (e) {
      networkError = e instanceof Error ? e.message : String(e);
      rendezvousEnabled = !enabled;
    }
  }

  async function applyCrowdMode(enabled) {
    networkError = '';
    try {
      await invoke('set_crowd_mode', { enabled });
    } catch (e) {
      networkError = e instanceof Error ? e.message : String(e);
      crowdMode = !enabled;
    }
  }

  async function applyMaxConnectivity(enabled) {
    networkError = '';
    try {
      await invoke('set_max_connectivity', { enabled });
      if (enabled) {
        internetEnabled = true;
        rendezvousEnabled = true;
        maxConnectivity = true;
      }
    } catch (e) {
      networkError = e instanceof Error ? e.message : String(e);
    }
  }

  async function applyBleBootstrap(enabled) {
    networkError = '';
    try {
      await invoke('set_ble_bootstrap', { enabled });
      bleBootstrap = enabled;
    } catch (e) {
      networkError = e instanceof Error ? e.message : String(e);
      bleBootstrap = !enabled;
    }
  }

  async function applyLocalGeohash() {
    networkError = '';
    try {
      const trimmed = localGeohashPrefix.trim();
      await invoke('set_local_geohash_prefix', {
        prefix: trimmed.length >= 4 ? trimmed : null,
      });
    } catch (e) {
      networkError = e instanceof Error ? e.message : String(e);
    }
  }

  async function save() {
    await invoke('set_display_name', { name: displayName });
    await invoke('set_energy_state', { energyState });
    await invoke('set_custom_bootstrap_peers', {
      peers: bootstrapPeers
        .split('\n')
        .map(s => s.trim())
        .filter(Boolean)
    });
    saved = true;
    setTimeout(() => saved = false, 2000);
  }

  async function runGc() {
    gcResult = await invoke('run_gc');
  }

  async function panicWipe() {
    dangerError = '';
    const confirmed = window.confirm(
      'This will immediately delete ALL local messages, keys, and data. ' +
      'The app will restart. This cannot be undone.\n\nAre you sure?'
    );
    if (!confirmed) return;
    try {
      await invoke('panic_wipe');
    } catch (e) {
      dangerError = e instanceof Error ? e.message : String(e);
    }
  }

  async function setupAgain() {
    dangerError = '';
    const confirmed = window.confirm(
      'This deletes all local chats, contacts, and keys on this Mac, then shows onboarding again.\n\nContinue?'
    );
    if (!confirmed) return;
    try {
      await invoke('return_to_onboarding');
    } catch (e) {
      dangerError = e instanceof Error ? e.message : String(e);
    }
  }

  onMount(() => {
    load();
    getVersion().then((v) => (appVersion = v));
    let unlistenSettings = () => {};
    (async () => {
      unlistenSettings = await listen('settings-updated', (event) => {
        const p = event.payload ?? {};
        if (p.internet_enabled !== undefined) internetEnabled = p.internet_enabled;
        if (p.rendezvous_enabled !== undefined) rendezvousEnabled = p.rendezvous_enabled;
        if (p.connectivity_mode) connectivityMode = p.connectivity_mode;
        if (p.connectivity_detail) connectivityDetail = p.connectivity_detail;
      });
    })();
    return () => unlistenSettings();
  });
</script>

<div class="settings">
  <h3>Settings</h3>

  <section>
    <h4>Identity</h4>
    <label for="settings-display-name">Display name</label>
    <input
      id="settings-display-name"
      bind:value={displayName}
      placeholder="Your name in the network"
    />
    <button type="button" class="linkish" onclick={() => (showTechnicalIds = !showTechnicalIds)}>
      {showTechnicalIds ? "Hide technical IDs" : "Show technical IDs (advanced)"}
    </button>
    {#if showTechnicalIds}
      {#if accountId}
        <p class="field-label">Account ID (stable)</p>
        <code class="peer-id">{accountId}</code>
        <p class="hint">Transport epoch: {transportEpoch}</p>
      {/if}
      <p class="field-label">Network ID</p>
      <code class="peer-id">{peerId}</code>
    {/if}
  </section>

  <section>
    <h4>Network</h4>
    <p class="connectivity-note">
      <strong>{connectivityMode === "mesh_only" ? "Local mesh" : "Internet + mesh"}</strong>
      {#if connectivityDetail}
        — {connectivityDetail}
      {/if}
    </p>
    {#if networkError}
      <p class="network-error" role="alert">{networkError}</p>
    {/if}
    <label class="checkbox-row">
      <input
        type="checkbox"
        bind:checked={maxConnectivity}
        onchange={() => applyMaxConnectivity(maxConnectivity)}
      />
      Maximum connectivity
    </label>
    <small class="hint">
      Enables internet and relay discovery. On Android, also turns on Bluetooth mesh (F1–F3).
      On macOS, also turns on Bluetooth mesh when BLE toggle is enabled (Phase 1b).
    </small>
    <label class="checkbox-row">
      <input
        type="checkbox"
        bind:checked={internetEnabled}
        onchange={() => applyInternet(internetEnabled)}
      />
      Use internet (relay + remote peers)
    </label>
    <small class="hint">
      Applies immediately. Turn off for local mesh only — same-Wi‑Fi peers still work.
    </small>
    <label class="checkbox-row">
      <input
        type="checkbox"
        bind:checked={bleBootstrap}
        onchange={() => applyBleBootstrap(bleBootstrap)}
      />
      Bluetooth mesh (macOS/Linux)
    </label>
    <small class="hint">
      Saves preference and starts desktop BLE host (scan + advertise). Pair with Android for pocket mesh.
    </small>
    <label>
      Energy mode
      <select bind:value={energyState}>
        {#each energyOptions as opt}
          <option value={opt}>{opt}</option>
        {/each}
      </select>
    </label>
    <label>
      Custom bootstrap peers
      <textarea
        bind:value={bootstrapPeers}
        placeholder="Relay bootstrap address (one per line)"
        rows="3"
      ></textarea>
      <small>One per line. Saved with “Save settings”. Leave empty for default relay.</small>
    </label>
    <label class="checkbox-row">
      <input
        type="checkbox"
        bind:checked={crowdMode}
        onchange={() => applyCrowdMode(crowdMode)}
      />
      Crowd mode (festivals / dense events)
    </label>
    <small class="hint">
      Applies immediately for Kademlia policy; crowd gossip profile on next restart.
    </small>
    {#if crowdDensityActive}
      <p class="connectivity-note">Dense mesh — auto backoff active (many nearby peers)</p>
    {/if}
    <label>
      Local area (geohash prefix)
      <input
        bind:value={localGeohashPrefix}
        onchange={() => applyLocalGeohash()}
        placeholder="u4pr"
        maxlength="12"
      />
      <small>4+ chars for SOS listen and bulletin shards — not GPS. Saved on blur.</small>
    </label>
    <label class="checkbox-row">
      <input
        type="checkbox"
        bind:checked={rendezvousEnabled}
        disabled={!internetEnabled}
        onchange={() => applyRendezvous(rendezvousEnabled)}
      />
      Relay discovery (opt-in)
    </label>
    <small class="hint">
      Applies immediately. Lists you on the public rendezvous while online; auto-dials other opted-in peers.
    </small>
  </section>

  <section>
    <h4>Storage</h4>
    <p>Messages stored: <strong>{storeStats.count}</strong></p>
    {#if storeStats.oldest_ms > 0}
      <p>Oldest: <strong>{new Date(storeStats.oldest_ms).toLocaleDateString()}</strong></p>
    {/if}
    <button type="button" onclick={runGc} class="secondary">
      Clean up expired messages
    </button>
    {#if gcResult !== null}
      <p class="success">Deleted {gcResult} expired messages</p>
    {/if}
  </section>

  <button type="button" onclick={save} class="primary">
    {saved ? '✓ Saved' : 'Save identity & advanced'}
  </button>
  <small class="hint">Display name, energy mode, and bootstrap peers require save.</small>

  {#if appVersion}
    <p class="version">Version {appVersion}</p>
  {/if}

  <section>
    <h4>Legal &amp; privacy</h4>
    <button type="button" class="linkish" onclick={() => (showDataHandling = true)}>
      {dataHandlingCopy.title}
    </button>
    <button
      type="button"
      class="linkish"
      onclick={() => window.open('/privacy_policy.html', '_blank', 'noopener,noreferrer')}
    >
      {dataHandlingCopy.technical_policy_link_label}
    </button>
  </section>

  <section class="danger-zone">
    <h4>⚠ Danger zone</h4>
    <p>
      <strong>Start fresh</strong> — permanently deletes all local chats, contacts, keys, and wallet
      data on this Mac, then returns to onboarding with a new identity.
    </p>
    <button type="button" onclick={setupAgain} class="secondary danger-outline">
      Start fresh / onboarding
    </button>
    <p>
      <strong>Panic wipe</strong> — immediately deletes all local messages,
      keys, and stored data. Use if you need to quickly clear sensitive data.
    </p>
    <button type="button" onclick={panicWipe} class="danger">Panic wipe</button>
    {#if dangerError}
      <p class="danger-error" role="alert">{dangerError}</p>
    {/if}
  </section>
</div>

<DataHandling bind:open={showDataHandling} />

<style>
  .settings {
    padding: 16px 16px 48px;
    max-width: 600px;
    display: flex;
    flex-direction: column;
    gap: 16px;
    color: var(--text);
  }
  section {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 16px;
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  h3 { font-size: 18px; font-weight: 600; margin: 0; }
  h4 {
    font-size: 14px;
    font-weight: 600;
    margin: 0;
    color: var(--text-secondary);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  label, .field-label { display: flex; flex-direction: column; gap: 4px; font-size: 13px; }
  .field-label { margin: 0; }
  .hint, .connectivity-note { font-size: 12px; color: var(--text-muted); margin: 0; }
  .network-error { font-size: 12px; color: #dc2626; margin: 0; }
  input, select, textarea {
    padding: 7px 10px;
    border-radius: 6px;
    border: 1px solid var(--border);
    background: var(--surface);
    color: var(--text);
    font-size: 13px;
    font-family: inherit;
  }
  textarea { resize: vertical; font-family: ui-monospace, monospace; font-size: 12px; }
  code.peer-id {
    font-family: ui-monospace, monospace;
    font-size: 11px;
    background: var(--border);
    padding: 4px 6px;
    border-radius: 4px;
    word-break: break-all;
  }
  small { color: var(--text-muted); font-size: 11px; }
  button {
    padding: 8px 16px;
    border-radius: 6px;
    border: none;
    cursor: pointer;
    font-size: 13px;
    font-weight: 500;
  }
  button.primary { background: var(--accent); color: white; }
  button.secondary { background: var(--surface); color: var(--text); border: 1px solid var(--border); }
  button.danger-outline { border-color: var(--danger, #dc2626); color: var(--danger, #dc2626); margin-bottom: 8px; }
  button.danger { background: #dc2626; color: white; }
  button.linkish {
    margin: 10px 0 0;
    padding: 0;
    background: transparent;
    color: var(--accent);
    font-size: 13px;
  }
  .success { color: var(--accent); font-size: 12px; }
  .danger-zone { border-color: #dc2626; }
  .danger-zone h4 { color: #dc2626; }
  .danger-error { color: #dc2626; font-size: 12px; margin: 8px 0 0; }
  .version { font-size: 12px; color: var(--text-muted); margin: 0; }
  .checkbox-row {
    flex-direction: row;
    align-items: center;
    gap: 8px;
  }
</style>
