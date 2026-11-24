// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
fn main() {
    uniffi::generate_scaffolding("src/mycena.udl").expect("failed to generate uniffi scaffolding");
}
