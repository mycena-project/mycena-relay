<script>
  // SPDX-License-Identifier: AGPL-3.0-or-later
  // Copyright (C) 2026 Mycena Project
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import QRCode from "qrcode";
  import {
    contactPeerId,
    sanitizeUserError,
    userVisibleContactName,
  } from "../lib/contactDisplay.js";
  import { PeersDiscoveryPhase, reduce } from "../lib/peersDiscovery.js";

  let peers = $state([]);
  let acceptedContacts = $state([]);
  let pasteInvite = $state("");
  let qr = $state("");
  let inviteUrl = $state("");
  let shareAddrs = $state([]);
  let relayStatus = $state(null);
  let rendezvousEnabled = $state(false);
  let connectMessage = $state("");
  let showAdvanced = $state(false);
  let discoveryPresentation = $state(null);

  const connectedLabels = $derived(
    peers.map((peerId) => {
      const contact = acceptedContacts.find((c) => contactPeerId(c) === peerId);
      return userVisibleContactName(contact, "Connected device");
    }),
  );

  async function refreshPeers() {
    peers = await invoke("get_peers");
    try {
      acceptedContacts = await invoke("list_accepted_contacts");
    } catch {
      acceptedContacts = [];
    }
  }

  async function refreshInvite() {
    shareAddrs = await invoke("get_shareable_multiaddrs");
    try {
      const settings = await invoke("get_settings");
      rendezvousEnabled = settings?.rendezvous_enabled ?? true;
    } catch {
      rendezvousEnabled = true;
    }
  }

  async function refreshDiscovery() {
    try {
      const snapshot = await invoke("get_mesh_discovery_snapshot");
      discoveryPresentation = reduce({
        ...snapshot,
        connectedPeerCount: peers.length,
      });
    } catch {
      discoveryPresentation = null;
    }
  }

  async function refreshAll() {
    await Promise.all([refreshPeers(), refreshInvite()]);
    await refreshDiscovery();
  }

  async function applyInvite() {
    const raw = pasteInvite.trim();
    if (!raw) return;
    connectMessage = "";
    try {
      await invoke("connect_peer_id", { peerId: raw });
      pasteInvite = "";
      connectMessage = "Connecting… check Connected devices below.";
      await refreshAll();
    } catch (e) {
      connectMessage = sanitizeUserError(e instanceof Error ? e.message : String(e));
    }
  }

  async function copyInvite() {
    if (!inviteUrl) return;
    await navigator.clipboard.writeText(inviteUrl);
    connectMessage = "Invite link copied.";
  }

  async function fetchRelayStatus() {
    try {
      relayStatus = await invoke("get_relay_status");
    } catch {
      relayStatus = { online: false, status: "offline", connections: 0 };
    }
  }

  onMount(() => {
    refreshAll();
    const retryInvite = setTimeout(refreshInvite, 3_000);
    const discoveryPoll = setInterval(refreshDiscovery, 2_000);
    let unlistenMetrics = () => {};
    let unlistenContacts = () => {};
    (async () => {
      unlistenMetrics = await listen("metrics-updated", refreshPeers);
      unlistenContacts = await listen("contacts-updated", refreshAll);
    })();
    return () => {
      clearTimeout(retryInvite);
      clearInterval(discoveryPoll);
      unlistenMetrics();
      unlistenContacts();
    };
  });

  $effect(() => {
    const nextUrl =
      shareAddrs.find((a) => a.startsWith("mycena://invite/")) ??
      shareAddrs[0] ??
      "";
    if (nextUrl === inviteUrl) return;
    inviteUrl = nextUrl;
    if (!inviteUrl) {
      qr = "";
      return;
    }
    QRCode.toDataURL(inviteUrl, {
      width: 320,
      margin: 2,
      errorCorrectionLevel: "M",
    }).then((x) => (qr = x));
  });

  $effect(() => {
    fetchRelayStatus();
    const t = setInterval(fetchRelayStatus, 30_000);
    return () => clearInterval(t);
  });
</script>

<div class="connect">
  {#if relayStatus}
    <div class="relay-status" class:online={relayStatus.online}>
      <span class="dot"></span>
      {relayStatus.online
        ? `Public relay online · ${relayStatus.reachable_peers ?? relayStatus.unique_peers ?? 0} reachable`
        : "Public relay unreachable — same Wi‑Fi still works"}
    </div>
  {/if}

  <section class="hero">
    <h2>Connect another device</h2>
    <p class="lede">
      Scan each other&apos;s QR code once. On the same Wi‑Fi, devices often connect automatically
      afterward.
    </p>
    {#if !rendezvousEnabled && peers.length === 0}
      <p class="tip">
        Tip: turn on <strong>Relay discovery</strong> in Settings if you are not on the same Wi‑Fi.
      </p>
    {/if}
  </section>

  <section class="qr-panel">
    <h3>Show this QR on your Mac</h3>
    <p class="muted scan-hint">On the other device: Connect → Scan their QR code.</p>
    {#if qr}
      <img alt="Invite QR for the other device to scan" src={qr} />
    {:else}
      <p class="muted">Starting node…</p>
    {/if}
  </section>

  {#if discoveryPresentation && discoveryPresentation.phase !== PeersDiscoveryPhase.Connected && discoveryPresentation.title}
    <section
      class="discovery-banner"
      class:discovery-active={discoveryPresentation.showProgress}
      class:discovery-quiet={discoveryPresentation.phase === PeersDiscoveryPhase.NobodyInRange ||
        discoveryPresentation.phase === PeersDiscoveryPhase.Idle}
    >
      <div class="discovery-row">
        {#if discoveryPresentation.showProgress}
          <div class="discovery-spinner" aria-hidden="true"></div>
        {/if}
        <div>
          <h3 class="discovery-title">{discoveryPresentation.title}</h3>
          <p class="discovery-body">{discoveryPresentation.body}</p>
        </div>
      </div>
    </section>
  {/if}

  <section>
    <button type="button" class="linkish" onclick={() => (showAdvanced = !showAdvanced)}>
      {showAdvanced ? "Hide advanced options" : "Advanced options"}
    </button>
    {#if showAdvanced}
      <p class="muted advanced-hint">
        For developers: paste an invite link or raw connection code manually.
      </p>
      <textarea
        bind:value={pasteInvite}
        placeholder="Invite link or code"
        rows="3"
      ></textarea>
      <div class="row">
        <button type="button" onclick={applyInvite} disabled={!pasteInvite.trim()}>Connect</button>
        <button type="button" class="secondary" onclick={copyInvite} disabled={!inviteUrl}>
          Copy invite link
        </button>
      </div>
    {/if}
    {#if connectMessage}
      <p class="message">{connectMessage}</p>
    {/if}
  </section>

  <section>
    <h3>Connected devices ({peers.length})</h3>
    {#if peers.length === 0}
      <p class="muted">
        None yet. After scanning, wait a few seconds. Same Wi‑Fi should appear without relay.
      </p>
    {:else}
      {#each connectedLabels as label}
        <p class="device-name">{label}</p>
      {/each}
    {/if}
  </section>

  <section class="muted-block">
    <h3>Contact durability</h3>
    <p>
      Scan an invite <strong>once</strong> — the contact stays in your book across restarts.
      Network paths come and go; you do not need to re-scan when someone drops offline.
    </p>
  </section>
</div>

<style>
  .connect {
    display: flex;
    flex-direction: column;
    gap: 20px;
    padding: 16px;
    max-width: 520px;
    margin: 0 auto;
    color: var(--text);
  }
  .relay-status {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 12px;
    border-radius: 8px;
    font-size: 13px;
    background: #fde8e8;
    color: #444;
  }
  .relay-status.online {
    background: #e1f5ee;
  }
  .relay-status .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #c44;
  }
  .relay-status.online .dot {
    background: #1d9e75;
  }
  .hero h2 {
    margin: 0 0 8px;
    font-size: 20px;
    color: var(--text);
  }
  .lede {
    margin: 0;
    font-size: 14px;
    line-height: 1.5;
    color: var(--text-muted);
  }
  .tip {
    margin: 12px 0 0;
    padding: 10px 12px;
    border-radius: 8px;
    background: var(--surface, #f4f4f5);
    font-size: 13px;
  }
  section {
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 14px;
  }
  h3 {
    margin: 0 0 10px;
    font-size: 14px;
    color: var(--text);
  }
  .qr-panel {
    text-align: center;
  }
  .qr-panel img {
    width: 280px;
    max-width: 100%;
    height: auto;
    margin: 8px auto;
    display: block;
  }
  .scan-hint {
    margin: 0 0 8px;
  }
  .discovery-banner {
    background: color-mix(in srgb, var(--accent) 12%, var(--surface, #f4f4f5));
  }
  .discovery-banner.discovery-quiet {
    background: var(--surface, #f4f4f5);
  }
  .discovery-row {
    display: flex;
    align-items: flex-start;
    gap: 12px;
  }
  .discovery-spinner {
    width: 28px;
    height: 28px;
    border: 3px solid color-mix(in srgb, var(--accent) 25%, transparent);
    border-top-color: var(--accent);
    border-radius: 50%;
    flex-shrink: 0;
    margin-top: 2px;
    animation: discovery-spin 0.9s linear infinite;
  }
  @keyframes discovery-spin {
    to {
      transform: rotate(360deg);
    }
  }
  .discovery-title {
    margin: 0;
    font-size: 14px;
  }
  .discovery-body {
    margin: 4px 0 0;
    font-size: 13px;
    color: var(--text-muted);
    line-height: 1.45;
  }
  .device-name {
    margin: 6px 0;
    font-size: 14px;
  }
  textarea {
    width: 100%;
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 10px;
    font-size: 14px;
    background: var(--surface);
    color: var(--text);
    resize: vertical;
    box-sizing: border-box;
  }
  button {
    margin-top: 10px;
    padding: 10px 16px;
    border: none;
    border-radius: 8px;
    background: var(--accent);
    color: #fff;
    cursor: pointer;
    font-size: 14px;
  }
  button.secondary {
    background: transparent;
    color: var(--accent);
    border: 1px solid var(--accent);
  }
  button.linkish {
    margin-top: 0;
    background: transparent;
    color: var(--accent);
    padding: 0;
    font-size: 13px;
  }
  button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .advanced-hint {
    margin: 8px 0 0;
  }
  .muted,
  .muted-block p {
    font-size: 13px;
    color: var(--text-muted);
    line-height: 1.45;
  }
  .message {
    margin-top: 8px;
    font-size: 13px;
    color: #1d6b4a;
  }
</style>
