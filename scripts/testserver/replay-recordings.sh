#!/usr/bin/env bash
# Replays the session recordings of Vanillite 30001 on vanillite-test (#152) that no earlier run
# replayed, against Vanilla (step session-replay of nightly.sh, #257).
# Usage: replay-recordings.sh <state dir> <out dir>
# Copies them from the ssh host vanillite-recordings (read-only, scripts/testserver/
# provision-test2.sh) into <state dir>/new, the oldest first and at most MAX_RECORDINGS, leaves
# out a session that is still running, runs `cargo xtask parity replay` on them (the first
# MAX_TICKS ticks each) and copies target/parity-fuzz (summary.md: one line per recording and
# the distinct divergences, shrunk scenarios in found/) to <out dir>/session-replay. The
# replayed names go to <state dir>/replayed; the copies are deleted.
set -uo pipefail
state=${1:?usage: replay-recordings.sh <state dir> <out dir>}
out=${2:?usage: replay-recordings.sh <state dir> <out dir>}
max_recordings=${MAX_RECORDINGS:-8}
max_ticks=${MAX_TICKS:-6000}
mkdir -p "$state"
touch "$state/replayed"
rm -rf "${state:?}/new"
mkdir -p "$state/new"

names=$(rsync --list-only vanillite-recordings:./ | awk '{print $NF}' \
    | grep -E '^[0-9]{4}-[0-9]{2}-[0-9]{2}_[0-9]{2}-[0-9]{2}-[0-9]{2}$' | sort) \
    || { echo "cannot list the recordings of vanillite-test" >&2; exit 1; }
dirs=()
for name in $names; do
    grep -qxF "$name" "$state/replayed" && continue
    [ "${#dirs[@]}" -ge "$max_recordings" ] && break
    rsync -a "vanillite-recordings:./$name" "$state/new/" || { echo "cannot copy $name" >&2; continue; }
    # A running session writes its packets at least every 10 s.
    if [ -n "$(find "$state/new/$name/packets.jsonl" -mmin -10)" ]; then
        echo "$name: still running, next time"
        rm -rf "${state:?}/new/$name"
        continue
    fi
    dirs+=("$state/new/$name")
done
if [ "${#dirs[@]}" = 0 ]; then
    mkdir -p "$out/session-replay"
    printf '# Session replay\n\nNo new recordings.\n' > "$out/session-replay/summary.md"
    exit 0
fi
echo "replaying ${#dirs[@]} recording(s), at most $max_ticks ticks each"
cargo xtask parity replay "${dirs[@]}" --ticks "$max_ticks"
status=$?
rm -rf "$out/session-replay"
if [ -d target/parity-fuzz ]; then
    cp -r target/parity-fuzz "$out/session-replay"
    rm -rf "$out/session-replay/work"
fi
# Marked even when the replay failed: the summary says so, and a recording that breaks the
# replay must not block the next ones.
for dir in "${dirs[@]}"; do basename "$dir" >> "$state/replayed"; done
rm -rf "${state:?}/new"
exit "$status"
