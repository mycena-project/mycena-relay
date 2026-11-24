// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
import { mount } from "svelte";
import App from "./App.svelte";
import "./app-theme.css";

mount(App, {
  target: document.getElementById("app"),
});
