<script>
  // SPDX-License-Identifier: AGPL-3.0-or-later
  // Copyright (C) 2026 Mycena Project
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import Chat from "./views/Chat.svelte";
  import Groups from "./views/Groups.svelte";
  import Mail from "./views/Mail.svelte";
  import Bulletin from "./views/Bulletin.svelte";
  import Sos from "./views/Sos.svelte";
  import Peers from "./views/Peers.svelte";
  import Share from "./views/Share.svelte";
  import Wallet from "./views/Wallet.svelte";
  import Settings from "./views/Settings.svelte";
  import Setup from "./views/Setup.svelte";
  import MiniApp from "./views/MiniApp.svelte";
  import MiniAppStore from "./views/MiniAppStore.svelte";
  import More from "./views/More.svelte";
  import ConnectionStatus from "./components/ConnectionStatus.svelte";
  import { startWebRtcHost } from "./lib/webrtcHost";

  let booting = $state(true);
  let nodeStarted = $state(false);
  let peerId = $state("");
  let activeView = $state("chat");
  let selectedMiniAppId = $state("");
  let peers = $state(0);
  let forwarded = $state(0);
  let queue = $state(0);
  const views = [
    { id: "chat", label: "Chat" },
    { id: "groups", label: "Groups" },
    { id: "mail", label: "Mail" },
    { id: "bulletin", label: "Bulletin" },
    { id: "sos", label: "SOS" },
    { id: "peers", label: "Connect" },
    { id: "share", label: "Share" },
    { id: "more", label: "More" },
    { id: "wallet", label: "Wallet" },
    { id: "miniapp", label: "Mini apps" },
    { id: "settings", label: "Settings" },
  ];
  let connectivityMode = $state("internet");
  let internetEnabled = $state(true);
  let rendezvousEnabled = $state(false);
  let connectivityDetail = $state("");
  let stopWebRtc = null;

  async function startNode(config) {
    peerId = await invoke("start_node", {
      dbPath: config.dbPath,
      displayName: config.displayName,
      bootstrapPeers: config.bootstrapPeers ?? [],
    });
    nodeStarted = true;
    stopWebRtc?.();
    stopWebRtc = startWebRtcHost();
  }

  onMount(async () => {
    try {
      const profile = await invoke("get_saved_profile");
      if (profile?.can_auto_start && profile.display_name) {
        peerId = await invoke("start_node", {
          dbPath: profile.db_path,
          displayName: profile.display_name,
          bootstrapPeers: [],
        });
        nodeStarted = true;
        stopWebRtc?.();
        stopWebRtc = startWebRtcHost();
      }
    } catch {
      /* first launch or corrupt profile — show setup */
    } finally {
      booting = false;
    }

    const unlisten = await listen("metrics-updated", (event) => {
      peers = event.payload.peers ?? 0;
      forwarded = event.payload.forwarded ?? 0;
      queue = event.payload.queue ?? 0;
      if (event.payload.connectivity) {
        connectivityMode = event.payload.connectivity;
      }
      if (event.payload.internet_enabled !== undefined) {
        internetEnabled = event.payload.internet_enabled;
      }
      if (event.payload.rendezvous_enabled !== undefined) {
        rendezvousEnabled = event.payload.rendezvous_enabled;
      }
      if (event.payload.connectivity_detail) {
        connectivityDetail = event.payload.connectivity_detail;
      }
    });
    const unlistenSettings = await listen("settings-updated", (event) => {
      if (event.payload.internet_enabled !== undefined) {
        internetEnabled = event.payload.internet_enabled;
      }
      if (event.payload.rendezvous_enabled !== undefined) {
        rendezvousEnabled = event.payload.rendezvous_enabled;
      }
      if (event.payload.connectivity_mode) {
        connectivityMode = event.payload.connectivity_mode;
      }
      if (event.payload.connectivity_detail) {
        connectivityDetail = event.payload.connectivity_detail;
      }
    });
    invoke("get_settings").then((settings) => {
      internetEnabled = settings.internet_enabled ?? true;
      rendezvousEnabled = settings.rendezvous_enabled ?? false;
      connectivityMode = settings.connectivity_mode ?? "internet";
      connectivityDetail = settings.connectivity_detail ?? "";
    }).catch(() => {});
    const onOpenMiniApp = (e) => {
      selectedMiniAppId = e.detail?.appId ?? "";
      if (selectedMiniAppId) activeView = "miniapp_run";
    };
    window.addEventListener("mycena-open-miniapp", onOpenMiniApp);
    return () => {
      unlisten();
      unlistenSettings();
      window.removeEventListener("mycena-open-miniapp", onOpenMiniApp);
    };
  });
</script>

{#if booting}
  <div class="boot">Loading…</div>
{:else if !nodeStarted}
  <Setup onstart={startNode} />
{:else}
  <div class="app">
    <nav>
      <div class="logo">Mycena</div>
      <div class="status">
        <span class="dot" class:green={peers > 0}></span>
        {peers} peer{peers !== 1 ? "s" : ""}
      </div>
      {#each views as view}
        <button class:active={activeView === view.id} onclick={() => (activeView = view.id)}>
          {view.label}
        </button>
      {/each}
      <div class="metrics-mini">
        <span>{forwarded} fwd</span>
        <span>{queue} q</span>
      </div>
    </nav>
    <main>
      <ConnectionStatus
        peerCount={peers}
        {connectivityMode}
        {internetEnabled}
        {rendezvousEnabled}
        {connectivityDetail}
      />
      {#if activeView === "chat"}
        <Chat {peerId} />
      {:else if activeView === "groups"}
        <Groups />
      {:else if activeView === "sos"}
        <Sos />
      {:else if activeView === "peers"}
        <Peers {peerId} />
      {:else if activeView === "share"}
        <Share />
      {:else if activeView === "more"}
        <More />
      {:else if activeView === "wallet"}
        <Wallet />
      {:else if activeView === "miniapp"}
        <MiniAppStore
          onOpenApp={(id) => {
            selectedMiniAppId = id;
            activeView = "miniapp_run";
          }}
        />
      {:else if activeView === "miniapp_run"}
        <div class="miniapp-run">
          <button type="button" class="back" onclick={() => (activeView = "miniapp")}>← Mini apps</button>
          <MiniApp appId={selectedMiniAppId} />
        </div>
      {:else if activeView === "settings"}
        <Settings {peerId} />
      {/if}
      <div class="view-panel" class:hidden={activeView !== "mail"}>
        <Mail />
      </div>
      <div class="view-panel" class:hidden={activeView !== "bulletin"}>
        <Bulletin />
      </div>
    </main>
  </div>
{/if}

<style>
  .boot {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100vh;
    color: var(--text-muted);
    font-size: 14px;
  }
  .app {
    display: flex;
    height: 100vh;
  }
  nav {
    width: 180px;
    min-width: 180px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 16px 8px;
    border-right: 1px solid var(--border);
    background: var(--sidebar-bg);
  }
  .logo {
    font-weight: 600;
    font-size: 18px;
    padding: 8px;
    margin-bottom: 12px;
    color: var(--text);
  }
  .status {
    font-size: 12px;
    color: var(--text-muted);
    padding: 4px 8px;
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 8px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--muted);
  }
  .dot.green {
    background: var(--accent);
  }
  nav button {
    text-align: left;
    padding: 8px 12px;
    border-radius: 6px;
    border: none;
    background: none;
    cursor: pointer;
    font-size: 14px;
    color: var(--text-secondary);
    text-transform: capitalize;
  }
  nav button.active {
    background: var(--accent-subtle);
    color: var(--accent);
    font-weight: 500;
  }
  .metrics-mini {
    margin-top: auto;
    font-size: 11px;
    color: var(--text-muted);
    padding: 4px 8px;
    display: flex;
    justify-content: space-between;
  }
  main {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    background: var(--bg);
  }
  .view-panel {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .view-panel.hidden {
    display: none;
  }
  .miniapp-run {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .miniapp-run .back {
    align-self: flex-start;
    margin: 8px 12px;
    padding: 6px 10px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface-elevated);
    color: var(--text);
    cursor: pointer;
    font-size: 13px;
  }
</style>
