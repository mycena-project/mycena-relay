#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
cargo install cargo-ndk || true

# Use -t / -o (cargo-ndk flags). Long forms like --output are passed through to `cargo` and break the build.
cargo ndk \
  -t aarch64-linux-android \
  -t armv7-linux-androideabi \
  -t x86_64-linux-android \
  -o android/rust-build \
  build --release -p mycena-ffi

cargo run -p mycena-ffi --bin uniffi-bindgen -- generate \
  crates/mycena-ffi/src/mycena.udl \
  --language kotlin \
  --out-dir android/uniffi-bindings/src/main/kotlin/

# When ktlint is missing, UniFFI can emit `} fun` without a newline; repair invalid Kotlin.
perl -0777 -pi -e 's/\}\s+fun `addBootstrapPeer`/}\n\nfun `addBootstrapPeer`/' \
  android/uniffi-bindings/src/main/kotlin/uniffi/mycena/mycena.kt

cd android
./gradlew assembleDebug
