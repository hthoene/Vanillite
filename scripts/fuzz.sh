#!/usr/bin/env bash
# Builds every fuzz target once and runs them in parallel, each for a time budget.
# Usage: scripts/fuzz.sh [seconds per target, default 300] [parallel jobs, default nproc]
# Needs a nightly toolchain and cargo-fuzz. The growing corpus lives in fuzz/corpus/<target>
# (not committed); the committed seeds in fuzz/seeds/<target> are read as a second corpus.
# Crashes are written to fuzz/artifacts/<target>.
set -euo pipefail
cd "$(dirname "$0")/.."

seconds="${1:-300}"
jobs="${2:-$(nproc)}"

cargo +nightly fuzz build -O
targets=$(cargo +nightly fuzz list)
for target in $targets; do
  mkdir -p "fuzz/corpus/$target" "fuzz/seeds/$target"
done

run() {
  local target="$1" seconds="$2"
  if cargo +nightly fuzz run -O "$target" "fuzz/corpus/$target" "fuzz/seeds/$target" -- \
    -max_total_time="$seconds" -print_final_stats=1 >"fuzz/$target.log" 2>&1; then
    echo "ok      $target ($(grep -m1 -o 'stat::number_of_executed_units: [0-9]*' "fuzz/$target.log" | cut -d' ' -f2) runs)"
  else
    echo "FAILED  $target (log: fuzz/$target.log)"
    tail -n 40 "fuzz/$target.log"
    return 1
  fi
}
export -f run

printf '%s\n' $targets | xargs -P "$jobs" -I{} bash -c 'run "$1" "$2"' _ {} "$seconds"
