<script>
  // SPDX-License-Identifier: AGPL-3.0-or-later
  // Copyright (C) 2026 Mycena Project
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';

  let summary = $state(null);
  let loading = $state(true);

  function formatUptime(secs) {
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    return h > 0 ? `${h}h ${m}m` : `${m}m`;
  }

  async function refresh() {
    loading = true;
    try {
      summary = await invoke('user_trust_summary');
    } catch {
      summary = null;
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    refresh();
    const interval = setInterval(refresh, 15_000);
    return () => clearInterval(interval);
  });
</script>

<div class="more">
  <header>
    <h3>More</h3>
    <p class="subtitle">Apps, network contribution, and shortcuts.</p>
  </header>

  <section class="contribution">
    <h4>Network contribution</h4>
    {#if loading && !summary}
      <p class="hint">Measuring…</p>
    {:else if summary}
      <p class="tier">{summary.contribution_tier}</p>
      <div class="bar">
        <div class="fill" style="width: {Math.min(100, summary.contribution_score)}%"></div>
      </div>
      <p class="stats">
        Relays: {summary.relay_contribution} · Deliveries: {summary.successful_deliveries} ·
        Uptime: {formatUptime(Number(summary.uptime_secs))} · Endorsements:
        {summary.local_endorsements}
      </p>
    {:else}
      <p class="hint">
        Relay messages, stay online, and endorse helpful peers to grow your contribution score.
      </p>
    {/if}
  </section>

  <section class="links">
    <p class="hint">Use the sidebar for Wallet, Mini apps, Share, Mail, and Settings.</p>
  </section>
</div>

<style>
  .more {
    padding: 16px;
    max-width: 640px;
    display: flex;
    flex-direction: column;
    gap: 16px;
    color: var(--text);
  }
  header h3 {
    margin: 0 0 4px;
    font-size: 18px;
  }
  .subtitle {
    margin: 0;
    font-size: 13px;
    color: var(--text-secondary);
  }
  section {
    padding: 16px;
    border: 1px solid var(--border);
    border-radius: 8px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  h4 {
    margin: 0;
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-secondary);
  }
  .tier {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
  }
  .bar {
    height: 8px;
    background: var(--surface);
    border-radius: 4px;
    overflow: hidden;
    border: 1px solid var(--border);
  }
  .fill {
    height: 100%;
    background: var(--accent, #2563eb);
  }
  .stats,
  .hint {
    margin: 0;
    font-size: 12px;
    color: var(--text-muted);
  }
</style>
