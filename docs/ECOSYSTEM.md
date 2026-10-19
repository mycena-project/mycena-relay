# the maintainer Mycelium family

**You are here:** [mycelium-relay](https://github.com/mycena-project/mycelium-relay) — public libp2p bootstrap relay. Install target is the VPS (`relay.example.invalid`); the tagged Fly.io workflow is still in the repo.

See [Mycelium docs/ECOSYSTEM.md](https://github.com/mycena-project/Mycelium/blob/main/docs/ECOSYSTEM.md) for the full family map.

## Local run

```bash
cargo run -p mycelium-relay
curl -s http://localhost:8080/health
```

## Production deploy

```bash
bash scripts/deploy-relay-vps.sh
```

VPS is a single container with host networking, separate from the demo Compose project. Local run remains `cargo run`. Tag pushes still run `scripts/deploy-relay-fly.sh` until that workflow is removed.
