# Releasing

1. CI green on `main`.
2. `git tag -a vX.Y.Z -m "vX.Y.Z"` && `git push origin vX.Y.Z`
3. `.github/workflows/deploy.yml` → Fly.io (requires `FLY_API_TOKEN` + `MYCELIUM_STORAGE_KEY` on Fly app). This tag workflow does not install on VPS.
4. VPS, when you intend to install there: `bash scripts/deploy-relay-vps.sh`. See [deploy/vps/README.md](deploy/vps/README.md).

See [deploy/relay/README.md](deploy/relay/README.md) for the Fly path.

**AGPL-3.0-or-later** — operators hosting a modified public relay must offer corresponding source. See [LICENSE](LICENSE).
