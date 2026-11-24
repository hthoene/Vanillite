#!/usr/bin/env bash
# Provisions the Vanillite test server (Ubuntu). Idempotent: safe to run again at any time.
# Run with `cargo xtask testserver provision` (pipes this script to `ssh <host> bash -s`).
#
# - Paper reference server on port 30000 (/opt/paper, paper.service, user minecraft).
# - Vanillite test servers on ports 30001-30999 (/opt/vanillite/<port>, vanillite@<port>.service,
#   user vanillite), deployed with `cargo xtask deploy` through /usr/local/bin/vanillite-deploy.
# Only ports 30000-30999 are reachable by players.
set -euo pipefail

PAPER_VERSION=26.3
PAPER_BUILD=262
PAPER_SHA256=b2d24a0abe99f481b35504b119e35ac721013f59d9c670aa9a4d05814a81617b
PAPER_URL="https://fill-data.papermc.io/v1/objects/${PAPER_SHA256}/paper-${PAPER_VERSION}-${PAPER_BUILD}.jar"

# Writes stdin to $1 only when the content changed; prints the path when it did.
write_if_changed() {
    local target=$1 tmp
    tmp=$(mktemp)
    cat > "$tmp"
    if [ -f "$target" ] && cmp -s "$tmp" "$target"; then
        rm -f "$tmp"
    else
        install -m "${2:-644}" "$tmp" "$target"
        rm -f "$tmp"
        echo "updated $target"
    fi
}

echo "== packages"
export DEBIAN_FRONTEND=noninteractive
missing=()
for pkg in openjdk-25-jre-headless curl rsync; do
    dpkg -s "$pkg" > /dev/null 2>&1 || missing+=("$pkg")
done
if [ ${#missing[@]} -gt 0 ]; then
    apt-get update -q
    apt-get install -y -q "${missing[@]}"
fi

echo "== users"
id minecraft > /dev/null 2>&1 || useradd --system --home-dir /opt/paper --create-home --shell /usr/sbin/nologin minecraft
id vanillite > /dev/null 2>&1 || useradd --system --home-dir /opt/vanillite --create-home --shell /usr/sbin/nologin vanillite
install -d -o vanillite -g vanillite /opt/vanillite

echo "== paper (reference server, port 30000)"
install -d -o minecraft -g minecraft /opt/paper
jar="/opt/paper/paper-${PAPER_VERSION}-${PAPER_BUILD}.jar"
if [ ! -f "$jar" ]; then
    curl -fsSL -o "$jar.part" "$PAPER_URL"
    echo "${PAPER_SHA256}  $jar.part" | sha256sum -c -
    mv "$jar.part" "$jar"
    chown minecraft:minecraft "$jar"
fi
ln -sfn "$(basename "$jar")" /opt/paper/paper.jar
chown -h minecraft:minecraft /opt/paper/paper.jar
if [ ! -f /opt/paper/server.properties ]; then
    # Fresh install: the owner accepted the Minecraft EULA for this test machine.
    echo "eula=true" > /opt/paper/eula.txt
    printf 'server-port=30000\nonline-mode=true\nwhite-list=true\n' > /opt/paper/server.properties
    cat > /opt/paper/whitelist.json <<'EOF'
[
  {
    "uuid": "d97211d5-1155-4faa-95db-7ddec775418c",
    "name": "de_hannes"
  }
]
EOF
    chown minecraft:minecraft /opt/paper/eula.txt /opt/paper/server.properties /opt/paper/whitelist.json
fi
write_if_changed /etc/systemd/system/paper.service <<'EOF'
[Unit]
Description=Paper Minecraft server (test)
After=network-online.target
[Service]
User=minecraft
WorkingDirectory=/opt/paper
ExecStart=/usr/bin/java -Xms1G -Xmx4G -jar paper.jar --nogui
Restart=on-failure
[Install]
WantedBy=multi-user.target
EOF

echo "== vanillite (test servers, ports 30001-30999)"
write_if_changed /etc/systemd/system/vanillite@.service <<'EOF'
[Unit]
Description=Vanillite test server on port %i
After=network-online.target
Wants=network-online.target

[Service]
User=vanillite
Group=vanillite
WorkingDirectory=/opt/vanillite/%i
ExecStart=/opt/vanillite/%i/vanillite
Environment=RUST_BACKTRACE=1
Restart=on-failure
RestartSec=5
KillSignal=SIGTERM
TimeoutStopSec=60
LimitNOFILE=65536
MemoryMax=3G
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/opt/vanillite/%i

[Install]
WantedBy=multi-user.target
EOF

write_if_changed /usr/local/bin/vanillite-deploy 755 <<'EOF'
#!/usr/bin/env bash
# Usage: vanillite-deploy <port> < vanillite-binary
# Installs the binary from stdin as /opt/vanillite/<port>/vanillite, writes the Vanilla keys
# this test setup needs into server.properties, copies Paper's whitelist and (re)starts
# vanillite@<port>.service. Installed by scripts/testserver/provision.sh.
set -euo pipefail
port=${1:?usage: vanillite-deploy <port> < binary}
if ! [[ "$port" =~ ^[0-9]+$ ]] || [ "$port" -lt 30001 ] || [ "$port" -gt 30999 ]; then
    echo "port must be 30001-30999 (30000 is Paper)" >&2
    exit 2
fi
dir=/opt/vanillite/$port
install -d -o vanillite -g vanillite "$dir"
tmp=$(mktemp -p "$dir" .vanillite.XXXXXX)
cat > "$tmp"
[ -s "$tmp" ] || { rm -f "$tmp"; echo "no binary on stdin" >&2; exit 2; }
chmod 755 "$tmp"
chown vanillite:vanillite "$tmp"
systemctl stop "vanillite@$port" 2> /dev/null || true
mv -f "$tmp" "$dir/vanillite"

props=$dir/server.properties
touch "$props"
set_prop() {
    if grep -q "^$1=" "$props"; then
        sed -i "s|^$1=.*|$1=$2|" "$props"
    else
        echo "$1=$2" >> "$props"
    fi
}
set_prop server-port "$port"
set_prop online-mode true
set_prop white-list true
# The Minecraft EULA is accepted for this test server.
echo "eula=true" > "$dir/eula.txt"
if [ -f /opt/paper/whitelist.json ]; then
    cp /opt/paper/whitelist.json "$dir/whitelist.json"
fi
chown vanillite:vanillite "$props" "$dir/eula.txt" "$dir"/*.json 2> /dev/null || true

systemctl enable --quiet "vanillite@$port"
systemctl restart "vanillite@$port"
sleep 1
systemctl --no-pager --lines=10 status "vanillite@$port" || true
EOF

systemctl daemon-reload
systemctl enable --quiet paper.service
systemctl is-active --quiet paper.service || systemctl start paper.service
echo "== done"
