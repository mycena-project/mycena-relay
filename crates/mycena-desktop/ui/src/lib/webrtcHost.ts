// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
/** WebRTC RTCDataChannel host for desktop (phone↔desktop direct path). */

import { invoke } from "@tauri-apps/api/core";

const MEDIUM_WEBRTC = 3;
const CHANNEL_LABEL = "mycena-mesh";
const TICK_MS = 400;

/** @typedef {{ from_peer: string, session_id: string, kind: number, payload: string, ice_mline_index?: number | null }} WebRtcSignal */

/** @type {Map<string, { pc: RTCPeerConnection, dc: RTCDataChannel | null, sessionId: string }>} */
const peers = new Map();

async function stunServers() {
  const urls = await invoke("webrtc_stun_servers");
  return /** @type {string[]} */ (urls).map((url) => ({ urls: url }));
}

/**
 * @param {string} peerId
 * @param {string} sessionId
 */
async function ensurePeer(peerId, sessionId) {
  let entry = peers.get(peerId);
  if (entry && entry.sessionId === sessionId) {
    return entry;
  }
  if (entry) {
    entry.pc.close();
    peers.delete(peerId);
  }
  const pc = new RTCPeerConnection({ iceServers: await stunServers() });
  entry = { pc, dc: null, sessionId };
  peers.set(peerId, entry);

  pc.onicecandidate = (ev) => {
    if (!ev.candidate) return;
    invoke("webrtc_post_signal", {
      toPeer: peerId,
      sessionId,
      kind: 2,
      payload: JSON.stringify(ev.candidate.toJSON()),
      iceMlineIndex: ev.candidate.sdpMLineIndex ?? null,
    }).catch(() => {});
  };

  pc.ondatachannel = (ev) => {
    wireDataChannel(peerId, entry, ev.channel);
  };

  return entry;
}

/**
 * @param {string} peerId
 * @param {{ pc: RTCPeerConnection, dc: RTCDataChannel | null, sessionId: string }} entry
 * @param {RTCDataChannel} dc
 */
function wireDataChannel(peerId, entry, dc) {
  entry.dc = dc;
  dc.binaryType = "arraybuffer";
  dc.onopen = () => {
    invoke("webrtc_set_link_ready", { peer: peerId, ready: true }).catch(() => {});
  };
  dc.onclose = () => {
    invoke("webrtc_set_link_ready", { peer: peerId, ready: false }).catch(() => {});
    entry.dc = null;
  };
  dc.onmessage = (ev) => {
    const bytes = new Uint8Array(/** @type {ArrayBuffer} */ (ev.data));
    invoke("webrtc_ingest_frame", { fromPeer: peerId, frame: Array.from(bytes) }).catch(
      () => {},
    );
  };
}

/**
 * @param {string} peerId
 * @param {string} sessionId
 */
async function createOffer(peerId, sessionId) {
  const entry = await ensurePeer(peerId, sessionId);
  const dc = entry.pc.createDataChannel(CHANNEL_LABEL, { ordered: true });
  wireDataChannel(peerId, entry, dc);
  const offer = await entry.pc.createOffer();
  await entry.pc.setLocalDescription(offer);
  await invoke("webrtc_post_signal", {
    toPeer: peerId,
    sessionId,
    kind: 0,
    payload: offer.sdp ?? "",
    iceMlineIndex: null,
  });
}

/**
 * @param {WebRtcSignal} signal
 */
async function handleInboundSignal(signal) {
  const { from_peer: peerId, session_id: sessionId, kind, payload, ice_mline_index: mline } =
    signal;
  const entry = await ensurePeer(peerId, sessionId);
  if (kind === 0) {
    await entry.pc.setRemoteDescription({ type: "offer", sdp: payload });
    const answer = await entry.pc.createAnswer();
    await entry.pc.setLocalDescription(answer);
    await invoke("webrtc_post_signal", {
      toPeer: peerId,
      sessionId,
      kind: 1,
      payload: answer.sdp ?? "",
      iceMlineIndex: null,
    });
    return;
  }
  if (kind === 1) {
    await entry.pc.setRemoteDescription({ type: "answer", sdp: payload });
    return;
  }
  if (kind === 2 && payload) {
    const init = JSON.parse(payload);
    if (mline != null) init.sdpMLineIndex = mline;
    await entry.pc.addIceCandidate(init);
  }
}

async function flushOutbound() {
  const frames = await invoke("webrtc_drain_bearer_outbound", { max: 12 });
  for (const item of /** @type {{ to_peer: string, frame: number[], medium: number }[]} */ (
    frames
  )) {
    if (item.medium !== MEDIUM_WEBRTC) continue;
    const entry = peers.get(item.to_peer);
    const dc = entry?.dc;
    if (!dc || dc.readyState !== "open") continue;
    dc.send(Uint8Array.from(item.frame));
  }
}

async function tick() {
  const negotiate = await invoke("webrtc_drain_negotiation_requests", { max: 4 });
  for (const peerId of /** @type {string[]} */ (negotiate)) {
    await createOffer(peerId, crypto.randomUUID());
  }
  const signals = await invoke("webrtc_drain_signaling_inbound", { max: 8 });
  for (const signal of /** @type {WebRtcSignal[]} */ (signals)) {
    await handleInboundSignal(signal);
  }
  await flushOutbound();
}

/** Start polling loop; returns stop function. */
export function startWebRtcHost() {
  let stopped = false;
  invoke("webrtc_set_available", { available: true }).catch(() => {});

  const loop = async () => {
    if (stopped) return;
    try {
      await tick();
    } catch (e) {
      console.warn("WebRTC host tick:", e);
    }
    setTimeout(loop, TICK_MS);
  };
  loop();

  return () => {
    stopped = true;
    for (const [, entry] of peers) {
      entry.pc.close();
    }
    peers.clear();
    invoke("webrtc_set_available", { available: false }).catch(() => {});
  };
}
