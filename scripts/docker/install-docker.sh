#!/usr/bin/env bash
# Installs Docker from Docker's apt repository on Debian or Ubuntu (idempotent).
#
#   install-docker.sh            Engine, CLI and buildx; starts the daemon (systemd hosts).
#   install-docker.sh --client   CLI and buildx only, for machines that cannot run containers
#                                (e.g. unprivileged dev containers); they build on a remote
#                                engine through DOCKER_HOST=ssh://<host>.
set -euo pipefail

packages=(docker-ce-cli docker-buildx-plugin)
[ "${1:-}" = "--client" ] || packages+=(docker-ce containerd.io)

missing=()
for pkg in "${packages[@]}"; do
    dpkg -s "$pkg" > /dev/null 2>&1 || missing+=("$pkg")
done
if [ ${#missing[@]} -gt 0 ]; then
    . /etc/os-release
    export DEBIAN_FRONTEND=noninteractive
    sudo apt-get update -q
    sudo apt-get install -y -q ca-certificates curl gnupg
    sudo install -m 0755 -d /etc/apt/keyrings
    curl -fsSL "https://download.docker.com/linux/${ID}/gpg" \
        | sudo gpg --dearmor --yes -o /etc/apt/keyrings/docker.gpg
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/${ID} ${VERSION_CODENAME} stable" \
        | sudo tee /etc/apt/sources.list.d/docker.list > /dev/null
    sudo apt-get update -q
    sudo apt-get install -y -q "${missing[@]}"
fi

if [ "${1:-}" != "--client" ]; then
    sudo systemctl enable --now docker
    sudo docker info --format 'Docker {{.ServerVersion}} ({{.Driver}})'
fi
docker --version
