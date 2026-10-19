# Mycelium Relay

> **Early Beta — expect bugs.** This public repository builds and deploys the bootstrap relay used by [Mycelium](https://github.com/mycena-project/Mycelium) clients. It is not a second copy of the Mycelium app.

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL%20v3-blue.svg)](LICENSE)
[![Mycelium Relay CI](https://github.com/mycena-project/mycelium-relay/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/mycena-project/mycelium-relay/actions/workflows/ci.yml)

**Mycelium Relay** is the Internet-facing libp2p circuit relay and rendezvous service that helps Mycelium nodes find each other across NAT — a bootstrap aid, not a central message server. Message content stays end-to-end encrypted on the mesh; the relay sees connection metadata only.

**This is not a replacement for emergency services.**

- Live endpoint: `mycelium-relay.fly.dev` (Frankfurt region)
- Main app repository: [the maintainer/Mycelium](https://github.com/mycena-project/Mycelium)
- Beta website: [mycelium-beta.vercel.app](https://mycelium-beta.vercel.app)

## What this repo contains

This tree mirrors the Mycelium Rust workspace crates needed to build `mycelium-relay`, synced from the main app repo via `scripts/sync-relay-repo.sh` in Mycelium. Deploy configuration lives under `deploy/vps/` (install target) and `deploy/relay/` (Fly.io).

## Run locally

```bash
cargo run -p mycelium-relay
```

Health check: `curl -s http://localhost:8080/health`

## Deploy (VPS)

Install target: `relay.example.invalid` on the VPS. See [deploy/vps/README.md](deploy/vps/README.md).

```bash
bash scripts/deploy-relay-vps.sh
```

That script is the install step. It is not run by CI.

A `v*.*.*` tag still deploys to Fly.io via [`.github/workflows/deploy.yml`](.github/workflows/deploy.yml) and [deploy/relay/README.md](deploy/relay/README.md). VPS does not replace that workflow until the tag deploy is removed.

### Run your own relay (AGPL-3.0)

You may deploy a **public bootstrap relay** for your community. Mycelium Relay is licensed under **[AGPL-3.0-or-later](LICENSE)** — if you modify and operate it as a network service, you must offer corresponding source to users.

1. Fork or clone this repo.
2. Follow [deploy/relay/fly.toml](deploy/relay/fly.toml) and [deploy/relay/README.md](deploy/relay/README.md).
3. **Required:** set a stable identity before the first production deploy:
   ```bash
   fly secrets set MYCELIUM_STORAGE_KEY="$(openssl rand -hex 32)" -a mycelium-relay
   ```
   Without `MYCELIUM_STORAGE_KEY`, the libp2p **peer ID changes on every deploy** and clients lose bootstrap.
4. Point Mycelium clients at your relay address (update bootstrap config in the main [Mycelium](https://github.com/mycena-project/Mycelium) app if needed).

the maintainer operates `mycelium-relay.fly.dev` as a convenience bootstrap; it is not required for LAN-only mesh use.

## Architecture role

```
Mycelium App ──► relay circuit (/p2p-circuit) ──► Mycelium App
                      │
                      └── rendezvous HTTP API (opt-in peer hints)
```

- **Not** a store-and-forward message hub — nodes relay for each other
- **Does** provide NAT traversal and optional rendezvous registration
- **AGPL note:** if you modify and operate this relay publicly, you must offer corresponding source to users per [LICENSE](LICENSE)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: **security@example.invalid** — [SECURITY.md](SECURITY.md).

## License

Copyright (C) 2026 Mycelium Project.

Licensed under [GNU Affero General Public License v3.0 or later](LICENSE) (AGPL-3.0-or-later).
