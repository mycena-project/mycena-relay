// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project

import arCopy from "@data-handling-copy/ar.json";
import deCopy from "@data-handling-copy/de.json";
import enCopy from "@data-handling-copy/en.json";
import faCopy from "@data-handling-copy/fa.json";
import ruCopy from "@data-handling-copy/ru.json";
import zhCopy from "@data-handling-copy/zh.json";

const LOCALES = { ar: arCopy, de: deCopy, en: enCopy, fa: faCopy, ru: ruCopy, zh: zhCopy };

function resolveLocaleTag() {
  const lang = (navigator.language || "en").split("-")[0].toLowerCase();
  if (lang === "zh") return "zh";
  return LOCALES[lang] ? lang : "en";
}

/** @returns {typeof enCopy} */
export function loadDataHandlingCopy() {
  return LOCALES[resolveLocaleTag()] ?? enCopy;
}
