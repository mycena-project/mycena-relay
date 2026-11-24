# Mycena Relay

> **Early Beta — expect bugs.** This public repository builds and deploys the bootstrap relay used by [Mycena](https://github.com/mycena-project/Mycena) clients. It is not a second copy of the Mycena app.

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL%20v3-blue.svg)](LICENSE)
[![Mycena Relay CI](https://github.com/mycena-project/mycena-relay/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/mycena-project/mycena-relay/actions/workflows/ci.yml)

**Mycena Relay** is the Internet-facing libp2p circuit relay and rendezvous service that helps Mycena nodes find each other across NAT — a bootstrap aid, not a central message server. Message content stays end-to-end encrypted on the mesh; the relay sees connection metadata only.

**This is not a replacement for emergency services.**

- Live endpoint: `mycena-relay.fly.dev` (Frankfurt region)
- Main app repository: [mycena-project/Mycena](https://github.com/mycena-project/Mycena)
- Beta website: [mycena-beta.vercel.app](https://mycena-beta.vercel.app)

## What this repo contains

This tree mirrors the Mycena Rust workspace crates needed to build `mycena-relay`, synced from the main app repo via `scripts/sync-relay-repo.sh` in Mycena. Deploy configuration lives under `deploy/relay/` (Fly.io).

## Run locally

```bash
cargo run -p mycena-relay
```

Health check: `curl -s http://localhost:8080/health`

## Deploy (Fly.io)

The project relay runs as a single small machine on Fly.io in Frankfurt: `mycena-relay.fly.dev`.

```bash
bash scripts/deploy-relay-fly.sh
```

A `v*.*.*` tag runs the same script from [`.github/workflows/deploy.yml`](.github/workflows/deploy.yml). Setup details: [deploy/relay/README.md](deploy/relay/README.md).

### Cost guard

Fly.io has no hard spending cap. The relay is kept to about **4–5 USD per month**: one `shared-cpu-1x` machine with 256 MB, one 1 GB volume and one dedicated IPv4. [`scripts/fly-budget-guard.sh`](scripts/fly-budget-guard.sh) checks that footprint after every deploy and weekly from [`.github/workflows/fly-budget-guard.yml`](.github/workflows/fly-budget-guard.yml), and scales extra machines back to one.

### Run your own relay (AGPL-3.0)

You may deploy a **public bootstrap relay** for your community. Mycena Relay is licensed under **[AGPL-3.0-or-later](LICENSE)** — if you modify and operate it as a network service, you must offer corresponding source to users.

1. Fork or clone this repo.
2. Follow [deploy/relay/fly.toml](deploy/relay/fly.toml) and [deploy/relay/README.md](deploy/relay/README.md).
3. **Required:** set a stable identity before the first production deploy:
   ```bash
   fly secrets set MYCENA_STORAGE_KEY="$(openssl rand -hex 32)" -a mycena-relay
   ```
   Without `MYCENA_STORAGE_KEY`, the libp2p **peer ID changes on every deploy** and clients lose bootstrap.
4. Point Mycena clients at your relay address (update bootstrap config in the main [Mycena](https://github.com/mycena-project/Mycena) app if needed).

The Mycena project operates `mycena-relay.fly.dev` as a convenience bootstrap; it is not required for LAN-only mesh use.

## Architecture role

```
Mycena App ──► relay circuit (/p2p-circuit) ──► Mycena App
                      │
                      └── rendezvous HTTP API (opt-in peer hints)
```

- **Not** a store-and-forward message hub — nodes relay for each other
- **Does** provide NAT traversal and optional rendezvous registration
- **AGPL note:** if you modify and operate this relay publicly, you must offer corresponding source to users per [LICENSE](LICENSE)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: **github.com/mycena-project/Mycena/security/advisories/new** — [SECURITY.md](SECURITY.md).

## License

Copyright (C) 2026 Mycena Project.

Licensed under [GNU Affero General Public License v3.0 or later](LICENSE) (AGPL-3.0-or-later).
