// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
fn main() {
    if let Err(e) = uniffi::generate_scaffolding("src/mycena.udl") {
        panic!("failed to generate uniffi scaffolding: {e}");
    }
}
