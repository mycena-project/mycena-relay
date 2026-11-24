// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project

import deCopy from "@onboarding-copy/de.json";
import enCopy from "@onboarding-copy/en.json";

const LOCALES = { de: deCopy, en: enCopy };
const VARIANT_STORAGE_KEY = "onboarding_positioning_variant";

function resolveLocaleTag() {
  const lang = (navigator.language || "en").split("-")[0].toLowerCase();
  return LOCALES[lang] ? lang : "en";
}

/** @returns {import("@onboarding-copy/en.json")} */
export function loadOnboardingCopy() {
  return LOCALES[resolveLocaleTag()] ?? enCopy;
}

/**
 * Pick a positioning variant once per browser profile (A/B-ready).
 * @param {ReturnType<typeof loadOnboardingCopy>} copy
 */
export function resolvePositioningVariant(copy) {
  const stored = localStorage.getItem(VARIANT_STORAGE_KEY);
  if (stored && copy.positioning.variants[stored]) {
    return { id: stored, ...copy.positioning.variants[stored] };
  }

  const keys = Object.keys(copy.positioning.variants);
  let chosenId = copy.positioning.default_variant;
  if (keys.length === 1) {
    chosenId = keys[0];
  } else if (keys.length > 1) {
    chosenId = keys[Math.floor(Math.random() * keys.length)];
  }

  localStorage.setItem(VARIANT_STORAGE_KEY, chosenId);
  return { id: chosenId, ...copy.positioning.variants[chosenId] };
}
