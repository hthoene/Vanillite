#!/usr/bin/env bash
# Nightly heavy tests on vanillite-test2 (#179). vanillite-nightly (timer, 00:30 Europe/Berlin)
# runs this file of the newest green dev commit in /opt/vanillite-work/src as user
# vanillite-work while it holds the server's lock: `nightly.sh <out dir> <commit>`.
# Every step gets a time limit; its output goes to <out dir>/<name>.log and one line per step
# to <out dir>/summary.md. All steps together must end before 03:00 Europe/Berlin, when the
# benchmark suite starts on vanillite-test (same Proxmox host). Add new heavy nightly tests
# (soak, session replay, ...) as steps here.
set -uo pipefail
out=${1:?usage: nightly.sh <out dir> <commit>}
commit=${2:-unknown}
failed=0

{
    echo "Nightly heavy tests of dev ${commit:0:12}, started $(date -u '+%Y-%m-%d %H:%M UTC')"
    echo
    echo "| step | result | minutes |"
    echo "|---|---|---|"
} > "$out/summary.md"

# step <name> <time limit in minutes> <command...>
step() {
    local name=$1 limit=$2 started=$SECONDS status result
    shift 2
    timeout --signal=INT --kill-after=120 "${limit}m" "$@" > "$out/$name.log" 2>&1
    status=$?
    case $status in
        0) result=ok ;;
        124) result="time limit (${limit} min)" ;;
        *) result="FAILED ($status)" ;;
    esac
    [ "$status" = 0 ] || failed=1
    echo "| $name | $result | $(((SECONDS - started) / 60)) |" >> "$out/summary.md"
}

rustup toolchain install --profile minimal > /dev/null 2>&1
step build 30 cargo build --locked -q -p xtask -p vanillite-test --bins

# Random parity scenarios (#132): a new seed range every night.
seed=$(($(date -u +%s) / 86400 * 1000))
step parity-fuzz 80 cargo xtask parity fuzz --seed "$seed" --minutes 70
[ -d target/parity-fuzz ] && cp -r target/parity-fuzz "$out/parity-fuzz"

# Bulk parity scenarios: records missing or stale bulk goldens and runs the ignored tests.
step golden-bulk 35 cargo xtask golden regen --bulk

if [ -f "$out/parity-fuzz/summary.md" ]; then
    { echo; echo "## parity fuzz (seeds from $seed)"; cat "$out/parity-fuzz/summary.md"; } >> "$out/summary.md"
fi
echo >> "$out/summary.md"
echo "Finished $(date -u '+%Y-%m-%d %H:%M UTC')" >> "$out/summary.md"
exit "$failed"
