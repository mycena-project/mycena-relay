#!/usr/bin/env bash
# Mirror primary CI cargo gates for mycelium-relay.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

echo "ci-check: cargo fmt --check"
cargo fmt --all -- --check

echo "ci-check: cargo clippy"
cargo clippy -p mycelium-relay -p mycelium-core -p mycelium-node --all-targets -- -D warnings

echo "ci-check: tests"
cargo test -p mycelium-relay -p mycelium-core -p mycelium-node

echo "ci-check: OK"
