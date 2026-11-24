# Mycena Relay — Fly.io deployment

This is the deployment path for the project relay on Fly.io. A version tag runs it from CI; `scripts/deploy-relay-fly.sh` runs it by hand.

Single image definition: **`Dockerfile.relay`** at the **repository root** (workspace build context).
Fly app config: **`deploy/relay/fly.toml`** (no `[build]` block — deploy script and CI pass `--dockerfile Dockerfile.relay`).

## One-time setup

### 1. Fly.io CLI

```bash
curl -L https://fly.io/install.sh | sh
fly auth login
```

The deploy script creates the app and volume if missing. For a fully manual one-time setup (names must match `fly.toml` `app` and `[mounts] source`):

```bash
fly apps create mycena-relay --org personal   # or your org; match fly.toml `app`
fly volumes create mycena_relay_data \
  --app mycena-relay \
  --region fra \
  --size 1
```

### 2. GitHub Actions

Create a deploy token and add **`FLY_API_TOKEN`** under **Settings → Secrets and variables → Actions**.

The workflow **`.github/workflows/deploy.yml`** runs on tag **`v*.*.*`** (and manual dispatch). It checks that **`MYCENA_STORAGE_KEY`** is set on the Fly app before deploy.

### 3. Manual deploy (same as CI)

From the **repository root**:

```bash
bash scripts/deploy-relay-fly.sh
```

With extra `flyctl deploy` flags:

```bash
bash scripts/deploy-relay-fly.sh --strategy immediate
```

### 4. Local Docker build (smoke)

```bash
docker build -f Dockerfile.relay -t mycena-relay:local .
docker run --rm -p 4001:4001/tcp -p 4001:4001/udp -p 8080:8080 mycena-relay:local
curl -s http://localhost:8080/health
```

### 5. Stable relay identity (required on Fly)

Fly has no OS keyring; without a fixed storage key the libp2p peer id changes on every deploy and clients lose bootstrap.

```bash
fly secrets set MYCENA_STORAGE_KEY="$(openssl rand -hex 32)" -a mycena-relay
```

Identity is persisted on the mounted volume at `/data/.mycena-relay/identity`.

### 6. Dedicated IPv4 for libp2p TCP/UDP

Shared Fly ingress breaks raw libp2p Noise on port 4001. Allocate once:

```bash
fly ips allocate-v4 --yes -a mycena-relay
```

After deploy, update `crates/mycena-core/src/bootstrap.rs` (`RELAY_IPV4`, `RELAY_PEER_ID`, `BOOTSTRAP_PEERS`) from:

```bash
curl -s https://mycena-relay.fly.dev/
```

Clients must dial the dedicated `/ip4/…` address, not `/dns4/mycena-relay.fly.dev/…`.

### 7. Logs and peer id

```bash
fly logs --app mycena-relay | grep -i peer
```

## Notes

- **GitHub** only runs workflows under **`.github/workflows/`** at the repo root; there is no duplicate workflow under `deploy/relay/.github`.
- Fly free tier: shared CPU / 256 MB is enough for a small relay; scale when needed.
