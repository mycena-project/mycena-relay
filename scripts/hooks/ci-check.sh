#!/usr/bin/env bash
# Mirror primary CI cargo gates for mycena-relay.
set -euo pipefail
# macOS 27: ld-1267 cannot read arm64e.x1 in the CLT SDK. See macos-sdk.sh.
# shellcheck disable=SC1091
source "$(git rev-parse --show-toplevel)/scripts/hooks/macos-sdk.sh"
use_linkable_macos_sdk
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

echo "ci-check: cargo fmt --check"
cargo fmt --all -- --check

echo "ci-check: cargo clippy"
cargo clippy -p mycena-relay -p mycena-core -p mycena-node --all-targets -- -D warnings

echo "ci-check: tests"
cargo test -p mycena-relay -p mycena-core -p mycena-node

echo "ci-check: OK"
