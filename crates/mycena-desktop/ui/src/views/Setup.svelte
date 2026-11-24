<script>
  // SPDX-License-Identifier: AGPL-3.0-or-later
  // Copyright (C) 2026 Mycena Project
  import { invoke } from "@tauri-apps/api/core";
  import { loadOnboardingCopy, resolvePositioningVariant } from "../lib/onboardingCopy.js";
  import { loadDataHandlingCopy } from "../lib/dataHandlingCopy.js";
  import DataHandling from "./DataHandling.svelte";

  /** @type {{ onstart?: (config: { dbPath: string; displayName: string; bootstrapPeers: string[] }) => void | Promise<void> }} */
  let { onstart } = $props();

  const copy = loadOnboardingCopy();
  const positioningVariant = resolvePositioningVariant(copy);
  const dataHandlingCopy = loadDataHandlingCopy();

  let showDataHandling = $state(false);

  let step = $state(0);
  let displayName = $state("");
  let error = $state("");
  let starting = $state(false);
  let dbPath = $state("");

  const progress = $derived((step + 1) / 4);

  const identityLocked =
    $derived(error.includes("decrypt") || error.includes("ed25519_identity"));

  $effect(() => {
    (async () => {
      try {
        dbPath = (await invoke("get_default_db_path")) ?? "";
      } catch {
        dbPath = "";
      }
    })();
  });

  async function startMycena() {
    if (!displayName.trim()) return;
    error = "";
    starting = true;
    try {
      dbPath = dbPath || (await invoke("get_default_db_path"));
      await invoke("reset_local_data", { dbPath });
      displayName = displayName.trim();
      await onstart?.({
        dbPath,
        displayName,
        bootstrapPeers: [],
      });
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      starting = false;
    }
  }

  async function finish() {
    await startMycena();
  }

  async function resetAndRetry() {
    if (!displayName.trim()) return;
    error = "";
    starting = true;
    try {
      const path = dbPath || (await invoke("get_default_db_path"));
      await invoke("reset_local_data", { dbPath: path });
      dbPath = path;
      await onstart?.({
        dbPath: path,
        displayName: displayName.trim(),
        bootstrapPeers: [],
      });
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      starting = false;
    }
  }
</script>

<div class="onboarding-shell">
  <progress class="progress" max="1" value={progress} aria-label="Onboarding progress"></progress>

  {#if step === 0}
    <div class="onboarding">
      <div class="icon">🍄</div>
      <h1 class="headline">{positioningVariant.headline}</h1>
      <div class="actions single">
        <button onclick={() => (step = 1)}>{copy.actions.next} →</button>
      </div>
    </div>
  {:else if step === 1}
    <div class="onboarding">
      <h2>{copy.why.title}</h2>
      <ul class="bullets">
        {#each copy.why.bullets as bullet}
          <li>{bullet}</li>
        {/each}
      </ul>
      <div class="actions">
        <button class="secondary" onclick={() => (step = 0)}>{copy.actions.back}</button>
        <button onclick={() => (step = 2)}>{copy.actions.next} →</button>
      </div>
    </div>
  {:else if step === 2}
    <div class="onboarding scroll">
      <h2>{copy.faq.title}</h2>
      <div class="faq-list">
        {#each copy.faq.items as item}
          <div class="faq-item">
            <strong>{item.question}</strong>
            <p>{item.answer}</p>
          </div>
        {/each}
      </div>
      <div class="actions">
        <button class="secondary" onclick={() => (step = 1)}>{copy.actions.back}</button>
        <button onclick={() => (step = 3)}>{copy.actions.next} →</button>
      </div>
    </div>
  {:else}
    <div class="onboarding">
      <h2>{copy.completion.title}</h2>
      <p class="muted">{copy.completion.body}</p>
      <p class="fresh-note">{copy.completion.fresh_identity_note}</p>
      <input
        bind:value={displayName}
        placeholder={copy.completion.name_placeholder}
        aria-label={copy.completion.name_hint}
        onkeydown={(e) => {
          if (e.key === "Enter") void finish();
        }}
      />
      <p class="privacy-note">{copy.completion.name_privacy_note}</p>
      {#if error}
        <p class="error" role="alert">{error}</p>
        {#if identityLocked}
          <p class="hint">
            Local identity files cannot be unlocked (often after a macOS keychain change or an older
            build). Resetting creates a new identity and clears local messages on this device.
          </p>
          <button type="button" class="secondary" onclick={resetAndRetry} disabled={starting}>
            Reset local data and try again
          </button>
        {/if}
      {/if}
      <div class="actions">
        <button class="secondary" onclick={() => (step = 2)}>{copy.actions.back}</button>
        <button onclick={finish} disabled={!displayName.trim() || starting}>
          {starting ? "Starting…" : copy.completion.start_button}
        </button>
      </div>
    </div>
  {/if}
  <p class="data-handling-link-wrap">
    <button type="button" class="data-handling-link" onclick={() => (showDataHandling = true)}>
      {dataHandlingCopy.title}
    </button>
  </p>
</div>

<DataHandling bind:open={showDataHandling} />

<style>
  .onboarding-shell {
    min-height: 100vh;
    background: var(--bg);
    color: var(--text);
  }
  .progress {
    width: 100%;
    height: 4px;
    border: none;
    appearance: none;
  }
  .progress::-webkit-progress-bar {
    background: var(--border);
  }
  .progress::-webkit-progress-value {
    background: var(--accent);
  }
  .progress::-moz-progress-bar {
    background: var(--accent);
  }
  .onboarding {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    min-height: calc(100vh - 4px);
    gap: 20px;
    padding: 40px;
    max-width: 520px;
    margin: 0 auto;
  }
  .onboarding.scroll {
    justify-content: flex-start;
    padding-top: 32px;
    padding-bottom: 32px;
  }
  .icon {
    font-size: 64px;
  }
  .headline {
    font-size: 26px;
    font-weight: 600;
    margin: 0;
    text-align: center;
    line-height: 1.35;
    color: var(--text);
  }
  h2 {
    font-size: 22px;
    font-weight: 600;
    margin: 0;
    text-align: center;
    color: var(--text);
  }
  p {
    text-align: center;
    color: var(--text-muted);
    margin: 0;
  }
  .muted {
    font-size: 14px;
  }
  .privacy-note {
    font-size: 12px;
    color: var(--text-muted);
    width: 100%;
    text-align: left;
  }
  .fresh-note {
    font-size: 13px;
    line-height: 1.45;
    color: var(--text-secondary);
    width: 100%;
    text-align: left;
    padding: 12px 14px;
    border-radius: 8px;
    background: var(--surface-elevated, var(--surface));
    border: 1px solid var(--border);
  }
  .bullets {
    width: 100%;
    margin: 0;
    padding-left: 1.25rem;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .bullets li {
    font-size: 16px;
    line-height: 1.45;
    color: var(--text);
  }
  .faq-list {
    width: 100%;
    display: flex;
    flex-direction: column;
    gap: 20px;
  }
  .faq-item strong {
    display: block;
    margin-bottom: 6px;
    color: var(--text);
    font-size: 15px;
  }
  .faq-item p {
    text-align: left;
    font-size: 14px;
    line-height: 1.45;
  }
  .error {
    color: var(--danger);
    font-size: 13px;
    text-align: center;
    width: 100%;
  }
  .hint {
    font-size: 12px;
    text-align: center;
    width: 100%;
    line-height: 1.4;
    color: var(--text-muted);
  }
  .actions {
    display: flex;
    gap: 12px;
    width: 100%;
    margin-top: 8px;
  }
  .actions.single button {
    width: 100%;
  }
  .actions button {
    flex: 1;
  }
  input[type="text"],
  input:not([type="checkbox"]) {
    width: 100%;
    padding: 12px 16px;
    border-radius: 8px;
    border: 1px solid var(--border);
    background: var(--surface);
    color: var(--text);
    font-size: 16px;
  }
  button {
    padding: 12px;
    background: var(--accent);
    color: #fff;
    border: none;
    border-radius: 8px;
    font-size: 15px;
    cursor: pointer;
  }
  button.secondary {
    background: transparent;
    color: var(--accent);
    border: 1px solid var(--accent);
  }
  button:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .data-handling-link-wrap {
    position: fixed;
    bottom: 16px;
    left: 0;
    right: 0;
    text-align: center;
    margin: 0;
    pointer-events: none;
  }
  .data-handling-link {
    pointer-events: auto;
    background: none;
    border: none;
    color: var(--text-muted);
    font-size: 13px;
    text-decoration: underline;
    cursor: pointer;
    padding: 8px 12px;
  }
</style>
