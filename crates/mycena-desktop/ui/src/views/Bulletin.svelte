<script>
  // SPDX-License-Identifier: AGPL-3.0-or-later
  // Copyright (C) 2026 Mycena Project
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";

  let scope = $state("mycena/general");
  let title = $state("");
  let body = $state("");
  let posts = $state([]);
  let error = $state("");

  async function loadPosts() {
    try {
      posts = await invoke("bulletins_for_scope", { scope });
      error = "";
    } catch (e) {
      error = String(e);
    }
  }

  async function ensureSubscribed() {
    try {
      await invoke("subscribe_bulletin_scope", { scope });
    } catch (e) {
      // Non-fatal — may already be subscribed at the gossip layer.
      console.debug("subscribe_bulletin_scope", e);
    }
  }

  async function refresh() {
    await ensureSubscribed();
    await loadPosts();
  }

  async function post() {
    error = "";
    if (!title || !body) return;
    try {
      await invoke("post_bulletin", { scope, title, body, ttlSecs: 86400 });
      title = "";
      body = "";
      await loadPosts();
    } catch (e) {
      error = String(e);
    }
  }

  $effect(() => {
    scope;
    refresh();
    const unsubs = [];
    (async () => {
      unsubs.push(await listen("bulletin-updated", loadPosts));
    })();
    return () => unsubs.forEach((u) => u?.());
  });
</script>

<div class="bulletin">
  {#if error}
    <p class="error">{error}</p>
  {/if}
  <section>
    <h3>Post</h3>
    <input bind:value={scope} placeholder="Scope" />
    <input bind:value={title} placeholder="Title" />
    <textarea bind:value={body} placeholder="Body"></textarea>
    <button type="button" onclick={post}>Post Bulletin</button>
  </section>
  <section>
    <h3>Board — {scope}</h3>
    {#if posts.length === 0}
      <p class="hint">No posts in this scope yet.</p>
    {:else}
      {#each posts as p (p.id)}
        <article>
          <strong>{p.title}</strong>
          <div>{p.from_display_name} — {p.scope}</div>
          <p>{p.body}</p>
        </article>
      {/each}
    {/if}
  </section>
</div>

<style>
  .bulletin {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 16px;
    padding: 16px;
    height: 100%;
    overflow: auto;
  }
  section {
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  input,
  textarea {
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px;
    background: none;
    color: inherit;
  }
  textarea {
    min-height: 120px;
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
  .error {
    grid-column: 1 / -1;
    color: #b45309;
    margin: 0;
  }
</style>
