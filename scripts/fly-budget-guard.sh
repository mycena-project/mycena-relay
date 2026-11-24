#!/usr/bin/env bash
# Fly.io budget guard for the Mycena relay.
#
# Fly.io has no hard spending cap. This script keeps the relay inside its
# intended footprint (about 4-5 USD/month): exactly one shared-cpu-1x/256 MB
# machine, at most one 1 GB volume and at most one dedicated IPv4.
#
# Default: report and exit 1 when the footprint is exceeded.
# --enforce: additionally scale extra machines down to one.
#
# Usage: bash scripts/fly-budget-guard.sh [--enforce]
# Needs: flyctl, jq, and `fly auth login` or FLY_API_TOKEN.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FLY_BIN="$(command -v flyctl 2>/dev/null || command -v fly)"
APP="$(grep -m1 '^app = ' "$ROOT/deploy/relay/fly.toml" | sed 's/^app = "\(.*\)".*/\1/')"

MAX_MACHINES=1
MAX_VOLUMES=1
MAX_VOLUME_GB=1
MAX_IPV4=1
ALLOWED_CPU_KIND="shared"
MAX_CPUS=1
MAX_MEMORY_MB=256

fail=0
note() { echo "fly-budget-guard: $*"; }
over() { echo "::error::fly-budget-guard: $*"; fail=1; }

machines="$("$FLY_BIN" machines list -a "$APP" --json)"
n_machines="$(echo "$machines" | jq 'length')"
note "app=$APP machines=$n_machines"
[[ "$n_machines" -le "$MAX_MACHINES" ]] || over "$n_machines machines (max $MAX_MACHINES)"

while read -r id kind cpus mem; do
  [[ -z "$id" ]] && continue
  note "machine $id: $kind cpus=$cpus memory=${mem}MB"
  [[ "$kind" == "$ALLOWED_CPU_KIND" ]] || over "machine $id uses cpu_kind=$kind"
  [[ "$cpus" -le "$MAX_CPUS" ]] || over "machine $id has $cpus CPUs"
  [[ "$mem" -le "$MAX_MEMORY_MB" ]] || over "machine $id has ${mem}MB memory"
done < <(echo "$machines" | jq -r '.[] | "\(.id) \(.config.guest.cpu_kind) \(.config.guest.cpus) \(.config.guest.memory_mb)"')

volumes="$("$FLY_BIN" volumes list -a "$APP" --json)"
n_volumes="$(echo "$volumes" | jq 'length')"
max_gb="$(echo "$volumes" | jq '[.[].size_gb] | max // 0')"
note "volumes=$n_volumes largest=${max_gb}GB"
[[ "$n_volumes" -le "$MAX_VOLUMES" ]] || over "$n_volumes volumes (max $MAX_VOLUMES)"
[[ "$max_gb" -le "$MAX_VOLUME_GB" ]] || over "volume of ${max_gb}GB (max ${MAX_VOLUME_GB}GB)"

ips="$("$FLY_BIN" ips list -a "$APP" --json)"
n_v4="$(echo "$ips" | jq '[.[] | select(.Type == "v4")] | length')"
note "dedicated IPv4=$n_v4"
[[ "$n_v4" -le "$MAX_IPV4" ]] || over "$n_v4 dedicated IPv4 addresses (max $MAX_IPV4, 2 USD/month each)"

if [[ "$fail" -ne 0 && "${1:-}" == "--enforce" && "$n_machines" -gt "$MAX_MACHINES" ]]; then
  note "enforcing: scaling to $MAX_MACHINES machine"
  "$FLY_BIN" scale count "$MAX_MACHINES" -a "$APP" -y
fi

if [[ "$fail" -ne 0 ]]; then
  echo "Footprint exceeded. Check https://fly.io/dashboard and the Fly.io billing page." >&2
  exit 1
fi
note "within budget footprint"
