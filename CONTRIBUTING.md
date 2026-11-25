# Contributing to Mycena Relay

Early public beta — coordinate with the main [Mycena](https://github.com/mycena-project/Mycena) repository when changes affect shared crates.

## Reporting Issues

| Type | Channel |
|------|---------|
| **Bug** | GitHub Issue with repro steps |
| **Feature** | GitHub Issue with use case |
| **Security** | [private vulnerability reporting](https://github.com/mycena-project/Mycena/security/advisories/new) only — see [SECURITY.md](SECURITY.md) |

## Development

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p mycena-relay
```

## Sync from Mycena

Most crates are mirrored from the main repo:

```bash
# From Mycena repo root:
./scripts/sync-relay-repo.sh /path/to/mycena-relay
```

Prefer implementing shared logic in **Mycena** first, then sync here.

## Commit Conventions

```
feat(relay): add rendezvous rate limit
fix(deploy): correct fly.toml health check path
docs: clarify AGPL obligations for relay operators
```

## Pull Requests

- Keep relay-specific changes focused (deploy, relay binary, ops docs)
- Ensure CI passes
- Note if a matching Mycena PR is required
- Contributions are licensed under AGPL-3.0-or-later

## Deploy changes

Document Fly.io impact in the PR. Do not commit secrets — use Fly secrets / GitHub Actions secrets.
