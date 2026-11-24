// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
fn main() {
    let dist_index = std::path::Path::new("ui/dist/index.html");
    if !dist_index.exists() {
        panic!(
            "Desktop UI not built (missing ui/dist). Run:\n  \
             cd crates/mycena-desktop/ui && npm install && npm run build"
        );
    }
    tauri_build::build()
}
