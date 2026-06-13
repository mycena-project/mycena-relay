# the maintainer Mycelium family

**You are here:** [mycelium-relay](https://github.com/mycena-project/mycelium-relay) — public libp2p bootstrap relay (Fly.io).

See [Mycelium docs/ECOSYSTEM.md](https://github.com/mycena-project/Mycelium/blob/main/docs/ECOSYSTEM.md) for the full family map.

## Local run

```bash
cargo run -p mycelium-relay
curl -s http://localhost:8080/health
```

## Production deploy

```bash
./scripts/deploy-relay-fly.sh
```

No `make up`/`down` — relay is a single binary service; local = `cargo run`, production = Fly.io.
