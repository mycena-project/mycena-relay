# Security Policy

## Reporting a Vulnerability

**Do not** open public GitHub issues for security vulnerabilities.

Report them privately to **github.com/mycena-project/Mycena/security/advisories/new** with:

- affected component (relay binary, deploy config, rendezvous API) and version or commit hash
- clear reproduction steps or proof-of-concept
- impact assessment

We aim to acknowledge receipt within **72 hours** and coordinate responsible disclosure.

Preferred channels:

1. Email: **github.com/mycena-project/Mycena/security/advisories/new**
2. GitHub Security Advisories on this repository

## Scope

In scope: `mycena-relay` binary, Fly.io deployment surface, rendezvous HTTP endpoints, libp2p listener configuration.

The main Mycena app security model is documented in [mycena-project/Mycena](https://github.com/mycena-project/Mycena).

## AGPL

This relay is AGPL-3.0-or-later. Operators running modified versions on a public network must provide corresponding source to users per the license.
