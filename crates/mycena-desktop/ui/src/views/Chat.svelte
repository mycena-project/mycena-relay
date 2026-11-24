<script>
  // SPDX-License-Identifier: AGPL-3.0-or-later
  // Copyright (C) 2026 Mycena Project
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount, tick } from "svelte";

  import {
    contactPeerId,
    sanitizeUserError,
    userVisibleContactName,
  } from "../lib/contactDisplay.js";

  let { peerId } = $props();
  let selectedPeer = $state("");
  let acceptedContacts = $state([]);
  let pendingContacts = $state([]);
  let messages = $state([]);
  let input = $state("");
  let isEncrypted = $state(false);
  let errorMsg = $state("");
  let locationLoading = $state(false);
  let peerReputation = $state(null);
  let pendingCustody = $state([]);
  let meshOnly = $state(false);
  let showLocationPrivacy = $state(false);
  let locationPrivacyAck = $state(
    typeof localStorage !== "undefined" &&
      localStorage.getItem("location_privacy_acknowledged") === "1",
  );

  function formatLocation(lat, lon, accuracyM) {
    const latAbs = Math.abs(lat).toFixed(4);
    const lonAbs = Math.abs(lon).toFixed(4);
    const latDir = lat >= 0 ? "N" : "S";
    const lonDir = lon >= 0 ? "E" : "W";
    const acc =
      accuracyM != null && !Number.isNaN(accuracyM)
        ? ` (±${Math.round(accuracyM)}m)`
        : "";
    return `📍 ${latAbs}° ${latDir}, ${lonAbs}° ${lonDir}${acc}`;
  }

  function extractCoordinates(body) {
    const m = body.match(/(-?\d+\.\d+)° ([NS]), (-?\d+\.\d+)° ([EW])/);
    if (!m) return null;
    const lat = parseFloat(m[1]) * (m[2] === "S" ? -1 : 1);
    const lon = parseFloat(m[3]) * (m[4] === "W" ? -1 : 1);
    return { lat, lon };
  }

  function contactDisplayName(c) {
    return userVisibleContactName(c, "Unknown contact");
  }

  async function refreshContacts() {
    errorMsg = "";
    try {
      acceptedContacts = await invoke("list_accepted_contacts");
      if (!Array.isArray(acceptedContacts)) {
        acceptedContacts = [];
      }
    } catch {
      errorMsg = sanitizeUserError(null);
      acceptedContacts = [];
    }
    try {
      pendingContacts = await invoke("list_pending_contacts");
    } catch {
      pendingContacts = [];
    }
    if (!selectedPeer && acceptedContacts.length > 0) {
      selectedPeer = contactPeerId(acceptedContacts[0]);
    }
  }

  function msToMinutes(ms) {
    return Math.max(1, Math.round(ms / 60000));
  }

  function custodyMessageText(status) {
    const carriers = status.expected?.active_carrier_count ?? 0;
    const noCarriers =
      carriers === 0 || status.expected?.basis === "NoCarriersNearby";
    if (noCarriers) {
      return "Stored locally · will deliver when someone is nearby";
    }
    const minMin = msToMinutes(status.expected.eta_min_ms);
    const maxMin = msToMinutes(status.expected.eta_max_ms);
    const carrierText =
      carriers === 1
        ? "1 possible carrier nearby"
        : `${carriers} possible carriers nearby`;
    return `Stored locally · expected delivery: ${minMin}–${maxMin} min · ${carrierText}`;
  }

  function custodyForMessage(msg) {
    if (!msg.is_mine || !meshOnly) return null;
    return (
      pendingCustody.find(
        (entry) =>
          entry.app_chat_id === msg.id ||
          (!entry.app_chat_id &&
            entry.chat_body === msg.body &&
            Math.abs(Number(entry.enqueued_at_ms) - Number(msg.timestamp_ms)) <=
              120_000),
      ) ?? null
    );
  }

  function formatTime(timestampMs) {
    if (!timestampMs) return "";
    return new Date(timestampMs).toLocaleString(undefined, {
      hour: "2-digit",
      minute: "2-digit",
      month: "short",
      day: "numeric",
    });
  }

  let messagesEl = $state(null);

  async function scrollMessagesToBottom() {
    await tick();
    if (messagesEl) {
      messagesEl.scrollTop = messagesEl.scrollHeight;
    }
  }

  async function refresh() {
    await refreshContacts();
    if (selectedPeer) {
      messages = await invoke("chat_history", { peerId: selectedPeer, limit: 100 });
      isEncrypted = await invoke("peer_has_enc_key", { peerId: selectedPeer });
      try {
        peerReputation = await invoke("get_peer_reputation", { peerId: selectedPeer });
      } catch {
        peerReputation = null;
      }
      try {
        const allPending = await invoke("list_pending_custody");
        pendingCustody = Array.isArray(allPending)
          ? allPending.filter((e) => e.dest_peer === selectedPeer)
          : [];
      } catch {
        pendingCustody = [];
      }
      try {
        const mode = await invoke("get_connectivity_mode");
        meshOnly = mode === "mesh_only";
      } catch {
        meshOnly = false;
      }
      await scrollMessagesToBottom();
    } else {
      isEncrypted = false;
      peerReputation = null;
      pendingCustody = [];
      meshOnly = false;
    }
  }

  async function acceptContact(peerId) {
    await invoke("accept_contact", { peerId });
    selectedPeer = peerId;
    await refresh();
  }

  async function rejectContact(peerId) {
    await invoke("reject_contact", { peerId });
    if (selectedPeer === peerId) selectedPeer = "";
    await refresh();
  }

  async function blockPeer() {
    if (!selectedPeer) return;
    await invoke("block_contact", { peerId: selectedPeer });
    selectedPeer = "";
    await refresh();
  }

  async function sendBody(body) {
    if (!selectedPeer || !body.trim()) return;
    errorMsg = "";
    try {
      await invoke("send_chat_encrypted", { toPeer: selectedPeer, body });
      await refresh();
    } catch (e) {
      const msg = String(e);
      if (msg.includes("enc_key_not_yet_exchanged")) {
        errorMsg =
          "Key exchange pending — message not sent. Try again shortly.";
      } else if (
        !meshOnly &&
        (msg.includes("peer_not_connected") ||
          msg.includes("not connected to peer"))
      ) {
        errorMsg =
          "Not connected yet — wait a moment after scanning QR, then try again.";
      } else if (!meshOnly) {
        errorMsg = sanitizeUserError(msg);
      } else {
        await refresh();
      }
    }
  }

  async function send() {
    if (!input.trim() || !selectedPeer) return;
    const body = input;
    input = "";
    await sendBody(body);
  }

  function shareLocationOnce() {
    if (!selectedPeer || !navigator.geolocation) {
      errorMsg = "Location is not available on this device.";
      return;
    }
    locationLoading = true;
    navigator.geolocation.getCurrentPosition(
      async (pos) => {
        locationLoading = false;
        const body = formatLocation(
          pos.coords.latitude,
          pos.coords.longitude,
          pos.coords.accuracy,
        );
        await sendBody(body);
      },
      () => {
        locationLoading = false;
        errorMsg = "Could not get location. Check system location permissions.";
      },
      { enableHighAccuracy: true, timeout: 15_000, maximumAge: 60_000 },
    );
  }

  function onLocationClick() {
    if (!locationPrivacyAck) {
      showLocationPrivacy = true;
      return;
    }
    shareLocationOnce();
  }

  function confirmLocationPrivacy() {
    localStorage.setItem("location_privacy_acknowledged", "1");
    locationPrivacyAck = true;
    showLocationPrivacy = false;
    shareLocationOnce();
  }

  async function openMaps(body) {
    const coords = extractCoordinates(body);
    if (!coords) return;
    const url = `https://www.google.com/maps?q=${coords.lat},${coords.lon}`;
    window.open(url, "_blank");
  }

  onMount(() => {
    refresh();
    const unsubs = [];
    (async () => {
      unsubs.push(await listen("chat-updated", refresh));
      unsubs.push(await listen("contacts-updated", refresh));
    })();
    return () => unsubs.forEach((u) => u?.());
  });
</script>

{#if showLocationPrivacy}
  <div class="modal-backdrop" role="presentation">
    <div class="modal" role="dialog">
      <h3>Share your location?</h3>
      <p>
        Your current coordinates will be sent once in this chat. Mycena does
        not track your location continuously. The message is end-to-end
        encrypted when a key is exchanged.
      </p>
      <div class="modal-actions">
        <button type="button" class="secondary" onclick={() => (showLocationPrivacy = false)}>
          Cancel
        </button>
        <button type="button" onclick={confirmLocationPrivacy}>Share once</button>
      </div>
    </div>
  </div>
{/if}

<div class="chat-layout">
  <div class="sidebar">
    {#if pendingContacts.length > 0}
      <p class="section-label">Contact requests</p>
      {#each pendingContacts as c}
        <div class="pending-row">
          <span class="pending-name">{contactDisplayName(c)}</span>
          <div class="pending-actions">
            <button type="button" class="small" onclick={() => acceptContact(contactPeerId(c))}>
              Accept
            </button>
            <button type="button" class="small secondary" onclick={() => rejectContact(contactPeerId(c))}>
              Decline
            </button>
          </div>
        </div>
      {/each}
    {/if}
    <p class="section-label">Contacts</p>
    {#if acceptedContacts.length === 0}
      <p class="empty-hint">No contacts yet. Connect a device on the Connect tab by scanning a QR code.</p>
    {/if}
    {#each acceptedContacts as c (contactPeerId(c))}
      <button
        onclick={() => ((selectedPeer = contactPeerId(c)), refresh())}
        class:active={selectedPeer === contactPeerId(c)}
      >
        {contactDisplayName(c)}
      </button>
    {/each}
  </div>
  <div class="conversation">
    <div class="enc-badge" class:encrypted={isEncrypted}>
      {isEncrypted
        ? "🔒 End-to-end encrypted"
        : "⚠ Not encrypted – peer key not yet exchanged"}
    </div>
    {#if selectedPeer && peerReputation}
      <div class="reputation-bar">
        <span>Mesh reputation: {peerReputation.strikes} strike{peerReputation.strikes === 1 ? "" : "s"}</span>
        <button type="button" class="small secondary" onclick={blockPeer}>Block contact</button>
      </div>
    {/if}
    {#if errorMsg}
      <div class="error-banner">{errorMsg}</div>
    {/if}
    <div class="messages" bind:this={messagesEl}>
      {#each messages as msg}
        {@const custody = custodyForMessage(msg)}
        <div class="msg-row" class:own={msg.is_mine}>
          <div class="msg">
            {#if !msg.is_mine}
              <span class="name">{msg.from_display_name}</span>
            {/if}
            <span class="body">{msg.body}</span>
            <span class="time">{formatTime(msg.timestamp_ms)}</span>
            {#if custody}
              <span class="custody-status">{custodyMessageText(custody)}</span>
            {/if}
            {#if msg.body.startsWith("📍")}
              {@const coords = extractCoordinates(msg.body)}
              {#if coords}
                <button type="button" class="maps-link" onclick={() => openMaps(msg.body)}>
                  Open in Maps
                </button>
              {/if}
            {/if}
          </div>
        </div>
      {/each}
    </div>
    <div class="input-row">
      <button
        type="button"
        class="loc-btn"
        title="Share my location"
        disabled={!selectedPeer || locationLoading}
        onclick={onLocationClick}
      >
        {locationLoading ? "…" : "📍"}
      </button>
      <input
        bind:value={input}
        onkeydown={(e) => e.key === "Enter" && send()}
        placeholder="Message..."
        disabled={!selectedPeer}
      />
      <button onclick={send} disabled={!selectedPeer}>Send</button>
    </div>
  </div>
</div>

<style>
  .chat-layout {
    display: flex;
    height: 100%;
  }
  .sidebar {
    width: 240px;
    border-right: 1px solid var(--border);
    overflow-y: auto;
    padding: 8px;
    background: var(--sidebar-bg);
    color: var(--text);
  }
  .section-label {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-muted);
    margin: 8px 4px 4px;
  }
  .empty-hint {
    font-size: 12px;
    color: var(--text-muted);
    padding: 8px 4px;
    margin: 0;
  }
  .pending-row {
    padding: 6px 4px 10px;
    border-bottom: 1px solid var(--border);
    margin-bottom: 4px;
  }
  .pending-name {
    display: block;
    font-size: 13px;
    margin-bottom: 4px;
    color: var(--text);
  }
  .pending-actions {
    display: flex;
    gap: 6px;
  }
  .sidebar button {
    display: block;
    width: 100%;
    text-align: left;
    padding: 8px;
    border-radius: 6px;
    border: none;
    background: none;
    cursor: pointer;
    font-size: 13px;
    color: var(--text);
  }
  .sidebar button:hover {
    background: var(--surface);
  }
  .sidebar button.active {
    background: var(--accent-subtle);
    color: var(--accent);
    font-weight: 500;
  }
  button.small {
    padding: 4px 8px;
    font-size: 12px;
  }
  button.secondary {
    background: var(--surface);
    color: var(--text);
    border: 1px solid var(--border);
  }
  .conversation {
    flex: 1;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    min-width: 0;
  }
  .enc-badge {
    font-size: 12px;
    padding: 6px 10px;
    border-bottom: 1px solid var(--border);
    background: var(--surface-variant, rgba(128, 128, 128, 0.12));
    color: var(--text-muted);
  }
  .enc-badge.encrypted {
    background: var(--accent-subtle);
    color: var(--text);
  }
  .reputation-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    font-size: 12px;
    padding: 6px 10px;
    border-bottom: 1px solid var(--border);
    color: var(--text-muted);
  }
  .error-banner {
    font-size: 12px;
    padding: 8px 10px;
    background: rgba(200, 60, 60, 0.15);
    color: #c0392b;
    border-bottom: 1px solid var(--border);
  }
  .custody-status {
    display: block;
    margin-top: 6px;
    font-size: 11px;
    line-height: 1.35;
    padding: 4px 8px;
    border-radius: 6px;
    background: rgba(52, 120, 190, 0.14);
    color: #2c5282;
  }
  .msg-row.own .custody-status {
    background: rgba(52, 120, 190, 0.18);
    color: #1e4f7a;
  }
  .messages {
    flex: 1;
    overflow-y: auto;
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .msg-row {
    display: flex;
    justify-content: flex-start;
  }
  .msg-row.own {
    justify-content: flex-end;
  }
  .msg {
    max-width: min(75%, 420px);
    padding: 10px 14px;
    border-radius: 16px;
    background: var(--msg-in);
    color: var(--text);
  }
  .msg-row.own .msg {
    background: var(--msg-out);
    border-bottom-right-radius: 4px;
  }
  .msg-row:not(.own) .msg {
    border-bottom-left-radius: 4px;
  }
  .name {
    font-size: 11px;
    color: var(--text-muted);
    display: block;
    margin-bottom: 4px;
  }
  .body {
    font-size: 14px;
    line-height: 1.45;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .time {
    display: block;
    margin-top: 4px;
    font-size: 10px;
    color: var(--text-muted);
    text-align: right;
  }
  .maps-link {
    margin-top: 4px;
    padding: 0;
    border: none;
    background: none;
    color: var(--accent);
    font-size: 12px;
    cursor: pointer;
    text-decoration: underline;
  }
  .input-row {
    display: flex;
    gap: 8px;
    padding: 12px;
    border-top: 1px solid var(--border);
    align-items: center;
  }
  .loc-btn {
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface);
    color: var(--text);
    cursor: pointer;
    font-size: 16px;
  }
  .loc-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  input {
    flex: 1;
    padding: 8px 12px;
    border-radius: 6px;
    border: 1px solid var(--border);
    background: var(--surface);
    color: var(--text);
    font-size: 14px;
  }
  button {
    padding: 8px 16px;
    background: var(--accent);
    color: #fff;
    border: none;
    border-radius: 6px;
    cursor: pointer;
    font-size: 14px;
  }
  button:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }
  .modal {
    background: var(--surface-elevated);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 20px;
    max-width: 400px;
    margin: 16px;
    color: var(--text);
  }
  .modal h3 {
    margin: 0 0 8px;
    font-size: 16px;
  }
  .modal p {
    font-size: 13px;
    color: var(--text-muted);
    margin: 0 0 16px;
    text-align: left;
  }
  .modal-actions {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
  }
  .modal-actions .secondary {
    background: var(--surface);
    color: var(--text);
    border: 1px solid var(--border);
  }
</style>
