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

# Minutes left until 02:50 Europe/Berlin; a run started by hand at another time gets 140.
minutes_left() {
    local now end
    now=$(date +%s)
    end=$(TZ=Europe/Berlin date -d '02:50' +%s)
    if [ "$end" -le "$now" ] || [ $((end - now)) -gt 10800 ]; then
        end=$((started_at + 140 * 60))
    fi
    echo $(((end - now) / 60))
}
started_at=$(date +%s)

rustup toolchain install --profile minimal > /dev/null 2>&1
step build 30 bash -c 'cargo build --locked -q -p xtask -p vanillite-test --bins &&
    cargo build --locked -q --profile release-fast -p vanillite-test --bin parity_soak'

# Bulk parity scenarios: records missing or stale bulk goldens and runs the ignored tests.
step golden-bulk 35 cargo xtask golden regen --bulk

# Soak parity runs (#166): game days against Vanilla, per-tick checksums; Vanilla's run is
# recorded once per golden key, so only a new jar, harness or scenario costs its recording.
soak=$(($(minutes_left) / 2))
[ "$soak" -gt 45 ] && soak=45
step parity-soak $((soak + 10)) cargo xtask parity soak --minutes "$soak"
[ -d target/parity-soak ] && cp -r target/parity-soak "$out/parity-soak"

# Random parity scenarios (#132): a new seed range every night, for the time that is left.
seed=$(($(date -u +%s) / 86400 * 1000))
fuzz=$(($(minutes_left) - 10))
if [ "$fuzz" -ge 5 ]; then
    step parity-fuzz $((fuzz + 8)) cargo xtask parity fuzz --seed "$seed" --minutes "$fuzz"
    [ -d target/parity-fuzz ] && cp -r target/parity-fuzz "$out/parity-fuzz"
else
    echo "| parity-fuzz | skipped (no time left) | 0 |" >> "$out/summary.md"
fi

if [ -f "$out/parity-soak/summary.md" ]; then
    { echo; echo "## parity soak"; cat "$out/parity-soak/summary.md"; } >> "$out/summary.md"
fi
if [ -f "$out/parity-fuzz/summary.md" ]; then
    { echo; echo "## parity fuzz (seeds from $seed)"; cat "$out/parity-fuzz/summary.md"; } >> "$out/summary.md"
fi
echo >> "$out/summary.md"
echo "Finished $(date -u '+%Y-%m-%d %H:%M UTC')" >> "$out/summary.md"
exit "$failed"
