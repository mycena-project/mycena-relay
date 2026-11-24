<script>
  // SPDX-License-Identifier: AGPL-3.0-or-later
  // Copyright (C) 2026 Mycena Project
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";

  let { onOpenApp = () => {} } = $props();

  let installed = $state([]);
  let listings = $state([]);
  let message = $state("");
  let pendingInstall = $state(null);
  let allowDowngrade = $state(false);
  let installingId = $state("");
  let showAdvanced = $state(false);
  let revokeAppId = $state("");
  let revokeReason = $state("malware");

  const trustLabels = {
    verified_listing: "Verified store listing",
    matching_listing_hash: "Listing hash match (signature invalid)",
    hash_mismatch: "Bundle hash mismatch",
    sideload_only: "Sideload only",
  };

  function appId(row) {
    return row?.manifest?.id ?? "";
  }

  function isInstalled(id) {
    return installed.some((app) => app.id === id);
  }

  async function refresh() {
    try {
      installed = await invoke("miniapp_list_installed");
      listings = await invoke("miniapp_browse_store");
      message = "";
    } catch (e) {
      message = String(e);
    }
  }

  async function installFromFile(ev) {
    const file = ev.target?.files?.[0];
    if (!file) return;
    const buf = await file.arrayBuffer();
    const bytes = new Uint8Array(buf);
    let binary = "";
    for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
    const bundleBase64 = btoa(binary);
    try {
      const preview = await invoke("miniapp_preview_install", { bundleBase64 });
      allowDowngrade = false;
      pendingInstall = { bundleBase64, preview };
    } catch (e) {
      message = String(e);
    }
    ev.target.value = "";
  }

  function listingAppIdForPending() {
    if (!pendingInstall) return null;
    if (pendingInstall.preview.trust_level === "verified_listing") {
      return pendingInstall.preview.manifest.id;
    }
    return null;
  }

  async function installOfficialListing(appId) {
    if (!appId || installingId) return;
    installingId = appId;
    try {
      await invoke("miniapp_install_official", { appId });
      message = `Installed ${appId}`;
      await refresh();
    } catch (e) {
      message = String(e);
    } finally {
      installingId = "";
    }
  }

  async function confirmPendingInstall() {
    if (!pendingInstall) return;
    try {
      await invoke("miniapp_install", {
        bundleBase64: pendingInstall.bundleBase64,
        listingAppId: listingAppIdForPending(),
        allowSideload: listingAppIdForPending() == null,
        allowDowngrade,
      });
      message = `Installed ${pendingInstall.preview.manifest.name}`;
      pendingInstall = null;
      await refresh();
    } catch (e) {
      message = String(e);
    }
  }

  async function publishRevocation() {
    const id = revokeAppId.trim();
    if (!id) {
      message = "Enter an app id to revoke";
      return;
    }
    try {
      await invoke("miniapp_publish_revocation", {
        appId: id,
        reason: revokeReason.trim() || "curator",
      });
      message = `Published revocation for ${id} on gossip`;
      revokeAppId = "";
      await refresh();
    } catch (e) {
      message = String(e);
    }
  }

  async function uninstall(appId) {
    try {
      await invoke("miniapp_uninstall", { appId });
      await refresh();
    } catch (e) {
      message = String(e);
    }
  }

  $effect(() => {
    refresh();
    const unsubs = [];
    (async () => {
      unsubs.push(await listen("appstore-updated", refresh));
    })();
    return () => unsubs.forEach((u) => u?.());
  });
</script>

<div class="store">
  <header class="hero">
    <h3>Mini apps</h3>
    <p class="lead">Official apps bundled with Mycena. Tap Install, then Open.</p>
  </header>

  {#if message}
    <p class="msg" role="status">{message}</p>
  {/if}

  <section class="catalog">
    <h4>Available apps</h4>
    {#if listings.length === 0}
      <p class="muted">No apps in the local catalog yet. Restart the node or sideload a bundle below.</p>
    {:else}
      <div class="cards">
        {#each listings as row (appId(row) + row.bundle_hash)}
          {@const id = appId(row)}
          {@const installedApp = isInstalled(id)}
          <article class="card">
            <div class="card-head">
              <div>
                <span class="title">{row.manifest.name}</span>
                <span class="meta">v{row.manifest.version}</span>
              </div>
              {#if row.signature_valid}
                <span class="badge ok">Signed</span>
              {/if}
            </div>
            {#if row.manifest.description}
              <p class="desc">{row.manifest.description}</p>
            {/if}
            <span class="meta mono">{id}</span>
            <div class="actions">
              {#if installedApp}
                <button type="button" class="primary" onclick={() => onOpenApp(id)}>Open</button>
                <button type="button" class="danger" onclick={() => uninstall(id)}>Remove</button>
              {:else}
                <button
                  type="button"
                  class="primary install"
                  disabled={installingId === id}
                  onclick={() => installOfficialListing(id)}
                >
                  {installingId === id ? "Installing…" : "Install"}
                </button>
              {/if}
            </div>
          </article>
        {/each}
      </div>
    {/if}
  </section>

  <section>
    <h4>Installed</h4>
    {#if installed.length === 0}
      <p class="muted">Nothing installed yet — pick an app above.</p>
    {:else}
      <ul>
        {#each installed as app (app.id)}
          <li>
            <span class="title">{app.name}</span>
            <span class="meta">{app.id} · v{app.version}</span>
            <div class="actions">
              <button type="button" class="primary" onclick={() => onOpenApp(app.id)}>Open</button>
              <button type="button" class="danger" onclick={() => uninstall(app.id)}>Remove</button>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section class="advanced">
    <button type="button" class="toggle-advanced" onclick={() => (showAdvanced = !showAdvanced)}>
      {showAdvanced ? "Hide" : "Show"} advanced options
    </button>
    {#if showAdvanced}
      <label class="file">
        Install bundle from file (.mxa / .zip)
        <input type="file" accept=".zip,.mxa,application/zip" onchange={installFromFile} />
      </label>
      <div class="curator">
        <h4>Curator — publish revocation</h4>
        <p class="muted">
          Signs with this node&apos;s identity and gossips on <code>mycena/appstore/revocations/v1</code>.
        </p>
        <div class="row">
          <input type="text" placeholder="com.example.badapp" bind:value={revokeAppId} aria-label="App id" />
          <input type="text" placeholder="reason" bind:value={revokeReason} aria-label="Reason" />
          <button type="button" class="danger" onclick={publishRevocation}>Revoke</button>
        </div>
      </div>
    {/if}
  </section>

  {#if pendingInstall}
    <div class="modal" role="dialog" aria-labelledby="install-review-title">
      <div class="modal-card">
        <h4 id="install-review-title">Review before install</h4>
        <p class="title">{pendingInstall.preview.manifest.name}</p>
        <p class="meta">{pendingInstall.preview.manifest.id} · v{pendingInstall.preview.manifest.version}</p>
        <p class="meta">
          Trust: {trustLabels[pendingInstall.preview.trust_level] ?? pendingInstall.preview.trust_level}
        </p>
        {#if pendingInstall.preview.listing_signature_ok}
          <p class="ok">Signed listing</p>
        {/if}
        {#if pendingInstall.preview.bundle_signature_ok}
          <p class="ok">Developer bundle signature verified</p>
        {/if}
        {#if pendingInstall.preview.has_inline_script}
          <p class="warn">This app uses inline scripts (higher XSS risk).</p>
        {/if}
        {#if pendingInstall.preview.is_downgrade && pendingInstall.preview.installed_version}
          <label class="row downgrade">
            <input type="checkbox" bind:checked={allowDowngrade} />
            Allow version downgrade
          </label>
        {/if}
        <p class="warn">Only install apps you trust.</p>
        <div class="row">
          <button
            type="button"
            class="primary"
            disabled={
              pendingInstall.preview.trust_level === "hash_mismatch" ||
              (pendingInstall.preview.is_downgrade && !allowDowngrade)
            }
            onclick={confirmPendingInstall}
          >
            Install
          </button>
          <button type="button" class="muted-btn" onclick={() => (pendingInstall = null)}>Cancel</button>
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  .store {
    padding: 16px 20px 32px;
    display: flex;
    flex-direction: column;
    gap: 16px;
    height: 100%;
    overflow: auto;
  }
  .hero h3 {
    margin: 0 0 4px;
  }
  .lead {
    margin: 0;
    color: var(--text-muted);
    font-size: 14px;
  }
  h4 {
    margin: 0 0 10px;
  }
  .catalog {
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 16px;
    background: var(--surface-elevated, var(--bg));
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: 12px;
  }
  .card {
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    background: var(--bg);
  }
  .card-head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 8px;
  }
  .badge {
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 999px;
    white-space: nowrap;
  }
  .badge.ok {
    background: var(--accent-subtle);
    color: var(--accent);
  }
  .desc {
    margin: 0;
    font-size: 13px;
    color: var(--text-secondary, var(--text-muted));
    line-height: 1.4;
  }
  .file {
    display: inline-block;
    padding: 8px 12px;
    border-radius: 8px;
    border: 1px solid var(--border);
    cursor: pointer;
    font-size: 14px;
    margin-top: 8px;
  }
  .file input {
    display: none;
  }
  .msg {
    color: #b45309;
    font-size: 14px;
    padding: 10px 12px;
    border-radius: 8px;
    background: #fef3c7;
    margin: 0;
  }
  .modal {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 20;
  }
  .modal-card {
    background: var(--bg, #fff);
    color: var(--text, #111);
    border-radius: 12px;
    padding: 20px;
    max-width: 420px;
    width: 90%;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.2);
  }
  .warn {
    color: #b45309;
    font-size: 13px;
  }
  .ok {
    color: #15803d;
    font-size: 12px;
  }
  section {
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 12px 16px;
  }
  ul {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  li {
    border-bottom: 1px solid var(--border);
    padding-bottom: 10px;
  }
  li:last-child {
    border-bottom: none;
    padding-bottom: 0;
  }
  .title {
    font-weight: 600;
    display: block;
  }
  .meta {
    font-size: 12px;
    color: var(--text-muted);
    display: block;
  }
  .mono {
    font-family: ui-monospace, monospace;
    word-break: break-all;
  }
  .actions {
    display: flex;
    gap: 8px;
    margin-top: 4px;
    flex-wrap: wrap;
  }
  button {
    padding: 8px 16px;
    border: none;
    border-radius: 8px;
    background: var(--surface-elevated, #eee);
    color: var(--text);
    cursor: pointer;
    font-size: 14px;
    font-weight: 500;
  }
  button.primary {
    background: var(--accent);
    color: #fff;
  }
  button.primary.install {
    min-width: 100px;
  }
  button:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }
  button.danger {
    background: #c45c5c;
    color: #fff;
  }
  button.muted-btn {
    background: transparent;
    color: var(--text-muted);
    border: 1px solid var(--border);
  }
  .toggle-advanced {
    width: 100%;
    text-align: left;
    background: transparent;
    border: 1px dashed var(--border);
    color: var(--text-muted);
    font-weight: 400;
  }
  .advanced {
    border-style: dashed;
  }
  .curator {
    margin-top: 12px;
  }
  .row {
    display: flex;
    gap: 8px;
    margin-top: 8px;
    flex-wrap: wrap;
    align-items: center;
  }
  .curator input {
    flex: 1;
    min-width: 140px;
    padding: 6px 8px;
    font-size: 13px;
  }
  .muted {
    color: var(--text-muted);
    font-size: 14px;
  }
</style>
