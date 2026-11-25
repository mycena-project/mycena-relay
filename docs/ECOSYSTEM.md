# Mycena project family

**You are here:** [mycena-relay](https://github.com/mycena-project/mycena-relay) — public libp2p bootstrap relay, deployed as one small Fly.io machine (`mycena-relay.fly.dev`).

| Repository | Role |
|---|---|
| [Mycena](https://github.com/mycena-project/Mycena) | App: Rust mesh core, Android, desktop, mini-apps |
| [mycena-relay](https://github.com/mycena-project/mycena-relay) | Optional bootstrap relay (this repo) |

See [Mycena docs/ECOSYSTEM.md](https://github.com/mycena-project/Mycena/blob/main/docs/ECOSYSTEM.md) for the full map.

## Local run

```bash
cargo run -p mycena-relay
curl -s http://localhost:8080/health
```

## Production deploy

```bash
bash scripts/deploy-relay-fly.sh
```

The script deploys to Fly.io, pins the app to one machine and runs the cost guard.
