<script>
  // SPDX-License-Identifier: AGPL-3.0-or-later
  // Copyright (C) 2026 Mycena Project
  import { invoke } from "@tauri-apps/api/core";
  import { onMount, onDestroy } from "svelte";
  import QRCode from "qrcode";

  let status = $state(null);
  let androidQr = $state("");
  let iosQr = $state("");
  let error = $state("");
  let loading = $state(false);

  async function refresh() {
    status = await invoke("get_share_status");
    if (status?.androidUrl) {
      androidQr = await QRCode.toDataURL(status.androidUrl, { width: 240, margin: 2 });
    } else {
      androidQr = "";
    }
    if (status?.iosUrl) {
      iosQr = await QRCode.toDataURL(status.iosUrl, { width: 240, margin: 2 });
    } else {
      iosQr = "";
    }
  }

  async function startSharing() {
    error = "";
    loading = true;
    try {
      status = await invoke("start_share_server");
      await refresh();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  async function stopSharing() {
    status = await invoke("stop_share_server");
    await refresh();
  }

  async function chooseApk() {
    error = "";
    try {
      status = await invoke("choose_share_apk");
      await refresh();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  function copyText(text) {
    if (!text) return;
    navigator.clipboard.writeText(text).catch(() => {});
  }

  onMount(refresh);
  onDestroy(() => {
    if (status?.active) {
      invoke("stop_share_server").catch(() => {});
    }
  });
</script>

<section class="share-view">
  <h2>Share Mycena</h2>
    <p class="lead">
    When the internet is down, phones on the same Wi‑Fi can install Mycena from this computer —
    no Play Store or App Store required for Android. Sharing uses plain HTTP on the LAN with a
    one-time URL token; anyone on this network who has the link can download. Prefer verifying
    the APK checksum with the person sharing.
  </p>

  {#if error}
    <p class="error">{error}</p>
  {/if}

  {#if status && !status.androidApkAvailable}
    <div class="warn card">
      <p><strong>Android APK not loaded.</strong> Choose a built APK once (while online or from a USB stick), then share offline anytime.</p>
      <button type="button" onclick={chooseApk}>Choose Android APK…</button>
      <p class="hint mono">{status.apkPath}</p>
    </div>
  {:else if status?.androidApkAvailable}
    <p class="ok">✓ Android APK ready on this Mac</p>
  {/if}

  {#if !status?.active}
    <button type="button" class="primary" disabled={loading} onclick={startSharing}>
      {loading ? "Starting…" : "Start sharing on local network"}
    </button>
    <p class="hint">
      Phones must be on the same Wi‑Fi or LAN as this computer. Keep this tab open while friends download.
    </p>
    {#if status?.androidApkAvailable}
      <button type="button" class="linkish" onclick={chooseApk}>Replace APK file…</button>
    {/if}
  {:else}
    <p class="ok">Sharing at <span class="mono">{status.baseUrl}</span></p>

    <div class="qr-grid">
      <div class="card">
        <h3>Android</h3>
        <p class="hint">Scan in Chrome on the phone, then tap Install.</p>
        {#if androidQr}
          <img alt="Android install QR" src={androidQr} />
        {/if}
        <p class="mono">{status.androidUrl}</p>
        <button type="button" onclick={() => copyText(status.androidUrl)}>Copy link</button>
      </div>

      <div class="card ios">
        <h3>iPhone / iOS</h3>
        <p class="hint">
          Native iOS app is not installable offline yet (Apple restriction). This QR opens info and the beta page when online.
        </p>
        {#if iosQr}
          <img alt="iOS info QR" src={iosQr} />
        {/if}
        <p class="mono">{status.iosUrl}</p>
        <button type="button" onclick={() => copyText(status.iosUrl)}>Copy link</button>
      </div>
    </div>

    {#if status.transferCount > 0}
      <p class="ok">APK downloaded {status.transferCount} time(s)</p>
    {/if}

    <button type="button" onclick={stopSharing}>Stop sharing</button>
  {/if}
</section>

<style>
  .share-view {
    padding: 24px;
    max-width: 720px;
  }
  h2 {
    margin: 0 0 8px;
  }
  .lead {
    color: var(--text-secondary);
    line-height: 1.5;
  }
  .hint {
    font-size: 13px;
    color: var(--text-muted);
    line-height: 1.45;
  }
  .mono {
    font-family: ui-monospace, monospace;
    font-size: 12px;
    word-break: break-all;
  }
  .error {
    color: #c62828;
  }
  .ok {
    color: var(--accent);
    font-size: 14px;
  }
  .warn {
    border-color: #f9a825;
  }
  .card {
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 16px;
    margin: 16px 0;
    background: var(--surface-elevated);
  }
  .qr-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 16px;
  }
  @media (max-width: 640px) {
    .qr-grid {
      grid-template-columns: 1fr;
    }
  }
  .qr-grid h3 {
    margin: 0 0 8px;
  }
  .qr-grid img {
    width: 220px;
    height: 220px;
    display: block;
    margin: 8px auto;
  }
  .primary {
    margin-top: 16px;
    padding: 10px 16px;
    border: none;
    border-radius: 8px;
    background: var(--accent);
    color: #fff;
    cursor: pointer;
    font-size: 14px;
  }
  .primary:disabled {
    opacity: 0.6;
    cursor: default;
  }
  button[type="button"]:not(.primary) {
    margin-top: 8px;
    padding: 8px 12px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface-elevated);
    color: var(--text);
    cursor: pointer;
  }
  .linkish {
    background: none;
    border: none;
    color: var(--accent);
    text-decoration: underline;
    cursor: pointer;
    font-size: 13px;
    padding: 0;
    margin-top: 8px;
  }
</style>
