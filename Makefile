SHELL := /bin/bash

.PHONY: help run test clippy fmt ci

help:
	@echo "mycena-relay — the Mycena project bootstrap relay"
	@echo ""
	@echo "  make run      Local relay (see README)"
	@echo "  make test     Test the relay crates"
	@echo "  make ci       Same checks as GitHub CI"
	@echo ""
	@echo "Production: ./scripts/deploy-relay-fly.sh · docs/ECOSYSTEM.md"

RELAY_CRATES := -p mycena-relay -p mycena-core -p mycena-node

run:
	cargo run -p mycena-relay

test:
	cargo test $(RELAY_CRATES)

clippy:
	cargo clippy $(RELAY_CRATES) --all-targets -- -D warnings

fmt:
	cargo fmt --all

ci: test clippy
	cargo fmt --all -- --check
