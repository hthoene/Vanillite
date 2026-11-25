#!/usr/bin/env bash
# Release metadata from the workspace version and the generated Minecraft version, as
# key=value lines (append them to $GITHUB_OUTPUT in workflows).
#
#   scripts/release/meta.sh [<git tag>]
#
# With a tag it fails unless the tag is exactly v<version>+mc<minecraft version>.
# Outputs: version, mc, tag, name (archive prefix), prerelease (true/false) and docker_tags
# (comma-separated): 0.1.0-beta.1-mc26.3, mc26.3-beta, beta for a pre-release and
# 0.1.0-mc26.3, mc26.3, latest for a stable version. A pre-release never moves latest or mc<id>.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"

version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)"
mc="$(sed -n 's/^pub const VERSION_ID: &str = "\(.*\)";$/\1/p' \
    "$root/crates/vanillite-data/src/generated/version.rs")"
[[ -n "$version" && -n "$mc" ]] || { echo "cannot read the version or the Minecraft version" >&2; exit 1; }
tag="v${version}+mc${mc}"

if [[ $# -gt 0 && "$1" != "$tag" ]]; then
    echo "tag $1 does not match the sources: expected $tag (Cargo.toml version $version," \
        "Minecraft $mc)" >&2
    exit 1
fi

if [[ "$version" == *-* ]]; then
    prerelease=true
    # The channel is the first identifier of the pre-release part: beta of 0.1.0-beta.1.
    channel="${version#*-}"
    channel="${channel%%.*}"
    docker_tags="${version}-mc${mc},mc${mc}-${channel},${channel}"
else
    prerelease=false
    docker_tags="${version}-mc${mc},mc${mc},latest"
fi

echo "version=$version"
echo "mc=$mc"
echo "tag=$tag"
echo "name=vanillite-${version}-mc${mc}"
echo "prerelease=$prerelease"
echo "docker_tags=$docker_tags"
