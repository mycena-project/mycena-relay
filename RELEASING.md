# Releasing

1. CI green on `main`.
2. `git tag -a vX.Y.Z -m "vX.Y.Z"` && `git push origin vX.Y.Z`
3. `.github/workflows/deploy.yml` deploys to Fly.io (requires the `FLY_API_TOKEN` repository secret and `MYCENA_STORAGE_KEY` on the Fly app).
4. Manual deploy: `bash scripts/deploy-relay-fly.sh`. See [deploy/relay/README.md](deploy/relay/README.md).
5. The deploy script runs [`scripts/fly-budget-guard.sh`](scripts/fly-budget-guard.sh) afterwards; it must report "within budget footprint".

**AGPL-3.0-or-later** — operators hosting a modified public relay must offer corresponding source. See [LICENSE](LICENSE).
