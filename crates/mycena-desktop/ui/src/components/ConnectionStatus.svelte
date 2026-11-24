<script>
  // SPDX-License-Identifier: AGPL-3.0-or-later
  // Copyright (C) 2026 Mycena Project
  import { onMount } from "svelte";

  let {
    peerCount = 0,
    connectivityMode = "internet",
    internetEnabled = true,
    rendezvousEnabled = false,
    connectivityDetail = "",
  } = $props();

  let showConnected = $state(false);
  let showSearching = $state(false);
  let prevPeers = $state(0);
  let searchingTimer;

  const meshOnly = $derived(connectivityMode === "mesh_only");
  const voluntaryMesh = $derived(meshOnly && !internetEnabled);
  const modeLabel = $derived(
    voluntaryMesh
      ? "Local mesh (internet off)"
      : meshOnly
        ? "Local mesh (relay offline)"
        : "Internet + mesh",
  );

  const SEARCHING_INITIAL_MS = 25_000;
  const SEARCHING_PULSE_MS = 20_000;
  const CONNECTED_MS = 4_000;

  function clearSearchingTimer() {
    if (searchingTimer) {
      clearTimeout(searchingTimer);
      searchingTimer = undefined;
    }
  }

  function pulseSearching(durationMs) {
    if (peerCount > 0) return;
    showSearching = true;
    clearSearchingTimer();
    searchingTimer = setTimeout(() => {
      if (peerCount === 0) {
        showSearching = false;
      }
    }, durationMs);
  }

  $effect(() => {
    if (peerCount > 0 && prevPeers === 0) {
      showSearching = false;
      clearSearchingTimer();
      showConnected = true;
      const t = setTimeout(() => {
        showConnected = false;
      }, CONNECTED_MS);
      prevPeers = peerCount;
      return () => clearTimeout(t);
    }
    if (peerCount === 0) {
      showConnected = false;
      if (prevPeers > 0) {
        pulseSearching(SEARCHING_PULSE_MS);
      }
      prevPeers = 0;
    } else {
      prevPeers = peerCount;
    }
  });

  onMount(() => {
    pulseSearching(SEARCHING_INITIAL_MS);
    return () => clearSearchingTimer();
  });
</script>

<div class="status-bar">
  <span class="mode-pill" class:mesh={meshOnly}>
    {modeLabel}
  </span>
  {#if connectivityDetail}
    <span class="mode-detail">{connectivityDetail}</span>
  {/if}
</div>

{#if showSearching && peerCount === 0}
  <div class="status-banner searching">
    <span class="spinner"></span>
    {meshOnly
      ? "Searching for peers on your local network…"
      : "Searching for peers (LAN + relay)…"}
  </div>
{:else if showConnected}
  <div class="status-banner connected">
    <span class="dot"></span>
    {peerCount} peer{peerCount !== 1 ? "s" : ""} connected
  </div>
{/if}

<style>
  .status-bar {
    padding: 6px 16px;
    border-bottom: 1px solid var(--border);
    background: var(--surface-elevated);
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .mode-pill {
    display: inline-block;
    font-size: 11px;
    padding: 3px 8px;
    border-radius: 999px;
    background: var(--accent-subtle);
    color: var(--text-secondary);
    width: fit-content;
  }
  .mode-pill.mesh {
    background: var(--surface);
    color: var(--text);
    border: 1px solid var(--border);
  }
  .mode-detail {
    font-size: 11px;
    color: var(--text-muted);
    line-height: 1.35;
  }
  .status-banner {
    padding: 8px 16px;
    font-size: 12px;
    display: flex;
    align-items: center;
    gap: 8px;
    border-bottom: 1px solid var(--border);
  }
  .searching {
    background: var(--sidebar-bg);
    color: var(--text-muted);
  }
  .connected {
    background: var(--accent-subtle);
    color: var(--text-secondary);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--accent);
    flex-shrink: 0;
  }
  .spinner {
    width: 14px;
    height: 14px;
    border: 2px solid var(--muted);
    border-top-color: var(--text-muted);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
