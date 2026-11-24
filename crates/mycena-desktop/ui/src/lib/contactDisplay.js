// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project

export function contactPeerId(c) {
  return c?.peer_id ?? c?.peerId ?? "";
}

function looksLikeTechnicalId(name, peerId) {
  if (!name) return true;
  if (peerId && name === peerId) return true;
  if (peerId?.length >= 12 && name === peerId.slice(0, 12)) return true;
  if (peerId?.length >= 16 && name === peerId.slice(0, 16)) return true;
  if (name.startsWith("12D3") && name.length <= 20) return true;
  return false;
}

/** @param {Record<string, unknown> | null | undefined} contact */
export function userVisibleContactName(contact, fallback = "Unknown contact") {
  const peerId = contactPeerId(contact);
  const name = String(contact?.display_name ?? contact?.displayName ?? "").trim();
  if (looksLikeTechnicalId(name, peerId)) return fallback;
  return name;
}

export function sanitizeUserError(raw, fallback = "Something went wrong. Please try again.") {
  const text = String(raw ?? "").trim();
  if (!text) return fallback;
  if (/12D3Koo/i.test(text)) return fallback;
  if (/peer[_\s-]?id/i.test(text)) return fallback;
  if (/mycena:\/\//i.test(text)) return fallback;
  if (text.length > 120) return fallback;
  return text;
}
