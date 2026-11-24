<script>
  // SPDX-License-Identifier: AGPL-3.0-or-later
  // Copyright (C) 2026 Mycena Project
  import { loadDataHandlingCopy } from "../lib/dataHandlingCopy.js";

  let { open = $bindable(false) } = $props();

  const copy = loadDataHandlingCopy();

  function openTechnicalPolicy() {
    window.open("/privacy_policy.html", "_blank", "noopener,noreferrer");
  }

  function close() {
    open = false;
  }
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={close}>
    <div
      class="panel"
      role="dialog"
      aria-labelledby="data-handling-title"
      onclick={(e) => e.stopPropagation()}
    >
      <header class="panel-header">
        <h2 id="data-handling-title">{copy.title}</h2>
        <button type="button" class="icon-close" onclick={close} aria-label={copy.close_button}>
          ×
        </button>
      </header>
      <div class="panel-body">
        <p class="intro">{copy.intro}</p>
        {#each copy.sections as section}
          <section class="block">
            <h3>{section.heading}</h3>
            {#if section.bullets?.length}
              <ul>
                {#each section.bullets as bullet}
                  <li>{bullet}</li>
                {/each}
              </ul>
            {/if}
            {#each section.paragraphs ?? [] as paragraph}
              <p>{paragraph}</p>
            {/each}
          </section>
        {/each}
        <p class="footnote">{copy.footnote}</p>
        <button type="button" class="linkish" onclick={openTechnicalPolicy}>
          {copy.technical_policy_link_label}
        </button>
      </div>
      <footer class="panel-footer">
        <button type="button" onclick={close}>{copy.close_button}</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 1000;
    background: rgba(0, 0, 0, 0.45);
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
  }
  .panel {
    background: var(--surface, #fff);
    color: var(--text, #1a1a1a);
    border-radius: 12px;
    max-width: 32rem;
    width: 100%;
    max-height: min(90vh, 640px);
    display: flex;
    flex-direction: column;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.2);
  }
  .panel-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    padding: 20px 20px 8px;
    border-bottom: 1px solid var(--border, #e5e5e5);
  }
  .panel-header h2 {
    margin: 0;
    font-size: 1.15rem;
    line-height: 1.3;
  }
  .icon-close {
    border: none;
    background: transparent;
    font-size: 1.5rem;
    line-height: 1;
    cursor: pointer;
    color: var(--text-muted, #666);
    padding: 0 4px;
  }
  .panel-body {
    overflow-y: auto;
    padding: 16px 20px;
    font-size: 0.95rem;
    line-height: 1.5;
  }
  .intro {
    margin: 0 0 16px;
    color: var(--text-secondary, #444);
  }
  .block {
    margin-bottom: 16px;
  }
  .block h3 {
    margin: 0 0 8px;
    font-size: 0.95rem;
    font-weight: 600;
  }
  .block ul {
    margin: 0;
    padding-left: 1.2rem;
  }
  .block li {
    margin-bottom: 6px;
  }
  .block p {
    margin: 0 0 6px;
  }
  .footnote {
    margin: 8px 0 16px;
    font-size: 0.9rem;
    color: var(--text-muted, #555);
  }
  .linkish {
    background: none;
    border: none;
    padding: 0;
    color: var(--accent, #2563eb);
    text-decoration: underline;
    cursor: pointer;
    font-size: 0.9rem;
    text-align: left;
  }
  .panel-footer {
    padding: 12px 20px 16px;
    border-top: 1px solid var(--border, #e5e5e5);
    display: flex;
    justify-content: flex-end;
  }
  .panel-footer button {
    padding: 8px 16px;
    border-radius: 8px;
    border: 1px solid var(--border, #ccc);
    background: var(--surface, #fff);
    cursor: pointer;
  }
</style>
