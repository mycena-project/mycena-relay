<script>
  // SPDX-License-Identifier: AGPL-3.0-or-later
  // Copyright (C) 2026 Mycena Project
  import { invoke } from "@tauri-apps/api/core";
  import { sanitizeUserError, userVisibleContactName } from "../lib/contactDisplay.js";

  let contacts = $state([]);
  let toPeer = $state("");
  let subject = $state("");
  let body = $state("");
  let inbox = $state([]);
  let error = $state("");
  let success = $state("");

  async function refreshInbox() {
    try {
      contacts = await invoke("list_accepted_contacts");
      inbox = await invoke("mail_inbox", { limit: 100 });
    } catch (e) {
      console.error("mail inbox refresh failed", e);
    }
  }

  async function send() {
    success = "";
    if (!toPeer) {
      error = "Select a contact first.";
      return;
    }
    if (!subject.trim() || !body.trim()) {
      error = "Subject and message are required.";
      return;
    }
    try {
      await invoke("send_mail", { toPeer, subject: subject.trim(), body: body.trim() });
      subject = "";
      body = "";
      error = "";
      success = "Mail queued for delivery.";
      await refreshInbox();
    } catch (e) {
      success = "";
      error = sanitizeUserError(e instanceof Error ? e.message : String(e));
    }
  }

  function dismissStatus() {
    error = "";
    success = "";
  }

  $effect(() => {
    refreshInbox();
    const unsubs = [];
    (async () => {
      unsubs.push(await listen("mail-updated", refreshInbox));
    })();
    return () => unsubs.forEach((u) => u?.());
  });
</script>

<div class="mail">
  {#if error}
    <div class="banner error">
      <span>{error}</span>
      <button type="button" class="dismiss" onclick={dismissStatus}>Dismiss</button>
    </div>
  {/if}
  {#if success}
    <div class="banner success">
      <span>{success}</span>
      <button type="button" class="dismiss" onclick={dismissStatus}>Dismiss</button>
    </div>
  {/if}
  <section class="compose">
    <h3>Compose</h3>
    {#if contacts.length > 0}
      <label>
        To
        <select bind:value={toPeer}>
          <option value="">Select contact…</option>
          {#each contacts as c (c.peer_id)}
            <option value={c.peer_id}>{userVisibleContactName(c, "Unknown contact")}</option>
          {/each}
        </select>
      </label>
    {:else}
      <p class="hint">Add a contact on the Connect tab first.</p>
      <input bind:value={toPeer} placeholder="Contact" />
    {/if}
    <input bind:value={subject} placeholder="Subject" />
    <textarea bind:value={body} placeholder="Message"></textarea>
    <button type="button" onclick={send}>Send mail</button>
  </section>
  <section class="inbox">
    <h3>Inbox</h3>
    {#if inbox.length === 0}
      <p class="hint">No mail yet.</p>
    {:else}
      {#each inbox as m (m.id)}
        <article>
          <strong>{m.subject}</strong>
          <div>{m.from_display_name}</div>
          <p>{m.body}</p>
        </article>
      {/each}
    {/if}
  </section>
</div>

<style>
  .mail {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 16px;
    padding: 16px;
    height: 100%;
    overflow: auto;
  }
  .compose,
  .inbox {
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .banner {
    grid-column: 1 / -1;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 10px 12px;
    border-radius: 8px;
    font-size: 14px;
  }
  .banner.error {
    background: #fef3c7;
    color: #92400e;
  }
  .banner.success {
    background: #dcfce7;
    color: #166534;
  }
  .dismiss {
    padding: 4px 10px;
    border: 1px solid currentColor;
    border-radius: 6px;
    background: transparent;
    color: inherit;
    cursor: pointer;
    font-size: 12px;
    flex-shrink: 0;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 13px;
    color: var(--text-muted);
  }
  input,
  select,
  textarea {
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px;
    background: none;
    color: inherit;
  }
  textarea {
    min-height: 140px;
  }
  button {
    padding: 8px 12px;
    border: none;
    border-radius: 6px;
    background: var(--accent);
    color: #fff;
    cursor: pointer;
  }
  article {
    border-top: 1px solid var(--border);
    padding-top: 8px;
  }
  .hint {
    color: var(--text-muted);
    font-size: 14px;
  }
</style>
