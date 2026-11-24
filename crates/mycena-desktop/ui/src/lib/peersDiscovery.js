// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project

/** Grace period before "nobody in range" (Bridgefy-style silent hang avoidance). */
export const SEARCH_GRACE_MS = 25_000;

/** How long a BLE sighting counts as "devices nearby". */
export const NEARBY_RECENT_MS = 90_000;

/** Restart BLE scan when no progress for this long (invisible to user). */
export const SCAN_RESTART_STALE_MS = 30_000;

export const PeersDiscoveryPhase = {
  Idle: "idle",
  Searching: "searching",
  NobodyInRange: "nobody_in_range",
  DevicesNearby: "devices_nearby",
  Connected: "connected",
  Connecting: "connecting",
};

const COPY = {
  connecting: {
    title: "Connecting…",
    body: "Setting up a secure link. This usually takes a few seconds.",
  },
  idle: {
    title: "Nearby discovery is off",
    body: "Turn on Bluetooth mesh in Settings for automatic nearby connections, or scan a QR code above.",
  },
  searching: {
    title: "Searching for nearby devices…",
    body: "Bluetooth and Wi‑Fi stay on in the background. This can take up to a minute.",
  },
  nobody: {
    title: "No one nearby right now",
    body: "That is normal. Open Connect on another phone nearby, or scan each other's QR code once.",
  },
  nearby: {
    title: "Devices nearby",
    body: "Mycena devices are in range. Connecting automatically…",
  },
};

/**
 * @param {import('./peersDiscovery.js').MeshDiscoverySnapshot} snapshot
 * @returns {{ phase: string, title: string, body: string, showProgress: boolean }}
 */
export function reduce(snapshot) {
  if (snapshot.connectingFromInvite) {
    return presentation(
      PeersDiscoveryPhase.Connecting,
      COPY.connecting.title,
      COPY.connecting.body,
      true,
    );
  }
  if (snapshot.connectedPeerCount > 0) {
    return presentation(PeersDiscoveryPhase.Connected, "", "", false);
  }
  if (!snapshot.bleBootstrapEnabled || !snapshot.blePermissionsGranted) {
    return presentation(
      PeersDiscoveryPhase.Idle,
      COPY.idle.title,
      COPY.idle.body,
      false,
    );
  }
  if (!snapshot.scanActive) {
    return presentation(
      PeersDiscoveryPhase.Searching,
      COPY.searching.title,
      COPY.searching.body,
      true,
    );
  }

  const searchAnchorMs = Math.max(
    snapshot.scanEpochStartedAtMs,
    snapshot.lastScanRestartAtMs,
  );
  const searchingElapsed = snapshot.nowMs - searchAnchorMs;

  const recentNearby =
    snapshot.lastNearbySeenAtMs != null &&
    snapshot.nowMs - snapshot.lastNearbySeenAtMs <= NEARBY_RECENT_MS;

  if (recentNearby && snapshot.nearbyDeviceCount > 0) {
    return presentation(
      PeersDiscoveryPhase.DevicesNearby,
      COPY.nearby.title,
      COPY.nearby.body,
      true,
    );
  }

  if (searchingElapsed < SEARCH_GRACE_MS) {
    return presentation(
      PeersDiscoveryPhase.Searching,
      COPY.searching.title,
      COPY.searching.body,
      true,
    );
  }

  return presentation(
    PeersDiscoveryPhase.NobodyInRange,
    COPY.nobody.title,
    COPY.nobody.body,
    false,
  );
}

/**
 * @param {import('./peersDiscovery.js').MeshDiscoverySnapshot} snapshot
 */
export function shouldRestartBleScan(snapshot) {
  if (!snapshot.scanActive || !snapshot.bleBootstrapEnabled) return false;
  if (snapshot.connectedPeerCount > 0) return false;
  const anchor = Math.max(
    snapshot.scanEpochStartedAtMs,
    snapshot.lastScanRestartAtMs,
  );
  const lastProgress = Math.max(anchor, snapshot.lastNearbySeenAtMs ?? anchor);
  return snapshot.nowMs - lastProgress >= SCAN_RESTART_STALE_MS;
}

function presentation(phase, title, body, showProgress) {
  return { phase, title, body, showProgress };
}

/**
 * @typedef {Object} MeshDiscoverySnapshot
 * @property {boolean} bleBootstrapEnabled
 * @property {boolean} blePermissionsGranted
 * @property {boolean} scanActive
 * @property {number} scanEpochStartedAtMs
 * @property {number} lastScanRestartAtMs
 * @property {number|null} lastNearbySeenAtMs
 * @property {number} nearbyDeviceCount
 * @property {number} connectedPeerCount
 * @property {boolean} connectingFromInvite
 * @property {number} nowMs
 */
