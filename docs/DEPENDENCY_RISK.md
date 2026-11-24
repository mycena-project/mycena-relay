# Dependency risk register

Supply-chain gates: `deny.toml`, `./scripts/check-dependencies.sh` (cargo-deny + cargo-audit), CycloneDX SBOM on release (`./scripts/generate-sbom.sh`).

Review cadence: **quarterly** or on any `Cargo.lock` change touching a row below.

---

## CI gates

| Tool | Purpose | Fail condition |
|------|---------|----------------|
| **cargo-deny** | SPDX license allow-list; RustSec advisories; duplicate bans | Unlisted license; vulnerability/unsound not in `deny.toml` ignore |
| **cargo-audit** | Second opinion on RustSec DB | Critical/high severity, DoS/RCE categories, or CVSS ≥ 7 — unless listed in [`DEPENDENCY_ADVISORY_ALLOWLIST.tsv`](./DEPENDENCY_ADVISORY_ALLOWLIST.tsv) |
| **cargo-cyclonedx** | SBOM artifact | Release workflow attaches `mycena-sbom-*.cdx.json` |

Allowlisted advisories **must** have a `review_by` date; expired rows fail `./scripts/check-advisory-allowlist.sh` (called from `check-dependencies.sh`).

---

## sled — embedded datastore (decision: **observe + plan migration**)

Mycena pins **`sled 0.34.7`** (`Cargo.toml` workspace dependency). It is the default on-device KV for messages, coin ledger, mini-app store, ratchet sessions, trust caches, and more (see [ARCHITECTURE_RISKS.md §5.3](./ARCHITECTURE_RISKS.md)).

### Maintenance assessment (2026-07-05)

| Signal | Value |
|--------|--------|
| **Pinned release** | `0.34.7` published **2021-09-12** ([crates.io](https://crates.io/crates/sled/0.34.7)) |
| **Latest stable line used** | No `0.34.x` release in ~5 years |
| **Alpha line** | `1.0.0-alpha.124` on **2024-10-11** — API/storage rewrite, **not semver-compatible** with 0.34 |
| **Repository activity** | [spacejam/sled](https://github.com/spacejam/sled): ~9k stars, **171 open issues**, last push **2026-04-04** |
| **Critical GitHub issues** | No `critical` label issues open; reliability concerns tracked generically (data loss, corruption reports in issues #1444, #1534) |
| **Author stance** | README: *“if reliability is your primary constraint, use SQLite”*; 1.0 blocked on [Komora/marble](https://github.com/komora-io) storage rewrite |
| **RustSec** | No current CVE for `sled 0.34.7` in RustSec DB |

### Risk summary

- **Stagnation:** stable branch effectively frozen; production fixes unlikely without migrating to alpha/Komora stack.
- **On-disk format:** upstream warns format may change before 1.0 — we already persist many trees; migration tooling would be required.
- **Operational:** single-writer embedded DB fits mesh nodes; corruption/panic during write is mitigated by `lock_or_recover` but not by engine guarantees.
- **Blast radius:** universal datastore — a sled bug affects messages, coin, mini-apps, and identity-adjacent trees together.

### Decision (2026-07-05)

**Status: continue using sled 0.34.7, active monitoring, staged migration planning.**

Rationale:

1. No RustSec CVE and no practical drop-in replacement without a multi-crate migration sprint.
2. Alpha 1.0 line is unsuitable for silent upgrade (API + on-disk break).
3. Immediate rewrite to SQLite/redb would touch every persistence module — high regression risk before field beta.

### Action plan

| Horizon | Action |
|---------|--------|
| **Now – 2026-09-30** | Monitor [sled releases](https://github.com/spacejam/sled/releases) and RustSec; run `make check-dependencies` in CI; export SBOM each release. |
| **2026-Q4** | Spike: **redb** vs **SQLite** (via `rusqlite`) for *one* non-critical tree (e.g. `transport_meta`) behind feature flag; document export/import from sled. |
| **2027-H1** | If no maintained `0.34.x` patch **or** Komora 1.0 remains alpha-only: execute phased migration (messages + app storage first, coin/ledger last); require `sled export` → new engine import in upgrade path. |
| **Trigger to accelerate** | RustSec advisory on sled; reproducible corruption in field; or two consecutive quarters with zero upstream maintenance commits on `main`. |

---

## libp2p transitive stack (hickory / quinn / quick-xml)

| Advisory | Crate | Via | Exposure | Remediation |
|----------|-------|-----|----------|-------------|
| RUSTSEC-2026-0118/0119, GHSA-3v94-mw7p-v465, GHSA-q2qq-hmj6-3wpp, GHSA-5j98-2g5x-46v6, GHSA-6f2x-v7q7-m7m5 | hickory-proto / hickory-resolver 0.25.2 | libp2p-dns 0.44, libp2p-mdns 0.48 | DNS DoS and DNSSEC-status APIs on the 0.25 resolver | libp2p 0.57.1 ships hickory 0.26, and also libp2p-gossipsub 0.50. The vendored mesh hot-swap fork is 0.49.4, so this stack stays on 0.25.2 until that fork is ported. |
| RUSTSEC-2026-0487/0489/0490/0491, 0504/0505/0511/0512/0514/0530/0531/0532/0549/0550/0551 (review 2026-11-24) | hickory-proto / hickory-resolver 0.25.2 | libp2p-dns 0.44, libp2p-mdns 0.48 | Zone-file parsing panics, DNS header memory amplification, DNSSEC validation bugs | No fix on the 0.25 line. Mycena resolves only the bootstrap relay hostname, DNSSEC validation is off and no zone files are parsed. Fixed with the libp2p 0.57 upgrade (needs the gossipsub fork port). Allowlisted until 2027-03-31. |
| GHSA-vxx9-2994-q338 | yamux 0.12.1 (0.13.10 is also linked) | libp2p-yamux 0.47 | Remote panic on a malformed yamux 0.12 data frame | 0.12 has no patched release. libp2p-yamux 0.48 drops it, which is the same libp2p 0.57 / gossipsub 0.50 port. |
| RUSTSEC-2026-0194/0195 | quick-xml 0.37/0.39 | uniffi, plist | XML bomb if parsing untrusted XML | No peer XML ingest; bump uniffi/plist when fixed |
| GHSA-wrw7-89jp-8q8g | glib 0.18.5 | tauri → gtk 0.18 → webkit2gtk | Unsound `VariantStrIter` | Patched line is glib 0.20. That is a different gtk-rs stack than Tauri 2.11 / wry 0.55 ship. |

`libp2p-quic` 0.13.1 is the in-stack fix for GHSA-5hq8-qhww-jm7q. `quinn-proto` 0.11.19 and `rustls` 0.23.46 clear the earlier QUIC and TLS advisories. Desktop UI overrides pin `devalue` 5.9.3, `source-map-js` 1.2.2, `nanoid` 3.3.18, and `postcss` 8.5.23.

Allowlisted until **2026-12-31** — see [`DEPENDENCY_ADVISORY_ALLOWLIST.tsv`](./DEPENDENCY_ADVISORY_ALLOWLIST.tsv).

---

## Unmaintained (informational, not CI-blocking)

| Crate | Advisory | Used for | Plan |
|-------|----------|----------|------|
| serde_cbor 0.11 | RUSTSEC-2021-0127 | libp2p CBOR codecs | Migrate when libp2p moves to ciborium/minicbor |
| bincode 1.x | RUSTSEC-2024-0437 | legacy serde snapshots | Replace with postcard/bincode 2 in non-wire paths (coin already uses serde_json for wire) |

---

## SBOM

Each tagged release publishes:

- `mycena-sbom-<tag>.cdx.json` — CycloneDX JSON (workspace, all features)
- `sbom-manifest.json` — generator metadata

Regenerate locally:

```bash
./scripts/generate-sbom.sh v0.1.62
```

---

## Related documents

- [RELEASE_INTEGRITY.md](./RELEASE_INTEGRITY.md) — reproducible builds + M-of-N signing
- [SECURITY_DEBT_REGISTER.md](./SECURITY_DEBT_REGISTER.md) — application-level debt
- [ARCHITECTURE_RISKS.md](./ARCHITECTURE_RISKS.md) — sled blast radius

## zip 0.6 (review 2026-11-24)

zip is now built with `default-features = false, features = ["deflate"]`, which removes AES, zstd and bzip2 support (and RUSTSEC-2026-0403/0410 via zstd). RUSTSEC-2026-0429/0545/0548 remain allowlisted until 2027-03-31: bundles are opened via the central directory (`ZipArchive::by_name`) after the bundle signature check; `extract`, `ZipStreamReader` and AES entries are not used. Planned fix: move to zip 2.x/9.x together with the mini-app bundle format review.

## GitHub Dependabot alerts (review 2026-11-24)

Open Dependabot alerts that remain after the 2026-11-24 updates were dismissed with the reasons below. Re-check them whenever libp2p is upgraded.

| Package | Advisory | Why it does not affect Mycena |
|---------|----------|-------------------------------|
| `yamux` 0.12.1 | GHSA-vxx9-2994-q338 (remote panic) | `libp2p-yamux` 0.47 compiles 0.12 only for its legacy config. Mycena uses `yamux::Config::default()`, which selects yamux **0.13.10** (patched). |
| `zip` 0.6 | GHSA-m665-v7pm-7grj (parser differential) | The differential is between `ZipArchive` and `ZipStreamReader`. Mycena reads bundles only with `ZipArchive` and verifies signatures over the same view; `ZipStreamReader` is never used. zip ≥ 9.0.5 needs Rust 1.89 (toolchain is pinned to 1.88). |
| `zip` 0.6 | GHSA-8hxf-qr8p-62x4 (AES HMAC bypass) | AES support is not compiled in (`default-features = false, features = ["deflate"]`). |
| `hickory-resolver` 0.25 | GHSA-5j98-2g5x-46v6 (DNSSEC result ignored) | DNSSEC validation is off; libp2p-dns only resolves the relay hostname. |
| `hickory-resolver` 0.25 | GHSA-6f2x-v7q7-m7m5 (CNAME query amplification) | Tolerable: a few extra DNS queries when resolving `/dns4` bootstrap addresses. No fix on the 0.25 line that libp2p-dns 0.44 pins. |

The `fuzz/Cargo.lock` workspace and the desktop UI (`vite` ≥ 8.1.6) were updated, so their alerts close on the next scan.
