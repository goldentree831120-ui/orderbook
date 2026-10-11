#!/usr/bin/env bash
# One command for reproducible benchmarks: prints the machine, then runs criterion.
#   ./scripts/bench.sh                    run all benchmarks
#   ./scripts/bench.sh --save-baseline X  save results as baseline X
#   ./scripts/bench.sh --baseline X       compare against baseline X
# Report only numbers from your own machine, with the header this prints.
set -euo pipefail
cd "$(dirname "$0")/.."
echo "== machine"
if command -v lscpu >/dev/null; then lscpu | grep -E "Model name|^CPU\(s\)|MHz" | sed 's/  */ /g'
elif command -v sysctl >/dev/null; then sysctl -n machdep.cpu.brand_string; sysctl -n hw.ncpu
else echo "(on Windows: run 'wmic cpu get name' and note it)"; fi
echo "rustc: $(rustc --version)"; echo "commit: $(git rev-parse --short HEAD 2>/dev/null || echo n/a)"
echo "date: $(date -u +%Y-%m-%dT%H:%MZ)"; echo
cargo bench --bench engine -- "$@"
