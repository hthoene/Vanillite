#!/usr/bin/env bash
# Packs a release archive: the server binary, LICENSE, NOTICE and README.txt in a folder named
# like the archive. Windows gets a .zip, the others a .tar.gz.
#
#   scripts/release/package.sh <binary> <os> <arch> <out dir>
#
# The archive is <out dir>/vanillite-<version>-mc<minecraft>-<os>-<arch>.<zip|tar.gz>.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
binary="$1" os="$2" arch="$3" out="$4"

eval "$("$root/scripts/release/meta.sh")"
stem="${name}-${os}-${arch}"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$stem" "$out"
out="$(cd "$out" && pwd)"

exe=vanillite
[[ "$os" == windows ]] && exe=vanillite.exe
cp "$binary" "$stage/$stem/$exe"
chmod 755 "$stage/$stem/$exe"
cp "$root/LICENSE" "$root/NOTICE" "$stage/$stem/"
start='./vanillite'
[[ "$os" == windows ]] && start='vanillite.exe'
sed -e "s/@VERSION@/$version/g" -e "s/@MC@/$mc/g" -e "s|@START@|$start|g" \
    "$root/scripts/release/README.txt" > "$stage/$stem/README.txt"
# Keep only the notes for this operating system.
for other in Windows macOS; do
    if [[ "$(echo "$other" | tr '[:upper:]' '[:lower:]')" != "$os" ]]; then
        sed -i.bak "/^$other:\$/,/^\$/d" "$stage/$stem/README.txt" && rm "$stage/$stem/README.txt.bak"
    fi
done
if [[ "$os" == windows ]]; then
    # Windows text files: CRLF line ends.
    for file in README.txt LICENSE NOTICE; do
        sed -i.bak 's/$/\r/' "$stage/$stem/$file" && rm "$stage/$stem/$file.bak"
    done
    archive="$out/$stem.zip"
    rm -f "$archive"
    (cd "$stage" && 7z a -tzip -bso0 "$archive" "$stem")
else
    archive="$out/$stem.tar.gz"
    owner=()
    tar --version 2>/dev/null | grep -q GNU && owner=(--owner=0 --group=0 --numeric-owner)
    tar -C "$stage" ${owner[@]+"${owner[@]}"} -czf "$archive" "$stem"
fi
echo "$archive"
