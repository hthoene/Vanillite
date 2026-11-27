#!/usr/bin/env bash
# Provisions the Vanillite test server (Ubuntu). Idempotent: safe to run again at any time.
# Run with `cargo xtask testserver provision` (pipes this script to `ssh <host> bash -s`).
#
# - Paper reference server on port 30000 (/opt/paper, paper.service, user minecraft).
# - Vanillite test servers on ports 30001-30999 (/opt/vanillite/<port>, vanillite@<port>.service,
#   user vanillite), deployed with `cargo xtask deploy` through /usr/local/bin/vanillite-deploy.
# - Benchmark instances (`cargo xtask loadtest` and `cargo xtask bench`), running only during a
#   run, all offline mode with the settings of `vanillite-bench props`: Vanillite on 30901 and,
#   taking turns on 30900, Paper (/opt/paper-loadtest), Vanilla (/opt/vanilla-loadtest),
#   Pumpkin (/opt/pumpkin-loadtest) and the proxies Velocity (/opt/velocity-loadtest) and
#   vanillite-proxy (/opt/vanillite/bench/proxy) in front of 30901. Controlled with
#   /usr/local/bin/vanillite-bench.
# - Docker engine for `cargo xtask docker` (image builds over SSH).
# - vanillite-proxy on 30010 (/opt/vanillite-proxy, vanillite-proxy.service, deployed with
#   `ssh <host> vanillite-proxy-deploy < target/release-fast/vanillite-proxy`) in front of
#   Vanillite 30001 and 30002 and a Paper with modern forwarding on 30011 (/opt/paper-proxy).
# - Bedrock players on UDP 30019: the Bedrock frontend of Vanillite 30001 (`[bedrock]` set by the
#   drop-in vanillite@30001.service.d/bedrock.conf), and on UDP 30020 the Bedrock frontend of
#   the proxy (`[bedrock]` in /opt/vanillite-proxy/proxy.toml).
# - vanillite-autodeploy (timer, every 30 minutes): builds the newest green dev commit in
#   /opt/vanillite-build and deploys it to 30001, 30002, 30030 and the proxy on 30010, with a chat
#   announcement and rollback; log in /var/log/vanillite-autodeploy.log. Manual deploys of a
#   branch go to 30003-30099.
# - Vanillite 30030: chunk generation test server (own world, flight allowed, all server-side
#   movement checks off so fast flying is never teleported back; drop-in chunk-test.conf).
# - vanillite-incidents (service): saves every crash and suspicious disconnect of the Vanillite
#   instances and the proxy as a bundle in /var/lib/vanillite-incidents (`vanillite-incidents
#   list|show`, `cargo xtask testserver incidents`).
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
for pkg in openjdk-25-jre-headless curl rsync jq python3 git build-essential; do
    dpkg -s "$pkg" > /dev/null 2>&1 || missing+=("$pkg")
done
if [ ${#missing[@]} -gt 0 ]; then
    apt-get update -q
    apt-get install -y -q "${missing[@]}"
fi

# Docker engine with buildx: `cargo xtask docker` builds the image here over SSH.
echo "== docker"
if ! command -v docker > /dev/null; then
    . /etc/os-release
    install -m 0755 -d /etc/apt/keyrings
    curl -fsSL "https://download.docker.com/linux/${ID}/gpg" | gpg --dearmor --yes -o /etc/apt/keyrings/docker.gpg
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/${ID} ${VERSION_CODENAME} stable" \
        > /etc/apt/sources.list.d/docker.list
    apt-get update -q
    apt-get install -y -q docker-ce docker-ce-cli containerd.io docker-buildx-plugin
fi
systemctl enable --now docker > /dev/null

echo "== users"
id minecraft > /dev/null 2>&1 || useradd --system --home-dir /opt/paper --create-home --shell /usr/sbin/nologin minecraft
id vanillite > /dev/null 2>&1 || useradd --system --home-dir /opt/vanillite --create-home --shell /usr/sbin/nologin vanillite
install -d -o vanillite -g vanillite /opt/vanillite

# Players who are whitelisted and OP level 4 on every test server. Paper's whitelist.json and
# ops.json are the source; vanillite-deploy copies both into every Vanillite instance.
# "creative": always in creative mode on every test server (see testserver-creative below).
players='[
  {"uuid": "d97211d5-1155-4faa-95db-7ddec775418c", "name": "de_hannes"},
  {"uuid": "997ddb16-a31a-43b1-8f10-dac13d1f6af6", "name": "Looxtastisch", "creative": true},
  {"uuid": "759689da-4bf5-4370-8f27-08382b24bb63", "name": "Tiegertropfen119", "creative": true}
]'
install -d /etc/vanillite-test
echo "$players" | jq '[.[] | select(.creative) | {uuid, name}]' \
    | write_if_changed /etc/vanillite-test/creative-players.json

echo "== rcon (paper-rcon [--port <port>] <command>, password in /root/.paper-rcon)"
if [ ! -s /root/.paper-rcon ]; then
    (umask 077 && head -c 24 /dev/urandom | base64 | tr -d '/+=' > /root/.paper-rcon)
fi
rcon_password=$(cat /root/.paper-rcon)
write_if_changed /usr/local/bin/paper-rcon 755 <<'EOF'
#!/usr/bin/env python3
"""Sends one console command to a local Paper server via RCON.

Usage: paper-rcon [--port <port>] <command...>  (25575: reference server, 25576: load test)
Installed by scripts/testserver/provision.sh."""
import socket, struct, sys
args = sys.argv[1:]
port = 25575
if args[:1] == ["--port"]:
    port, args = int(args[1]), args[2:]
pw = open("/root/.paper-rcon").read().strip()
def pkt(i, t, body):
    b = body.encode() + b"\x00\x00"
    return struct.pack("<iii", len(b) + 8, i, t) + b
def recv(s):
    n = struct.unpack("<i", s.recv(4))[0]
    data = b""
    while len(data) < n:
        data += s.recv(n - len(data))
    i, t = struct.unpack("<ii", data[:8])
    return i, data[8:-2].decode(errors="replace")
s = socket.create_connection(("127.0.0.1", port), timeout=5)
s.sendall(pkt(1, 3, pw))
if recv(s)[0] == -1:
    sys.exit("RCON auth failed")
s.sendall(pkt(2, 2, " ".join(args)))
print(recv(s)[1])
EOF

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
    printf 'enable-rcon=true\nrcon.port=25575\nrcon.password=%s\n' "$rcon_password" \
        >> /opt/paper/server.properties
    echo "$players" | jq '[.[] | {uuid, name}]' > /opt/paper/whitelist.json
    echo "$players" | jq '[.[] | {uuid, name, level: 4, bypassesPlayerLimit: false}]' \
        > /opt/paper/ops.json
    chown minecraft:minecraft /opt/paper/eula.txt /opt/paper/server.properties /opt/paper/*.json
fi
write_if_changed /etc/systemd/system/paper.service <<'EOF'
[Unit]
Description=Paper Minecraft server (test)
After=network-online.target
[Service]
User=minecraft
WorkingDirectory=/opt/paper
ExecStart=/usr/bin/java -Xms1G -Xmx4G -jar paper.jar --nogui
Restart=always
RestartSec=5
[Install]
WantedBy=multi-user.target
EOF

echo "== vanillite (test servers, ports 30001-30999)"
write_if_changed /etc/systemd/system/vanillite@.service <<'EOF'
[Unit]
Description=Vanillite test server on port %i
After=network-online.target vanillite-console@%i.socket
Wants=network-online.target
Requires=vanillite-console@%i.socket

[Service]
User=vanillite
Group=vanillite
WorkingDirectory=/opt/vanillite/%i
ExecStartPre=-/usr/local/bin/testserver-creative vanillite /opt/vanillite/%i
ExecStart=/opt/vanillite/%i/vanillite
# Console commands: `vanillite-console <port> <command>` writes into this FIFO.
Sockets=vanillite-console@%i.socket
StandardInput=socket
StandardOutput=journal
StandardError=journal
Environment=RUST_BACKTRACE=1
Restart=always
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

# The console of an instance (its stdin) is a FIFO that systemd keeps open, so it never ends.
write_if_changed /etc/systemd/system/vanillite-console@.socket <<'EOF'
[Unit]
Description=Console FIFO of Vanillite instance %i
[Socket]
ListenFIFO=/run/vanillite-console/%i
SocketMode=0600
RemoveOnStop=true
Service=vanillite@%i.service
EOF
write_if_changed /usr/local/bin/vanillite-console 755 <<'EOF'
#!/usr/bin/env bash
# Usage: vanillite-console <port> <command...>
# Runs a console command on a running Vanillite instance (e.g. `vanillite-console 30001 say hi`).
# Installed by scripts/testserver/provision.sh.
set -euo pipefail
port=${1:?usage: vanillite-console <port> <command...>}
shift
fifo=/run/vanillite-console/$port
# No FIFO: an instance started before it existed. Never create a plain file in its place: the
# socket could then not start, nor the instance with it.
if ! systemctl is-active --quiet "vanillite@$port" || [ ! -p "$fifo" ]; then
    echo "vanillite@$port is not running with a console" >&2
    exit 1
fi
printf '%s\n' "$*" > "$fifo"
EOF

echo "== creative players (/etc/vanillite-test/creative-players.json)"
write_if_changed /usr/local/bin/testserver-creative 755 <<'EOF'
#!/usr/bin/env python3
"""Keeps the players in /etc/vanillite-test/creative-players.json in creative mode.

Usage: testserver-creative paper [<rcon port>] (Paper timer: gamemode creative for online ones)
       testserver-creative vanillite <dir>    (Vanillite: player data, before the instance starts
                                              and whenever the server writes player data)
Installed by scripts/testserver/provision.sh."""
import gzip, json, os, re, struct, subprocess, sys

players = json.load(open("/etc/vanillite-test/creative-players.json"))

def paper(port):
    rcon = ["paper-rcon"] + (["--port", port] if port else [])
    online = subprocess.run(rcon + ["list"], capture_output=True, text=True).stdout
    names = {n.strip() for n in online.partition(":")[2].split(",") if n.strip()}
    for p in players:
        if p["name"] in names:
            subprocess.run(rcon + ["gamemode", "creative", p["name"]], capture_output=True)

# Minimal NBT reader and writer: tags as (type, value).
def read_tag(b, i, t):
    fixed = {1: ">b", 2: ">h", 3: ">i", 4: ">q", 5: ">f", 6: ">d"}
    if t in fixed:
        n = struct.calcsize(fixed[t])
        return struct.unpack_from(fixed[t], b, i)[0], i + n
    if t in (7, 11, 12):
        n = struct.unpack_from(">i", b, i)[0]
        f = {7: "b", 11: "i", 12: "q"}[t]
        size = struct.calcsize(f)
        return list(struct.unpack_from(">%d%s" % (n, f), b, i + 4)), i + 4 + n * size
    if t == 8:
        n = struct.unpack_from(">H", b, i)[0]
        return b[i + 2:i + 2 + n], i + 2 + n
    if t == 9:
        et, n = struct.unpack_from(">bi", b, i)
        i += 5
        items = []
        for _ in range(n):
            v, i = read_tag(b, i, et)
            items.append(v)
        return (et, items), i
    if t == 10:
        c = {}
        while b[i] != 0:
            ct = b[i]
            n = struct.unpack_from(">H", b, i + 1)[0]
            name = b[i + 3:i + 3 + n]
            v, i = read_tag(b, i + 3 + n, ct)
            c[name] = (ct, v)
        return c, i + 1
    raise ValueError("bad tag type %d" % t)

def write_tag(t, v):
    fixed = {1: ">b", 2: ">h", 3: ">i", 4: ">q", 5: ">f", 6: ">d"}
    if t in fixed:
        return struct.pack(fixed[t], v)
    if t in (7, 11, 12):
        return struct.pack(">i%d%s" % (len(v), {7: "b", 11: "i", 12: "q"}[t]), len(v), *v)
    if t == 8:
        return struct.pack(">H", len(v)) + v
    if t == 9:
        et, items = v
        return struct.pack(">bi", et, len(items)) + b"".join(write_tag(et, x) for x in items)
    if t == 10:
        out = b""
        for name, (ct, cv) in v.items():
            out += struct.pack(">bH", ct, len(name)) + name + write_tag(ct, cv)
        return out + b"\x00"
    raise ValueError("bad tag type %d" % t)

def read_file(path):
    b = gzip.decompress(open(path, "rb").read())
    n = struct.unpack_from(">H", b, 1)[0]
    return read_tag(b, 3 + n, 10)[0]

def write_file(path, root):
    tmp = path + ".creative-tmp"
    with open(tmp, "wb") as f:
        f.write(gzip.compress(b"\x0a\x00\x00" + write_tag(10, root)))
    os.replace(tmp, path)

def vanillite(dir):
    props = open(os.path.join(dir, "server.properties")).read() if os.path.exists(
        os.path.join(dir, "server.properties")) else ""
    m = re.search(r"^level-name=(.*)$", props, re.M)
    world = os.path.join(dir, m.group(1).strip() if m else "world")
    if not os.path.exists(os.path.join(world, "level.dat")):
        return
    data = os.path.join(world, "players", "data")
    os.makedirs(data, exist_ok=True)
    for p in players:
        path = os.path.join(data, p["uuid"] + ".dat")
        if os.path.exists(path):
            root = read_file(path)
            if root.get(b"playerGameType") == (3, 1):
                continue
        else:
            # Never joined: a new player at the world spawn, like the server would place it.
            spawn = read_file(os.path.join(world, "level.dat"))[b"Data"][1].get(b"spawn", (10, {}))[1]
            x, y, z = spawn.get(b"pos", (11, [0, 64, 0]))[1]
            yaw = spawn.get(b"yaw", (5, 0.0))[1]
            root = {
                b"Pos": (9, (6, [x + 0.5, float(y), z + 0.5])),
                b"Rotation": (9, (5, [yaw, 0.0])),
                b"Dimension": (8, b"minecraft:overworld"),
            }
        root[b"playerGameType"] = (3, 1)
        write_file(path, root)
        print("creative:", p["name"], path)

if sys.argv[1:2] == ["paper"] and len(sys.argv) <= 3:
    paper(sys.argv[2] if len(sys.argv) == 3 else None)
elif sys.argv[1:2] == ["vanillite"] and len(sys.argv) == 3:
    vanillite(sys.argv[2])
else:
    sys.exit(__doc__)
EOF
write_if_changed /etc/systemd/system/paper-creative.service <<'EOF'
[Unit]
Description=Keep the creative test players in creative mode on Paper (testserver-creative)
[Service]
Type=oneshot
ExecStart=/usr/local/bin/testserver-creative paper
ExecStart=-/usr/local/bin/testserver-creative paper 25577
EOF
write_if_changed /etc/systemd/system/paper-creative.timer <<'EOF'
[Unit]
Description=Run paper-creative.service every 15 seconds
[Timer]
OnBootSec=30
OnUnitActiveSec=15
AccuracySec=1
[Install]
WantedBy=timers.target
EOF
# Vanillite overwrites a player's data when they leave: put creative back right after each write.
write_if_changed /etc/systemd/system/vanillite-creative@.path <<'EOF'
[Unit]
Description=Watch the player data of Vanillite instance %i for creative test players
[Path]
PathChanged=/opt/vanillite/%i/world/players/data
[Install]
WantedBy=multi-user.target
EOF
write_if_changed /etc/systemd/system/vanillite-creative@.service <<'EOF'
[Unit]
Description=Keep the creative test players in creative mode on Vanillite instance %i
[Service]
Type=oneshot
User=vanillite
Group=vanillite
ExecStart=/usr/local/bin/testserver-creative vanillite /opt/vanillite/%i
EOF

write_if_changed /usr/local/bin/vanillite-deploy 755 <<'EOF'
#!/usr/bin/env bash
# Usage: vanillite-deploy <port> < vanillite-binary
# Installs the binary from stdin as /opt/vanillite/<port>/vanillite, writes the Vanilla keys
# this test setup needs into server.properties, copies Paper's whitelist and ops and (re)starts
# vanillite@<port>.service. Port 30901 is the load test instance: offline mode without
# whitelist, started but not enabled (`cargo xtask loadtest` stops it after the run).
# 30001, 30002 and 30030 belong to vanillite-autodeploy (newest green dev), which sets
# VANILLITE_DEPLOY_AUTO=1 and the MOTD in VANILLITE_DEPLOY_MOTD.
# Installed by scripts/testserver/provision.sh.
set -euo pipefail
port=${1:?usage: vanillite-deploy <port> < binary}
if ! [[ "$port" =~ ^[0-9]+$ ]] || [ "$port" -lt 30001 ] || [ "$port" -gt 30999 ]; then
    echo "port must be 30001-30999 (30000 is Paper)" >&2
    exit 2
fi
if { [ "$port" = 30001 ] || [ "$port" = 30002 ] || [ "$port" = 30030 ]; } \
    && [ "${VANILLITE_DEPLOY_AUTO:-}" != 1 ]; then
    echo "30001, 30002 and 30030 run the newest green dev (vanillite-autodeploy): land on dev instead," >&2
    echo "or test a branch on a port in 30003-30099 and stop it afterwards" >&2
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
if [ "$port" = 30901 ]; then
    # The shared benchmark settings (seed, offline mode, distances), like the references.
    vanillite-bench props vanillite
    rm -f "$dir/tick.log"
else
    set_prop online-mode true
    set_prop white-list true
fi
if [ -n "${VANILLITE_DEPLOY_MOTD:-}" ]; then
    set_prop motd "$VANILLITE_DEPLOY_MOTD"
fi
# The Minecraft EULA is accepted for this test server.
echo "eula=true" > "$dir/eula.txt"
for f in whitelist.json ops.json; do
    if [ -f "/opt/paper/$f" ]; then
        cp "/opt/paper/$f" "$dir/$f"
    fi
done
chown vanillite:vanillite "$props" "$dir/eula.txt" "$dir"/*.json 2> /dev/null || true

if [ "$port" = 30901 ]; then
    systemctl restart "vanillite@$port"
    exit 0
fi
systemctl enable --quiet "vanillite@$port" "vanillite-creative@$port.path"
systemctl start "vanillite-creative@$port.path"
systemctl restart "vanillite@$port"
sleep 1
systemctl --no-pager --lines=10 status "vanillite@$port" || true
EOF

echo "== benchmark instances (xtask loadtest and bench; running only during a run)"
# Vanillite on 30901; the references take turns on 30900: Paper (paper-loadtest), Vanilla
# (vanilla-loadtest), Pumpkin (pumpkin-loadtest), and as proxies in front of 30901 Velocity
# (velocity-loadtest) and vanillite-proxy (vanillite-proxy-loadtest). All share the settings of
# `vanillite-bench props` (same seed, world type, difficulty and distances).
VANILLA_VERSION=26.3
VANILLA_SHA1=33680f5f2ac32864d6d7cf5e56a705fdb3e05f4c
PUMPKIN_TAG="0.2.2+26.3-26.60"
PUMPKIN_SHA256=3755df414cdece664223388a93421c5c674682f5b9264387416b8a9074cd8d59
VELOCITY_JAR=velocity-4.3.1-45.jar
VELOCITY_SHA256=0844bcabdcdb4f5b875906fd4c3365bc1bb76918e42bc595ed88ad195258dd75
CHUNKY_JAR=Chunky-Paper-1.5.66.jar
CHUNKY_URL=https://cdn.modrinth.com/data/fALzjamp/versions/RBuKjN4k/Chunky-Paper-1.5.66.jar
CHUNKY_SHA512=c67b14569b8624d8d4ca550f0a064703e144d388073b4c34a913c3bd34ea39351bef504542101d7f85b499b4de95c24d027a62ee64aad3f9a7051ae4bec80c52

# Downloads $2 to $1 unless present and checks it with `<tool>sum -c` ($3: tool, $4: hash).
fetch() {
    local target=$1 url=$2 tool=$3 hash=$4
    [ -f "$target" ] && return 0
    curl -fsSL -o "$target.part" "$url"
    echo "$hash  $target.part" | "${tool}sum" -c -
    mv "$target.part" "$target"
}

install -d /etc/systemd/system/vanillite@30901.service.d
write_if_changed /etc/systemd/system/vanillite@30901.service.d/loadtest.conf <<'EOF'
[Service]
# Tick durations for `cargo xtask loadtest`; the same memory limit as the Paper instance's heap.
Environment=VANILLITE_TICK_LOG=tick.log
# Accepts the benchmark proxies (modern forwarding) and still direct connections.
EnvironmentFile=/etc/vanillite-test/proxy-backend.env
MemoryMax=6G
Restart=no
EOF
install -d -o vanillite -g vanillite /opt/vanillite/bench /opt/vanillite/bench/bin /opt/vanillite/bench/results

install -d -o minecraft -g minecraft /opt/paper-loadtest /opt/paper-loadtest/plugins
ln -sfn "$jar" /opt/paper-loadtest/paper.jar
# All bots connect from one address: no per-address connection throttle.
if [ -f /opt/paper-loadtest/bukkit.yml ]; then
    sed -i 's/^\(\s*connection-throttle:\).*/\1 -1/' /opt/paper-loadtest/bukkit.yml
else
    printf 'settings:\n  connection-throttle: -1\n' > /opt/paper-loadtest/bukkit.yml
fi
# The load test world was flat before the suite (#71): start over with the shared seed.
if grep -q '^level-type=minecraft\\\?:flat' /opt/paper-loadtest/server.properties 2> /dev/null; then
    rm -rf /opt/paper-loadtest/world*
fi
fetch "/opt/vanillite/bench/$CHUNKY_JAR" "$CHUNKY_URL" sha512 "$CHUNKY_SHA512"
write_if_changed /etc/systemd/system/paper-loadtest.service <<'EOF'
[Unit]
Description=Paper benchmark reference (offline mode, port 30900, only during xtask loadtest/bench)
After=network-online.target
[Service]
User=minecraft
WorkingDirectory=/opt/paper-loadtest
ExecStart=/usr/bin/java -Xms1G -Xmx6G -jar paper.jar --nogui
LimitNOFILE=65536
SuccessExitStatus=143
EOF

install -d -o minecraft -g minecraft /opt/vanilla-loadtest
fetch "/opt/vanilla-loadtest/server-$VANILLA_VERSION.jar" \
    "https://piston-data.mojang.com/v1/objects/$VANILLA_SHA1/server.jar" sha1 "$VANILLA_SHA1"
ln -sfn "server-$VANILLA_VERSION.jar" /opt/vanilla-loadtest/server.jar
write_if_changed /etc/systemd/system/vanilla-loadtest.service <<'EOF'
[Unit]
Description=Vanilla benchmark reference (offline mode, port 30900, only during xtask bench)
After=network-online.target
[Service]
User=minecraft
WorkingDirectory=/opt/vanilla-loadtest
ExecStart=/usr/bin/java -Xms1G -Xmx6G -jar server.jar --nogui
LimitNOFILE=65536
SuccessExitStatus=143
EOF

install -d -o minecraft -g minecraft /opt/pumpkin-loadtest
fetch "/opt/pumpkin-loadtest/pumpkin-$PUMPKIN_TAG" \
    "https://github.com/Pumpkin-MC/Pumpkin/releases/download/${PUMPKIN_TAG/+/%2B}/pumpkin-X64-Linux" \
    sha256 "$PUMPKIN_SHA256"
chmod 755 "/opt/pumpkin-loadtest/pumpkin-$PUMPKIN_TAG"
ln -sfn "pumpkin-$PUMPKIN_TAG" /opt/pumpkin-loadtest/pumpkin
# Pumpkin's defaults with the benchmark settings (seed, port, offline, distances, peaceful), no
# Bedrock listener, no telemetry and no update check.
write_if_changed /opt/pumpkin-loadtest/pumpkin.toml <<'EOF'
seed = "-578941029922802438"
default_difficulty = "Peaceful"
op_permission_level = 4
allow_nether = true
allow_end = true
hardcore = false
tps = 20.0
default_gamemode = "Survival"
force_gamemode = false
scrub_ips = true
use_favicon = true
default_level_name = "world"
allow_chat_reports = false
white_list = false
enforce_whitelist = false
accepts_transfers = true
spawn_protection = 0

[logging]
enabled = true
level = "info"
threads = false
thread_ids = false
target = false
color = false
timestamp = true
timestamp_format = "[hour]:[minute]:[second]"
file = "latest.log"

[world]
lighting = "default"
autosave_ticks = 6000

[networking.java]
enabled = true
address = "0.0.0.0:30900"
encryption = false
online_mode = false
max_players = 10000
view_distance = 10
simulation_distance = 10
keep_alive_time = 15
motd = "Pumpkin benchmark reference"

[networking.java.compression]
enabled = true
threshold = 256
level = 4

[networking.bedrock]
enabled = false

[networking.lan_broadcast]
enabled = false

[commands]
use_console = true
use_tty = false
log_console = true
broadcast_console_to_ops = true
default_op_level = 0

[update_checker]
enabled = false

[telemetry]
enabled = false
EOF
chown -R minecraft:minecraft /opt/pumpkin-loadtest
write_if_changed /etc/systemd/system/pumpkin-loadtest-console.socket <<'EOF'
[Unit]
Description=Console FIFO of the Pumpkin benchmark reference
[Socket]
ListenFIFO=/run/vanillite-console/pumpkin
SocketUser=minecraft
SocketMode=0600
RemoveOnStop=true
Service=pumpkin-loadtest.service
EOF
write_if_changed /etc/systemd/system/pumpkin-loadtest.service <<'EOF'
[Unit]
Description=Pumpkin benchmark reference (offline mode, port 30900, only during xtask bench)
After=network-online.target pumpkin-loadtest-console.socket
Requires=pumpkin-loadtest-console.socket
[Service]
User=minecraft
WorkingDirectory=/opt/pumpkin-loadtest
ExecStart=/opt/pumpkin-loadtest/pumpkin
Sockets=pumpkin-loadtest-console.socket
StandardInput=socket
StandardOutput=journal
StandardError=journal
LimitNOFILE=65536
MemoryMax=6G
EOF

install -d -o minecraft -g minecraft /opt/velocity-loadtest /opt/velocity-loadtest/plugins/bStats
fetch "/opt/velocity-loadtest/$VELOCITY_JAR" \
    "https://fill-data.papermc.io/v1/objects/$VELOCITY_SHA256/$VELOCITY_JAR" sha256 "$VELOCITY_SHA256"
ln -sfn "$VELOCITY_JAR" /opt/velocity-loadtest/velocity.jar
install -d /etc/vanillite-test
if [ ! -s /etc/vanillite-test/forwarding.secret ]; then
    (umask 077 && head -c 24 /dev/urandom | base64 | tr -d '/+=' > /etc/vanillite-test/forwarding.secret)
fi
install -m 600 /etc/vanillite-test/forwarding.secret /opt/velocity-loadtest/forwarding.secret
printf 'enabled=false\n' > /opt/velocity-loadtest/plugins/bStats/config.txt
write_if_changed /opt/velocity-loadtest/velocity.toml <<'EOF'
# Velocity in front of the Vanillite benchmark instance 30901 (xtask bench proxy).
# Installed by scripts/testserver/provision.sh.
config-version = "2.9"
bind = "0.0.0.0:30900"
motd = "Velocity benchmark reference"
show-max-players = 10000
online-mode = false
force-key-authentication = false
prevent-client-proxy-connections = false
player-info-forwarding-mode = "MODERN"
forwarding-secret-file = "forwarding.secret"
announce-forge = false
kick-existing-players = false
sample-players-in-ping = false
enable-player-address-logging = false

[servers]
vanillite = "127.0.0.1:30901"
try = ["vanillite"]

[forced-hosts]

[advanced]
compression-threshold = 256
compression-level = -1
# All bots connect from one address.
login-ratelimit = 0
connection-timeout = 5000
read-timeout = 30000
haproxy-protocol = false
tcp-fast-open = false
bungee-plugin-message-channel = true
show-ping-requests = false
failover-on-unexpected-server-disconnect = true
announce-proxy-commands = true
log-command-executions = false
log-player-connections = false
accepts-transfers = false

[query]
enabled = false
port = 30900
map = "Velocity"
show-plugins = false
EOF
chown -R minecraft:minecraft /opt/velocity-loadtest
write_if_changed /etc/systemd/system/velocity-loadtest.service <<'EOF'
[Unit]
Description=Velocity benchmark reference in front of 30901 (port 30900, only during xtask bench)
After=network-online.target
[Service]
User=minecraft
WorkingDirectory=/opt/velocity-loadtest
ExecStart=/usr/bin/java -Xms512M -Xmx2G -jar velocity.jar
LimitNOFILE=65536
SuccessExitStatus=143
EOF

install -d -o vanillite -g vanillite /opt/vanillite/bench/proxy
install -m 600 -o vanillite -g vanillite /etc/vanillite-test/forwarding.secret \
    /opt/vanillite/bench/proxy/forwarding.secret
write_if_changed /opt/vanillite/bench/proxy/proxy.toml <<'EOF'
# vanillite-proxy in front of the Vanillite benchmark instance 30901 (xtask bench proxy).
# Installed by scripts/testserver/provision.sh.
bind = "0.0.0.0:30900"
online-mode = false
motd = "vanillite-proxy benchmark"
max-players = 10000
forwarding-secret-file = "forwarding.secret"
try = ["vanillite"]

[servers]
vanillite = "127.0.0.1:30901"

[limits]
max-connections = 12000
# All bots connect from one address.
max-pending-per-ip = 1000
login-interval-ms = 0
EOF
write_if_changed /etc/systemd/system/vanillite-proxy-loadtest.service <<'EOF'
[Unit]
Description=vanillite-proxy benchmark in front of 30901 (port 30900, only during xtask bench)
After=network-online.target
[Service]
User=vanillite
Group=vanillite
WorkingDirectory=/opt/vanillite/bench/proxy
ExecStart=/opt/vanillite/bench/bin/vanillite-proxy
LimitNOFILE=65536
MemoryMax=2G
EOF

write_if_changed /usr/local/bin/vanillite-bench 755 <<'EOF'
#!/usr/bin/env bash
# Usage: vanillite-bench <command> [<target>] ...   (driven by `cargo xtask bench`)
# Targets: vanillite (vanillite@30901), paper, vanilla, pumpkin (30900), velocity and
# vanillite-proxy (30900, in front of 30901).
#   props <target>                 write the shared benchmark settings into server.properties
#   reset <target>                 stop it, delete its world (fresh world with the shared seed) and
#                                  drop its hardware limits
#   limit <target> <cpu %> <MiB>   hardware limits for its next start (cgroup CPUQuota, MemoryMax),
#                                  like a small container; `reset` drops them
#   start <target> [<profile>]     start it (Vanillite: rule profile) and wait for a status ping;
#                                  prints {"ready_s", "version", "protocol"}
#   stop <target>                  clean stop (saves); prints {"stop_s"}
#   cmds <target>                  console commands from stdin, one per line; prints
#                                  {"sent", "failed", "errors"}; exit 3 when it has no console
#   timed <target> <timeout s> <regex|-> <command...>
#                                  runs a command and prints {"seconds"} until a log line matches
#                                  (-: until RCON answers)
#   cpu <target>                   prints {"cpu_s"}: CPU time of the server process so far
#   chunks <target>                prints {"chunks"}: chunks in the overworld region files
#   idle <target> <seconds>        prints {"rss_mib", "rss_max_mib", "cpu_percent"} over the time
#   chunky on|off                  add or remove the Chunky plugin of paper-loadtest
#   install <name>                 binary from stdin into /opt/vanillite/bench/bin/<name>
#   lock <owner>                   waits for the benchmark lock (/run/vanillite-bench.lock), prints
#                                  "busy <owner>" while another run holds it, then "locked", and
#                                  holds it until stdin closes (xtask bench and loadtest take it)
#   pregen <radius> <threads> <repetitions>
#                                  runs bin/chunk_bench (generate, save, load) and prints its JSON
# Installed by scripts/testserver/provision.sh.
set -euo pipefail
seed=-578941029922802438
cmd=${1:?usage: vanillite-bench <command> [<target>] ...}
shift
target=${1:-}

unit_of() {
    case $1 in
        vanillite) echo vanillite@30901.service ;;
        paper | vanilla | pumpkin | velocity | vanillite-proxy) echo "$1-loadtest.service" ;;
        *) echo "unknown target $1" >&2; exit 2 ;;
    esac
}
dir_of() {
    case $1 in
        vanillite) echo /opt/vanillite/30901 ;;
        vanillite-proxy) echo /opt/vanillite/bench/proxy ;;
        *) echo "/opt/$1-loadtest" ;;
    esac
}
port_of() { [ "$1" = vanillite ] && echo 30901 || echo 30900; }
rcon_of() {
    case $1 in
        paper) echo 25576 ;;
        vanilla) echo 25578 ;;
        *) echo "" ;;
    esac
}
now() { date +%s.%N; }
since() { awk -v a="$1" -v b="$(now)" 'BEGIN { printf "%.3f", b - a }'; }

props() {
    local file=$1 port=$2 rcon=$3
    touch "$file"
    set_prop() {
        if grep -q "^$1=" "$file"; then
            sed -i "s|^$1=.*|$1=$2|" "$file"
        else
            echo "$1=$2" >> "$file"
        fi
    }
    set_prop server-port "$port"
    set_prop online-mode false
    set_prop white-list false
    set_prop enforce-secure-profile false
    set_prop max-players 10000
    set_prop level-seed "$seed"
    set_prop level-type minecraft:normal
    set_prop generate-structures true
    set_prop spawn-protection 0
    # Straight-flight bots (loadbot --movement straight:...) cruise above the terrain.
    set_prop allow-flight true
    # No mobs spawn by themselves: hostile mobs would kill the bots, which do not respawn.
    set_prop difficulty peaceful
    set_prop view-distance 10
    set_prop simulation-distance 10
    # The world keeps ticking without players: idle numbers measure a running server.
    set_prop pause-when-empty-seconds 0
    if [ -n "$rcon" ]; then
        set_prop enable-rcon true
        set_prop rcon.port "$rcon"
        set_prop rcon.password "$(cat /root/.paper-rcon)"
    fi
    echo "eula=true" > "$(dirname "$file")/eula.txt"
}

# Sends the lines of stdin to the console of $target; prints the JSON summary.
send_cmds() {
    local unit=$1 rcon=$2 fifo=$3
    if [ -n "$rcon" ]; then
        python3 -c '
import json, socket, struct, sys
port = int(sys.argv[1])
pw = open("/root/.paper-rcon").read().strip()
def pkt(i, t, body):
    b = body.encode() + b"\x00\x00"
    return struct.pack("<iii", len(b) + 8, i, t) + b
def recv(s):
    n = struct.unpack("<i", s.recv(4))[0]
    data = b""
    while len(data) < n:
        data += s.recv(n - len(data))
    return struct.unpack("<ii", data[:8]), data[8:-2].decode(errors="replace")
s = socket.create_connection(("127.0.0.1", port), timeout=1800)
s.sendall(pkt(1, 3, pw))
if recv(s)[0][0] == -1:
    sys.exit("RCON auth failed")
sent, errors = 0, []
bad = ("<--[HERE]", "unexpected error")
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    s.sendall(pkt(2, 2, line))
    reply = recv(s)[1]
    sent += 1
    if any(b in reply for b in bad):
        errors.append(line[:80] + ": " + reply[:160])
print(json.dumps({"sent": sent, "failed": len(errors), "errors": errors[:5]}))
' "$rcon"
        return
    fi
    [ -p "$fifo" ] || { echo '{"sent": 0, "failed": 0, "errors": ["no console"]}'; exit 3; }
    local start marker sent tmp
    start=$(date '+%Y-%m-%d %H:%M:%S')
    marker="bench-marker-$RANDOM$RANDOM"
    # The commands, then an unknown command as a marker: its error line says that all of them
    # ran (every server logs console errors).
    tmp=$(mktemp)
    grep . > "$tmp" || true
    sent=$(wc -l < "$tmp")
    { cat "$tmp"; echo "$marker"; } > "$fifo"
    rm -f "$tmp"
    local deadline=$((SECONDS + 300))
    while [ "$SECONDS" -lt "$deadline" ]; do
        journalctl -q -u "$unit" --since "$start" -o cat | grep -q "$marker" && break
        sleep 0.5
    done
    journalctl -q -u "$unit" --since "$start" -o cat \
        | grep -E '<--\[HERE\]|unexpected error' | grep -v "$marker" \
        | head -n 50 | jq -R -s --argjson sent "$sent" \
            '(split("\n") | map(select(length > 0))) as $e
             | {sent: $sent, failed: ($e | length), errors: ($e[:5] | map(.[:200]))}'
}

case $cmd in
    props)
        case $target in
            vanillite | paper | vanilla)
                props "$(dir_of "$target")/server.properties" "$(port_of "$target")" "$(rcon_of "$target")"
                chown "$( [ "$target" = vanillite ] && echo vanillite || echo minecraft)": \
                    "$(dir_of "$target")/server.properties" "$(dir_of "$target")/eula.txt"
                ;;
            *) ;; # Pumpkin and the proxies have their own configuration files.
        esac
        ;;
    reset)
        systemctl stop "$(unit_of "$target")" 2> /dev/null || true
        if [ -e "/run/systemd/system/$(unit_of "$target").d/zz-bench-limit.conf" ]; then
            rm -f "/run/systemd/system/$(unit_of "$target").d/zz-bench-limit.conf"
            systemctl daemon-reload
        fi
        "$0" props "$target"
        rm -rf "$(dir_of "$target")"/world* "$(dir_of "$target")/tick.log"
        ;;
    start)
        unit=$(unit_of "$target")
        dropin=/run/systemd/system/vanillite@30901.service.d/bench-profile.conf
        if [ "$target" = vanillite ]; then
            if [ -n "${2:-}" ]; then
                install -d "$(dirname "$dropin")"
                printf '[Service]\nEnvironment=VANILLITE_PROFILE=%s\n' "$2" > "$dropin"
            else
                rm -f "$dropin"
            fi
            systemctl daemon-reload
            rm -f /opt/vanillite/30901/tick.log
        fi
        t0=$(now)
        systemctl start "$unit"
        for _ in $(seq 3000); do
            if status=$(python3 -c '
import json, socket, struct, sys
def varint(v):
    out = b""
    while True:
        b = v & 0x7F
        v >>= 7
        out += bytes([b | (0x80 if v else 0)])
        if not v:
            return out
def read_varint(s):
    v, shift = 0, 0
    while True:
        b = s.recv(1)[0]
        v |= (b & 0x7F) << shift
        shift += 7
        if not b & 0x80:
            return v
port = int(sys.argv[1])
s = socket.create_connection(("127.0.0.1", port), timeout=2)
host = b"127.0.0.1"
hs = varint(0) + varint(0xFFFFFFFF) + varint(len(host)) + host + struct.pack(">H", port) + varint(1)
s.sendall(varint(len(hs)) + hs + varint(1) + varint(0))
read_varint(s)
read_varint(s)
n = read_varint(s)
data = b""
while len(data) < n:
    data += s.recv(n - len(data))
v = json.loads(data).get("version", {})
print(json.dumps({"version": v.get("name"), "protocol": v.get("protocol")}))
' "$(port_of "$target")" 2> /dev/null); then
                echo "$status" | jq -c --argjson s "$(since "$t0")" '{ready_s: $s} + .'
                exit 0
            fi
            systemctl is-active --quiet "$unit" || { echo "$unit stopped while starting" >&2; exit 1; }
            sleep 0.1
        done
        echo "$unit did not answer a status ping within 300 s" >&2
        exit 1
        ;;
    limit)
        # Sorts after the unit's other drop-ins (loadtest.conf sets MemoryMax), so it wins.
        dropin=/run/systemd/system/$(unit_of "$target").d/zz-bench-limit.conf
        install -d "$(dirname "$dropin")"
        printf '[Service]\nCPUQuota=%s%%\nMemoryMax=%sM\nMemorySwapMax=0\n' "${2:?cpu %}" "${3:?MiB}" > "$dropin"
        systemctl daemon-reload
        ;;
    stop)
        t0=$(now)
        systemctl stop "$(unit_of "$target")"
        jq -n -c --argjson s "$(since "$t0")" '{stop_s: $s}'
        ;;
    cmds)
        fifo=
        case $target in
            vanillite) fifo=/run/vanillite-console/30901 ;;
            pumpkin) fifo=/run/vanillite-console/pumpkin ;;
        esac
        send_cmds "$(unit_of "$target")" "$(rcon_of "$target")" "$fifo"
        ;;
    timed)
        timeout=${2:?timeout}
        regex=${3:?regex}
        shift 3
        unit=$(unit_of "$target")
        rcon=$(rcon_of "$target")
        start=$(date '+%Y-%m-%d %H:%M:%S')
        if [ -n "$rcon" ]; then
            t0=$(now)
            echo "$*" | send_cmds "$unit" "$rcon" "" > /dev/null
            # "-": RCON answers when the command has run.
            if [ "$regex" = - ]; then
                jq -n -c --argjson s "$(since "$t0")" '{seconds: $s}'
                exit 0
            fi
        else
            [ "$regex" != - ] || { echo "$target has no RCON: give a log regex" >&2; exit 2; }
            fifo=/run/vanillite-console/$([ "$target" = vanillite ] && echo 30901 || echo "$target")
            [ -p "$fifo" ] || { echo "$target has no console" >&2; exit 3; }
            t0=$(now)
            echo "$*" > "$fifo"
        fi
        deadline=$(awk -v a="$t0" -v t="$timeout" 'BEGIN { print a + t }')
        while awk -v d="$deadline" -v n="$(now)" 'BEGIN { exit !(n < d) }'; do
            if journalctl -q -u "$unit" --since "$start" -o cat | grep -qE "$regex"; then
                jq -n -c --argjson s "$(since "$t0")" '{seconds: $s}'
                exit 0
            fi
            sleep 0.1
        done
        echo "no log line matching $regex within $timeout s" >&2
        exit 1
        ;;
    idle)
        seconds=${2:?seconds}
        unit=$(unit_of "$target")
        pid=$(systemctl show -p MainPID --value "$unit")
        [ "${pid:-0}" -gt 0 ] || { echo "$unit is not running" >&2; exit 1; }
        hz=$(getconf CLK_TCK)
        cpu() { sed 's/^.*) //' "/proc/$pid/stat" | awk '{ print $12 + $13 }'; }
        c0=$(cpu)
        sum=0 max=0
        for _ in $(seq "$seconds"); do
            rss=$(awk '/^VmRSS:/ { print $2 }' "/proc/$pid/status")
            sum=$((sum + rss))
            [ "$rss" -gt "$max" ] && max=$rss
            sleep 1
        done
        c1=$(cpu)
        jq -n -c --argjson sum "$sum" --argjson max "$max" --argjson n "$seconds" \
            --argjson cpu "$((c1 - c0))" --argjson hz "$hz" \
            '{rss_mib: ($sum / $n / 1024 * 100 | round / 100), rss_max_mib: ($max / 1024 * 100 | round / 100),
              cpu_percent: ($cpu / $hz / $n * 100 * 100 | round / 100)}'
        ;;
    cpu)
        pid=$(systemctl show -p MainPID --value "$(unit_of "$target")")
        [ "${pid:-0}" -gt 0 ] || { echo "$target is not running" >&2; exit 1; }
        sed 's/^.*) //' "/proc/$pid/stat" | awk -v hz="$(getconf CLK_TCK)" '{ printf "{\"cpu_s\": %.2f}\n", ($12 + $13) / hz }'
        ;;
    chunks)
        # Chunks stored in the overworld region files: Anvil header entries in use, or entries
        # with data in the newer index copy of Vanillite's own format (.vwr, see vwr.rs).
        python3 -c '
import glob, json, struct, sys
n = 0
for f in glob.glob(sys.argv[1] + "/world*/**/region/r.*.*", recursive=True):
    if "nether" in f or "the_end" in f or "DIM" in f:
        continue
    with open(f, "rb") as h:
        head = h.read(4096 + 2 * 24832)
    if f.endswith(".mca"):
        n += sum(1 for i in range(0, 4096, 4) if head[i:i + 4] != b"\0\0\0\0")
    elif f.endswith(".vwr") and len(head) == 4096 + 2 * 24832:
        index = max((4096, 4096 + 24832), key=lambda o: struct.unpack_from("<Q", head, o)[0])
        n += sum(1 for i in range(1024) if struct.unpack_from("<I", head, index + 16 + 24 * i + 6)[0])
print(json.dumps({"chunks": n}))
' "$(dir_of "$target")"
        ;;
    lock)
        exec 9>> /run/vanillite-bench.lock
        if ! flock -n 9; then
            echo "busy $(cat /run/vanillite-bench.owner 2> /dev/null)"
            flock 9
        fi
        echo "${target:-unknown} since $(date -u '+%Y-%m-%d %H:%M UTC')" > /run/vanillite-bench.owner
        echo locked
        # The lock (fd 9) lives as long as this process: until the caller closes stdin.
        cat > /dev/null
        ;;
    chunky)
        case $target in
            on) ln -sfn /opt/vanillite/bench/Chunky-Paper-*.jar /opt/paper-loadtest/plugins/Chunky.jar ;;
            off) rm -f /opt/paper-loadtest/plugins/Chunky.jar ;;
            *) echo "usage: vanillite-bench chunky on|off" >&2; exit 2 ;;
        esac
        ;;
    install)
        case $target in
            vanillite-proxy | chunk_bench) ;;
            *) echo "usage: vanillite-bench install vanillite-proxy|chunk_bench < binary" >&2; exit 2 ;;
        esac
        tmp=$(mktemp -p /opt/vanillite/bench/bin)
        cat > "$tmp"
        [ -s "$tmp" ] || { rm -f "$tmp"; echo "no binary on stdin" >&2; exit 2; }
        chmod 755 "$tmp"
        chown vanillite:vanillite "$tmp"
        mv -f "$tmp" "/opt/vanillite/bench/bin/$target"
        ;;
    pregen)
        radius=${1:?radius}
        threads=${2:?threads}
        reps=${3:-1}
        work=/opt/vanillite/bench/pregen
        install -d -o vanillite -g vanillite "$work"
        for _ in $(seq "$reps"); do
            runuser -u vanillite -- env VANILLITE_CACHE=/opt/vanillite/30901/cache \
                /opt/vanillite/bench/bin/chunk_bench --radius "$radius" --threads "$threads" \
                --pump --load --dir "$work/world" 2> /dev/null
        done
        rm -rf "$work/world"
        ;;
    *)
        sed -n '2,/^# Installed/p' "$0" | sed 's/^# \{0,1\}//' >&2
        exit 2
        ;;
esac
EOF
"/usr/local/bin/vanillite-bench" props paper
"/usr/local/bin/vanillite-bench" props vanilla

write_if_changed /usr/local/bin/vanillite-loadtest-monitor 755 <<'EOF'
#!/usr/bin/env bash
# Usage: vanillite-loadtest-monitor <unit>
# Streams load test metrics of a systemd unit to stdout until the connection closes:
#   H <clock ticks per second>
#   S <rss KiB> <cpu ticks> <net rx bytes> <net tx bytes> <read syscalls> <storage read bytes>
#     <storage write bytes>                                  every second
#   T <tick nanoseconds>                                     every tick (vanillite@<port>)
#   Q <output of /tick query>                                every 5 s (paper/vanilla-loadtest)
# Installed by scripts/testserver/provision.sh.
set -u
unit=${1:?usage: vanillite-loadtest-monitor <unit>}
trap 'kill $(jobs -p) 2> /dev/null' EXIT
echo "H $(getconf CLK_TCK)"
case $unit in
    vanillite@*)
        port=${unit#vanillite@}
        port=${port%.service}
        tail -n 0 -F "/opt/vanillite/$port/tick.log" 2> /dev/null | sed -u 's/^/T /' &
        ;;
    paper-loadtest* | vanilla-loadtest*)
        rcon=25576
        case $unit in vanilla-loadtest*) rcon=25578 ;; esac
        while sleep 5; do
            echo "Q $(paper-rcon --port "$rcon" tick query 2> /dev/null | tr '\n' ' ')"
        done &
        ;;
esac
while sleep 1; do
    pid=$(systemctl show -p MainPID --value "$unit")
    rss=0 cpu=0 io="0 0 0"
    if [ "${pid:-0}" -gt 0 ] && [ -r "/proc/$pid/stat" ]; then
        rss=$(awk '/^VmRSS:/ { print $2 }' "/proc/$pid/status")
        cpu=$(sed 's/^.*) //' "/proc/$pid/stat" | awk '{ print $12 + $13 }')
        io=$(awk '{ v[$1] = $2 } END { print v["syscr:"] + 0, v["read_bytes:"] + 0, v["write_bytes:"] + 0 }' "/proc/$pid/io" 2> /dev/null)
    fi
    net=$(awk -F'[: ]+' 'NR > 2 && $2 != "lo" { rx += $3; tx += $11 } END { print rx + 0, tx + 0 }' /proc/net/dev)
    echo "S $rss $cpu $net $io"
done
EOF

echo "== proxy (vanillite-proxy on 30010; Paper with modern forwarding on 30011)"
if [ ! -s /etc/vanillite-test/forwarding.secret ]; then
    (umask 077 && head -c 24 /dev/urandom | base64 | tr -d '/+=' > /etc/vanillite-test/forwarding.secret)
fi
secret=$(cat /etc/vanillite-test/forwarding.secret)
# Vanillite 30001 and 30002 accept the proxy's players and still direct connections.
printf 'VANILLITE_PROXY__VELOCITY_SECRET=%s\nVANILLITE_PROXY__ALLOW_DIRECT_CONNECTIONS=true\n' \
    "$secret" | write_if_changed /etc/vanillite-test/proxy-backend.env 600
for port in 30001 30002; do
    install -d "/etc/systemd/system/vanillite@$port.service.d"
    write_if_changed "/etc/systemd/system/vanillite@$port.service.d/proxy.conf" <<'EOF'
[Service]
EnvironmentFile=/etc/vanillite-test/proxy-backend.env
EOF
done

echo "== Bedrock (UDP 30019 in front of Vanillite 30001)"
install -d /etc/systemd/system/vanillite@30001.service.d
write_if_changed /etc/systemd/system/vanillite@30001.service.d/bedrock.conf <<'EOF'
[Service]
Environment=VANILLITE_BEDROCK__ENABLED=true
Environment=VANILLITE_BEDROCK__PORT=30019
EOF

# Paper behind the proxy: offline mode in server.properties, identities from the proxy.
install -d -o minecraft -g minecraft /opt/paper-proxy /opt/paper-proxy/config
ln -sfn "$jar" /opt/paper-proxy/paper.jar
echo "eula=true" > /opt/paper-proxy/eula.txt
props=/opt/paper-proxy/server.properties
touch "$props"
set_prop() {
    if grep -q "^$1=" "$props"; then
        sed -i "s|^$1=.*|$1=$2|" "$props"
    else
        echo "$1=$2" >> "$props"
    fi
}
set_prop server-port 30011
set_prop online-mode false
set_prop white-list true
set_prop enforce-whitelist true
set_prop motd "Vanillite test: Paper behind the proxy"
set_prop enable-rcon true
set_prop rcon.port 25577
set_prop rcon.password "$rcon_password"
cp /opt/paper/whitelist.json /opt/paper/ops.json /opt/paper-proxy/
global=/opt/paper-proxy/config/paper-global.yml
[ -f "$global" ] || cp /opt/paper/config/paper-global.yml "$global"
sed -i "/^  velocity:/,/^  [a-z]/{s/^    enabled: .*/    enabled: true/;s/^    online-mode: .*/    online-mode: true/;s/^    secret: .*/    secret: '$secret'/}" "$global"
chown -R minecraft:minecraft /opt/paper-proxy
write_if_changed /etc/systemd/system/paper-proxy.service <<'EOF'
[Unit]
Description=Paper behind vanillite-proxy (modern forwarding, port 30011)
After=network-online.target
[Service]
User=minecraft
WorkingDirectory=/opt/paper-proxy
ExecStart=/usr/bin/java -Xms512M -Xmx1536M -jar paper.jar --nogui
Restart=always
RestartSec=5
[Install]
WantedBy=multi-user.target
EOF

install -d -o vanillite -g vanillite /opt/vanillite-proxy
install -m 600 -o vanillite -g vanillite /etc/vanillite-test/forwarding.secret \
    /opt/vanillite-proxy/forwarding.secret
write_if_changed /opt/vanillite-proxy/proxy.toml <<'EOF'
# vanillite-proxy on the test server. Installed by scripts/testserver/provision.sh.
bind = "0.0.0.0:30010"
online-mode = true
motd = "Vanillite test: proxy (/server to switch)"
max-players = 100
forwarding-secret-file = "forwarding.secret"
try = ["vanillite", "import", "paper"]

[servers]
vanillite = "127.0.0.1:30001"
import = "127.0.0.1:30002"
paper = "127.0.0.1:30011"

[permissions]
server = ["*"]
glist = ["*"]

# Bedrock players join the proxy's network on UDP 30020.
[bedrock]
enabled = true
bind = "0.0.0.0:30020"
EOF
write_if_changed /etc/systemd/system/vanillite-proxy.service <<'EOF'
[Unit]
Description=vanillite-proxy on port 30010
After=network-online.target
[Service]
User=vanillite
Group=vanillite
WorkingDirectory=/opt/vanillite-proxy
ExecStart=/opt/vanillite-proxy/vanillite-proxy
Environment=RUST_BACKTRACE=1
Restart=always
RestartSec=5
LimitNOFILE=65536
MemoryMax=1G
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/opt/vanillite-proxy
[Install]
WantedBy=multi-user.target
EOF
write_if_changed /usr/local/bin/vanillite-proxy-deploy 755 <<'EOF'
#!/usr/bin/env bash
# Usage: vanillite-proxy-deploy < vanillite-proxy-binary
# Installs the binary as /opt/vanillite-proxy/vanillite-proxy and (re)starts the proxy.
# Only vanillite-autodeploy (VANILLITE_DEPLOY_AUTO=1) deploys here: the proxy follows dev.
# Installed by scripts/testserver/provision.sh.
set -euo pipefail
if [ "${VANILLITE_DEPLOY_AUTO:-}" != 1 ]; then
    echo "the proxy runs the newest green dev (vanillite-autodeploy): land on dev instead" >&2
    exit 2
fi
dir=/opt/vanillite-proxy
tmp=$(mktemp -p "$dir" .proxy.XXXXXX)
cat > "$tmp"
[ -s "$tmp" ] || { rm -f "$tmp"; echo "no binary on stdin" >&2; exit 2; }
chmod 755 "$tmp"
chown vanillite:vanillite "$tmp"
mv -f "$tmp" "$dir/vanillite-proxy"
systemctl enable --quiet vanillite-proxy.service
systemctl restart vanillite-proxy.service
sleep 1
systemctl --no-pager --lines=10 status vanillite-proxy.service || true
EOF

echo "== chunk generation test server (Vanillite 30030: flight, no movement checks)"
# Players fly fast through new terrain to test chunk generation and loading: the server never
# teleports them back ("moved too quickly") and never kicks them for flying.
install -d /etc/systemd/system/vanillite@30030.service.d
write_if_changed /etc/systemd/system/vanillite@30030.service.d/chunk-test.conf <<'EOF'
[Service]
Environment=VANILLITE_MOVEMENT__TOO_FAST_CHECK=false
Environment=VANILLITE_MOVEMENT__ELYTRA_TOO_FAST_CHECK=false
Environment=VANILLITE_MOVEMENT__WRONG_MOVEMENT_CHECK=false
Environment=VANILLITE_MOVEMENT__FLYING_KICK=false
EOF
install -d -o vanillite -g vanillite /opt/vanillite/30030
props30030=/opt/vanillite/30030/server.properties
[ -f "$props30030" ] || install -o vanillite -g vanillite -m 644 /dev/null "$props30030"
for kv in allow-flight=true view-distance=16 simulation-distance=10 spawn-protection=0; do
    grep -q "^${kv%%=*}=" "$props30030" && sed -i "s|^${kv%%=*}=.*|$kv|" "$props30030" \
        || echo "$kv" >> "$props30030"
done

echo "== auto-deploy of dev (vanillite-autodeploy: 30001, 30002, 30030, proxy 30010)"
id vanillite-build > /dev/null 2>&1 \
    || useradd --system --home-dir /opt/vanillite-build --create-home --shell /usr/sbin/nologin vanillite-build
install -d -o vanillite-build -g vanillite-build /opt/vanillite-build
if [ ! -x /opt/vanillite-build/.cargo/bin/rustup ]; then
    curl -fsSL https://sh.rustup.rs | runuser -u vanillite-build -- env HOME=/opt/vanillite-build \
        sh -s -- -y --quiet --no-modify-path --profile minimal --default-toolchain none
fi
install -d /var/lib/vanillite-autodeploy
write_if_changed /usr/local/bin/vanillite-autodeploy 755 <<'EOF'
#!/usr/bin/env bash
# Usage: vanillite-autodeploy [run | status [<lines>]]
# run (vanillite-autodeploy.timer, every 30 minutes): takes the newest dev commit whose CI
# workflow passed (dev lands often, so the head itself is often still running or cancelled); if
# it is newer than the deployed one, builds vanillite and vanillite-proxy (release-fast) in
# /opt/vanillite-build, announces the restart in chat 10 s ahead, deploys 30001, 30002, 30030 and the
# proxy on 30010 (only those whose binary changed) and rolls all of them back to the previous
# binaries if one does not come up. An unbuildable or rolled back commit is not tried again; the
# next green one is.
# Log: /var/log/vanillite-autodeploy.log. Installed by scripts/testserver/provision.sh.
set -uo pipefail
repo=https://github.com/hthoene/vanillite.git
api=https://api.github.com/repos/hthoene/vanillite
ports=(30001 30002 30030)
declare -A motd=([30001]="Vanillite test: own worldgen" [30002]="Vanillite test: Paper world import"
    [30030]="Vanillite test: chunk generation, no movement checks")
build=/opt/vanillite-build
state=/var/lib/vanillite-autodeploy
logfile=/var/log/vanillite-autodeploy.log
start_timeout=120
sha=

log() {
    printf '%s %s %s\n' "$(date '+%F %T')" "${sha:0:12}" "$*" | tee -a "$logfile"
}

case "${1:-run}" in
    status)
        echo "deployed: $(cat "$state/deployed" 2> /dev/null || echo none)"
        tail -n "${2:-15}" "$logfile" 2> /dev/null
        exit 0
        ;;
    run) ;;
    *)
        echo "usage: vanillite-autodeploy [run | status [<lines>]]" >&2
        exit 2
        ;;
esac

install -d "$state"
exec 9> "$state/lock"
flock -n 9 || exit 0

as_build() {
    runuser -u vanillite-build -- env HOME="$build" PATH="$build/.cargo/bin:/usr/bin:/bin" "$@"
}

# The newest dev commit whose whole CI workflow (all jobs) succeeded.
sha=$(curl -fsS -H 'Accept: application/vnd.github+json' \
    "$api/actions/workflows/ci.yml/runs?branch=dev&event=push&status=success&per_page=1" \
    | jq -r '.workflow_runs[0].head_sha // empty')
[ -n "$sha" ] || { echo "no green dev commit found (GitHub API unavailable?)" >&2; exit 0; }
deployed=$(cat "$state/deployed" 2> /dev/null)
[ "$sha" = "$deployed" ] && exit 0
[ "$sha" = "$(cat "$state/skipped" 2> /dev/null)" ] && exit 0
as_build bash -ec '
    cd "$HOME"
    [ -d src/.git ] || git clone --quiet "$1" src
    git -C src fetch --quiet origin dev
' build "$repo" || { echo "cannot fetch dev" >&2; exit 0; }
# A re-run of an older commit's CI does not take the servers back.
if [ -n "$deployed" ] && as_build git -C "$build/src" merge-base --is-ancestor "$sha" "$deployed"; then
    exit 0
fi

log "building"
started=$SECONDS
if ! as_build bash -ec '
    cd "$HOME/src"
    git checkout --quiet --force --detach "$1"
    rustup toolchain install --profile minimal > /dev/null
    CARGO_TARGET_DIR="$HOME/target" cargo build --locked --profile release-fast \
        -p vanillite -p vanillite-proxy
' build "$sha" > "$state/build.log" 2>&1; then
    log "FAILED: build failed after $((SECONDS - started)) s, see $state/build.log"
    tail -n 30 "$state/build.log"
    echo "$sha" > "$state/skipped"
    exit 1
fi
log "built in $((SECONDS - started)) s"
server_bin=$build/target/release-fast/vanillite
proxy_bin=$build/target/release-fast/vanillite-proxy

changed=()
for port in "${ports[@]}"; do
    cmp -s "$server_bin" "/opt/vanillite/$port/vanillite" || changed+=("$port")
done
proxy_changed=false
cmp -s "$proxy_bin" /opt/vanillite-proxy/vanillite-proxy || proxy_changed=true
if [ ${#changed[@]} -eq 0 ] && ! $proxy_changed; then
    echo "$sha" > "$state/deployed"
    log "ok: binaries unchanged, nothing restarted"
    exit 0
fi

# Restarting the proxy disconnects the players of every server behind it.
msg="Restart in 10 seconds: update to dev ${sha:0:7}"
if $proxy_changed; then
    for port in "${ports[@]}"; do vanillite-console "$port" "say $msg" > /dev/null 2>&1; done
    paper-rcon --port 25577 "say $msg" > /dev/null 2>&1
else
    for port in "${changed[@]}"; do vanillite-console "$port" "say $msg" > /dev/null 2>&1; done
fi
sleep 10

# Installs a server binary (clean stop with save, start) and waits for "Done".
deploy_server() { # <port> <binary> <commit label>
    local port=$1 since
    since=$(date '+%F %T')
    VANILLITE_DEPLOY_AUTO=1 VANILLITE_DEPLOY_MOTD="${motd[$port]} - dev $3" \
        vanillite-deploy "$port" < "$2" > /dev/null 2>&1 || return 1
    for _ in $(seq "$start_timeout"); do
        journalctl -q -u "vanillite@$port" --since "$since" -o cat | grep -q 'Done (' && return 0
        sleep 1
    done
    return 1
}
deploy_proxy() { # <binary>
    VANILLITE_DEPLOY_AUTO=1 vanillite-proxy-deploy < "$1" > /dev/null 2>&1 || return 1
    for _ in $(seq 30); do
        systemctl is-active --quiet vanillite-proxy && ss -Hltn 'sport = :30010' | grep -q . \
            && return 0
        sleep 1
    done
    return 1
}

failed=
done_ports=()
for port in "${changed[@]}"; do
    cp -p "/opt/vanillite/$port/vanillite" "$state/prev-$port" 2> /dev/null || rm -f "$state/prev-$port"
    done_ports+=("$port")
    if ! deploy_server "$port" "$server_bin" "${sha:0:7}"; then
        failed="vanillite@$port did not start within $start_timeout s"
        journalctl -q -u "vanillite@$port" -n 30 -o cat --no-pager
        break
    fi
done
proxy_done=false
if [ -z "$failed" ] && $proxy_changed; then
    cp -p /opt/vanillite-proxy/vanillite-proxy "$state/prev-proxy" 2> /dev/null || rm -f "$state/prev-proxy"
    proxy_done=true
    if ! deploy_proxy "$proxy_bin"; then
        failed="vanillite-proxy did not listen on 30010 within 30 s"
        journalctl -q -u vanillite-proxy -n 30 -o cat --no-pager
    fi
fi

if [ -n "$failed" ]; then
    log "FAILED: $failed; rolling back to ${deployed:0:12}"
    echo "$sha" > "$state/skipped"
    for port in "${done_ports[@]}"; do
        if [ -f "$state/prev-$port" ]; then
            deploy_server "$port" "$state/prev-$port" "${deployed:0:7} (rollback)" \
                || log "rollback of vanillite@$port did not start either"
        fi
    done
    if $proxy_done && [ -f "$state/prev-proxy" ]; then
        deploy_proxy "$state/prev-proxy" || log "rollback of vanillite-proxy did not start either"
    fi
    log "rolled back"
    exit 1
fi
echo "$sha" > "$state/deployed"
log "ok: deployed to ${changed[*]}$($proxy_changed && echo ' proxy')"
EOF
write_if_changed /etc/systemd/system/vanillite-autodeploy.service <<'EOF'
[Unit]
Description=Deploy the newest green dev to Vanillite 30001, 30002 and the proxy (vanillite-autodeploy)
After=network-online.target
Wants=network-online.target
[Service]
Type=oneshot
ExecStart=/usr/local/bin/vanillite-autodeploy run
TimeoutStartSec=1h
# The build must not slow down the running servers, nor take their memory.
Nice=10
CPUWeight=20
IOWeight=20
MemoryHigh=3G
MemoryMax=3500M
EOF
write_if_changed /etc/systemd/system/vanillite-autodeploy.timer <<'EOF'
[Unit]
Description=Check for a new green dev every 30 minutes (vanillite-autodeploy)
[Timer]
OnCalendar=*:0/30
AccuracySec=10s
[Install]
WantedBy=timers.target
EOF

echo "== incidents (vanillite-incidents: crashes and suspicious disconnects)"
install -d -m 755 /var/lib/vanillite-incidents
write_if_changed /usr/local/bin/vanillite-incidents 755 <<'EOF'
#!/usr/bin/env python3
"""Crash and disconnect incidents of the Vanillite test servers.

Usage: vanillite-incidents watch | scan <since> [<until>] | list [<count>] | show <id>
  watch  (vanillite-incidents.service) follows the journal of vanillite@*.service and
         vanillite-proxy.service and saves an incident bundle for every crash (panic, crashed
         world thread, exit with an error, kill, OOM kill) and every suspicious disconnect (a log
         line starting with "Suspicious disconnect", a player who loses the connection within
         10 s of logging in or with an error as reason; not on the load test instance 30901)
  scan   the same for a past time range (journalctl --since/--until syntax)
  list   the newest incidents (default 20); "new" marks the first one of its signature
  show   the summary and the backtrace of an incident
Bundles: /var/lib/vanillite-incidents/<id>/ with summary.txt, journal.log (the instance's
journal around the event), backtrace.txt, meta.json; index.tsv lists all incidents.
Retention: 60 days, at most 300 bundles. Installed by scripts/testserver/provision.sh.
"""
import hashlib
import json
import os
import queue
import re
import shutil
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone

ROOT = os.environ.get("VANILLITE_INCIDENTS_DIR", "/var/lib/vanillite-incidents")
CURSOR = os.path.join(ROOT, ".cursor")
INDEX = os.path.join(ROOT, "index.tsv")
UNITS = ["-u", "vanillite@*.service", "-u", "vanillite-proxy.service"]
AUTODEPLOYED = {
    "vanillite@30001.service",
    "vanillite@30002.service",
    "vanillite@30030.service",
    "vanillite-proxy.service",
}
NO_DISCONNECT = {"vanillite@30901.service"}  # load test bots come and go
SETTLE = 30  # seconds after the last trigger line before the bundle is written
BEFORE = {"crash": 300, "disconnect": 120}  # journal seconds before the first trigger line
EARLY = 10  # a disconnect this soon after the login is suspicious
COOLDOWN = 600  # at most one disconnect incident per instance and player in this time
KEEP_DAYS, KEEP_MAX = 60, 300
MAX_LINES = 5000

LOG = re.compile(r"^\[\d\d:\d\d:\d\d\] \[(?P<thread>[^\]]+?)/(?P<level>[A-Z]+)\]: (?P<text>.*)$")
PANIC = re.compile(r"^thread '(?P<thread>[^']*)'(?: \(\d+\))? panicked at (?P<loc>.+?):?$")
FRAME = re.compile(r"^\s*\d+: (?P<sym><?vanillite\S*)")
LOGIN = re.compile(r"^(?P<name>\S+)\[(?P<addr>[^\]]*)\] logged in with entity id \d+ at \((?P<pos>[^)]*)\)")
LOST = re.compile(r"^(?P<name>\S+) (?:lost connection: (?P<reason>.*)|left the game)")
EXIT = re.compile(r"Main process exited, code=(?P<code>\w+), status=(?P<status>\S+)")
SUSPICIOUS = "Suspicious disconnect"


def message(entry):
    msg = entry.get("MESSAGE", "")
    if isinstance(msg, list):
        msg = bytes(msg).decode("utf-8", "replace")
    return msg or ""


def unit_of(entry):
    return entry.get("_SYSTEMD_UNIT") if entry.get("_PID") != "1" else entry.get("UNIT")


def iso(ts):
    return datetime.fromtimestamp(ts, timezone.utc).strftime("%Y-%m-%d %H:%M:%S UTC")


def journal(args):
    out = subprocess.run(["journalctl", "--no-pager", "-q", *args], capture_output=True, text=True,
                         errors="replace")
    return out.stdout


class Watcher:
    def __init__(self):
        self.pending = {}  # key -> incident in the making
        self.last_pid = {}  # unit -> main pid
        self.logins = {}  # (unit, name) -> login time
        self.cooldown = {}  # (unit, name) -> time of the last disconnect incident

    def trigger(self, key, kind, unit, ts, line, **extra):
        inc = self.pending.get(key)
        if inc is None:
            inc = self.pending[key] = {"kind": kind, "unit": unit, "t0": ts, "pid": self.last_pid.get(unit),
                                       "triggers": [], **extra}
        inc["t1"] = ts
        inc["wall"] = time.time()
        if len(inc["triggers"]) < 20:
            inc["triggers"].append(line)

    def feed(self, entry):
        unit = unit_of(entry)
        if not unit:
            return
        ts = int(entry.get("__REALTIME_TIMESTAMP", "0")) / 1e6
        msg = message(entry)
        if entry.get("_PID") == "1":
            m = EXIT.search(msg)
            crashed = m and not (m["code"] == "exited" and m["status"].startswith("0/")) \
                and not (m["code"] == "killed" and m["status"] in ("15/TERM", "2/INT"))
            if crashed or "OOM killer" in msg:
                self.trigger((unit, "crash"), "crash", unit, ts, msg)
            return
        self.last_pid[unit] = entry.get("_PID")
        first = msg.split("\n", 1)[0]
        log = LOG.match(first)
        text = log["text"] if log else first
        if PANIC.match(first) or "crashed; stopping the server" in text:
            self.trigger((unit, "crash"), "crash", unit, ts, first)
        elif text.startswith(SUSPICIOUS) and unit not in NO_DISCONNECT:
            name = text[len(SUSPICIOUS):].lstrip(": ").split(" ", 1)[0]
            self.trigger((unit, "disconnect", name), "disconnect", unit, ts, first, player=name,
                         reason="suspicious")
        elif m := LOGIN.match(text):
            if not m["addr"].startswith("127.0.0.1"):
                self.logins[(unit, m["name"])] = ts
        elif (m := LOST.match(text)) and unit not in NO_DISCONNECT:
            login = self.logins.pop((unit, m["name"]), None)
            key = (unit, m["name"])
            early = login is not None and ts - login <= EARLY
            error = re.search(r"(?i)error|exception|invalid|decode", m["reason"] or "")
            if (early or error) and ts - self.cooldown.get(key, 0) > COOLDOWN:
                self.cooldown[key] = ts
                note = f" ({ts - login:.1f} s after the login)" if early else ""
                self.trigger((unit, "disconnect", m["name"]), "disconnect", unit, ts, first + note,
                             player=m["name"], reason=m["reason"] or "left the game")

    def flush(self, now_journal, force=False):
        for key, inc in list(self.pending.items()):
            if force or time.time() - inc["wall"] >= SETTLE or now_journal - inc["t1"] >= SETTLE:
                del self.pending[key]
                try:
                    collect(inc)
                except Exception as e:  # never stop watching because of one bundle
                    print(f"collecting {key} failed: {e!r}", file=sys.stderr)


def binary_of(unit):
    if unit == "vanillite-proxy.service":
        return "/opt/vanillite-proxy/vanillite-proxy"
    port = unit.split("@", 1)[1].split(".", 1)[0]
    return f"/opt/vanillite/{port}/vanillite"


def deployed(unit):
    info = {}
    if unit in AUTODEPLOYED:
        try:
            with open("/var/lib/vanillite-autodeploy/deployed") as f:
                info["commit"] = f.read().strip()
        except OSError:
            pass
    path = binary_of(unit)
    try:
        h = hashlib.sha256()
        with open(path, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        info["binary"] = path
        info["binary_sha256"] = h.hexdigest()
        info["binary_mtime"] = iso(os.stat(path).st_mtime)
    except OSError:
        pass
    return info


def backtraces(lines):
    """Panic blocks (message and backtrace) among the instance's own journal lines."""
    blocks, block = [], None
    for msg in lines:
        if PANIC.match(msg):
            block = [msg]
            blocks.append(block)
        elif block is not None:
            if LOG.match(msg) or len(block) > 400:
                block = None
            else:
                block.append(msg)
    return blocks


def signature(inc, blocks):
    if inc["kind"] == "disconnect":
        return f"disconnect: {inc['reason']}", ""
    if blocks:
        block = blocks[0]
        loc = PANIC.match(block[0])["loc"]
        if loc.startswith("/rustc/") or "/.cargo/" in loc:
            frame = next((m["sym"] for line in block if (m := FRAME.match(line))), None)
            if frame:
                loc = re.sub(r"::<.*$", "", frame)
        text = block[1] if len(block) > 1 and not block[1].startswith("stack backtrace") else ""
        return f"panic at {loc}", text[:160]
    for line in inc["triggers"]:
        if m := EXIT.search(line):
            return f"exit {m['code']} {m['status']}", ""
        if "OOM killer" in line:
            return "oom kill", ""
    return "crash", inc["triggers"][0][:160]


def players(inc, until):
    """Players online in the instance's process at the event, with their last world and login position."""
    if inc["unit"] == "vanillite-proxy.service" or not inc["pid"]:
        return []
    text = journal([f"_SYSTEMD_UNIT={inc['unit']}", f"_PID={inc['pid']}", "--until", f"@{until:.0f}", "-o", "cat"])
    online = {}
    for line in text.splitlines():
        log = LOG.match(line)
        if not log:
            continue
        body = log["text"]
        if m := LOGIN.match(body):
            online[m["name"]] = {"name": m["name"], "address": m["addr"], "login_position": m["pos"],
                                 "login": line[1:9], "world": None}
        elif m := LOST.match(body):
            if m["name"] in online and m["name"] != inc.get("player"):
                del online[m["name"]]
        name = body.lstrip("[").split("[", 1)[0].split(" ", 1)[0].rstrip(":")
        if name in online and log["thread"].startswith("World "):
            online[name]["world"] = log["thread"][6:]
    if inc.get("player"):
        return [p for n, p in online.items() if n == inc["player"]]
    return list(online.values())


def collect(inc):
    os.makedirs(ROOT, exist_ok=True)
    t0, t1 = inc["t0"], inc["t1"]
    until = t1 + SETTLE
    entries = journal(["-u", inc["unit"], "--since", f"@{t0 - BEFORE[inc['kind']]:.0f}",
                       "--until", f"@{until:.0f}", "-o", "json"]).splitlines()[-MAX_LINES:]
    log_lines, own = [], []
    for raw in entries:
        try:
            e = json.loads(raw)
        except ValueError:
            continue
        ts = int(e.get("__REALTIME_TIMESTAMP", "0")) / 1e6
        msg = message(e)
        stamp = datetime.fromtimestamp(ts, timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3]
        log_lines.append(f"{stamp} {e.get('SYSLOG_IDENTIFIER', '?')}[{e.get('_PID', '?')}]: {msg}")
        if e.get("_PID") != "1":
            own.extend(msg.split("\n"))
    blocks = backtraces(own)
    sig, text = signature(inc, blocks)
    online = players(inc, until if inc["kind"] == "crash" else t1 + 1)
    info = deployed(inc["unit"])

    instance = inc["unit"].split("@", 1)[1].split(".", 1)[0] if "@" in inc["unit"] else "proxy"
    base = f"{datetime.fromtimestamp(t0, timezone.utc):%Y%m%d-%H%M%S}-{instance}-{inc['kind']}"
    ident, n = base, 1
    while os.path.exists(os.path.join(ROOT, ident)):
        n += 1
        ident = f"{base}-{n}"
    seen = []
    if os.path.exists(INDEX):
        with open(INDEX) as f:
            seen = [line.rstrip("\n").split("\t") for line in f if line.strip()]
    earlier = [row[0] for row in seen if len(row) > 5 and row[5] == sig]

    tmp = os.path.join(ROOT, f".{ident}")
    shutil.rmtree(tmp, ignore_errors=True)
    os.makedirs(tmp)
    with open(os.path.join(tmp, "journal.log"), "w") as f:
        f.write("\n".join(log_lines) + "\n")
    if blocks:
        with open(os.path.join(tmp, "backtrace.txt"), "w") as f:
            f.write("\n\n".join("\n".join(b) for b in blocks) + "\n")
    commit = info.get("commit", "")
    meta = {"id": ident, "kind": inc["kind"], "unit": inc["unit"], "pid": inc["pid"], "time": iso(t0),
            "signature": sig, "message": text, "first_of_signature": not earlier,
            "earlier": earlier[-10:], "triggers": inc["triggers"], "players": online, **info}
    with open(os.path.join(tmp, "meta.json"), "w") as f:
        json.dump(meta, f, indent=2)
        f.write("\n")
    summary = [f"Incident {ident}",
               f"Time:      {iso(t0)}",
               f"Instance:  {inc['unit']} (pid {inc['pid'] or '?'})",
               f"Kind:      {inc['kind']}",
               f"Signature: {sig}" + (f"  ({text})" if text else ""),
               f"Seen:      " + (f"{len(earlier)} time(s) before, last {earlier[-1]}" if earlier else "first time"),
               f"Commit:    {commit or 'not an auto-deployed instance'}"
               + (f"; binary {info['binary']} from {info['binary_mtime']}" if "binary" in info else ""),
               "Players:   " + ("none logged" if not online else "")]
    for p in online:
        summary.append(f"  {p['name']} in {p['world'] or '?'}, logged in at {p['login']} UTC"
                       f" at ({p['login_position']})")
    summary.append("Trigger:")
    summary.extend(f"  {line}" for line in inc["triggers"][:10])
    if blocks:
        frames = [line.strip() for line in blocks[0] if FRAME.match(line)][:8]
        if frames:
            summary.append("Backtrace (Vanillite frames, full in backtrace.txt):")
            summary.extend(f"  {line}" for line in frames)
    summary.append("Files: summary.txt, journal.log, meta.json" + (", backtrace.txt" if blocks else ""))
    with open(os.path.join(tmp, "summary.txt"), "w") as f:
        f.write("\n".join(summary) + "\n")
    os.rename(tmp, os.path.join(ROOT, ident))
    with open(INDEX, "a") as f:
        f.write("\t".join([ident, iso(t0), inc["unit"], inc["kind"], commit[:12] or "-", sig,
                           "new" if not earlier else "seen", text.replace("\t", " ")]) + "\n")
    print(f"incident {ident}: {sig}", flush=True)
    prune()


def prune():
    ids = sorted(d for d in os.listdir(ROOT) if d[:1].isdigit())
    cutoff = datetime.fromtimestamp(time.time() - KEEP_DAYS * 86400, timezone.utc).strftime("%Y%m%d")
    for i, ident in enumerate(ids):
        if i < len(ids) - KEEP_MAX or ident[:8] < cutoff:
            shutil.rmtree(os.path.join(ROOT, ident), ignore_errors=True)


def run(args, follow):
    proc = subprocess.Popen(["journalctl", "--no-pager", "-o", "json", *UNITS, *args],
                            stdout=subprocess.PIPE, text=True, errors="replace")
    lines = queue.Queue()

    def reader():
        for line in proc.stdout:
            lines.put(line)
        lines.put(None)

    threading.Thread(target=reader, daemon=True).start()
    w = Watcher()
    now, cursor, saved = 0.0, None, 0.0
    while True:
        try:
            line = lines.get(timeout=1)
        except queue.Empty:
            line = ""
        if line is None:
            break
        if line:
            try:
                entry = json.loads(line)
            except ValueError:
                continue
            now = int(entry.get("__REALTIME_TIMESTAMP", "0")) / 1e6
            cursor = entry.get("__CURSOR", cursor)
            w.feed(entry)
        w.flush(now)
        # The cursor only moves on while nothing is pending: a restart replays an unsaved incident.
        if follow and cursor and not w.pending and time.time() - saved > 10:
            with open(CURSOR + ".tmp", "w") as f:
                f.write(cursor)
            os.replace(CURSOR + ".tmp", CURSOR)
            saved = time.time()
    w.flush(now, force=True)
    return proc.wait()


def rows():
    if not os.path.exists(INDEX):
        return []
    with open(INDEX) as f:
        return [r for r in (line.rstrip("\n").split("\t") for line in f) if len(r) >= 7
                and os.path.isdir(os.path.join(ROOT, r[0]))]


def main(argv):
    cmd = argv[1] if len(argv) > 1 else ""
    if cmd == "watch":
        os.makedirs(ROOT, exist_ok=True)
        cursor = open(CURSOR).read().strip() if os.path.exists(CURSOR) else ""
        if cursor and subprocess.run(["journalctl", "-q", "-n", "1", "--after-cursor", cursor],
                                     capture_output=True).returncode != 0:
            cursor = ""
        return run(["-f", *(["--after-cursor", cursor] if cursor else ["-n", "0"])], follow=True)
    if cmd == "scan" and len(argv) in (3, 4):
        return run(["--since", argv[2], *(["--until", argv[3]] if len(argv) == 4 else [])], follow=False)
    if cmd == "list" and len(argv) <= 3:
        for r in rows()[-int(argv[2] if len(argv) == 3 else 20):]:
            print(f"{r[0]:<36} {r[4]:<12} {r[6]:<4} {r[5]}" + (f"  ({r[7][:80]})" if len(r) > 7 and r[7] else ""))
        return 0
    if cmd == "show" and len(argv) == 3:
        d = os.path.join(ROOT, os.path.basename(argv[2]))
        if not os.path.isdir(d):
            print(f"no incident {argv[2]}", file=sys.stderr)
            return 1
        for name in ("summary.txt", "backtrace.txt"):
            if os.path.exists(os.path.join(d, name)):
                print(open(os.path.join(d, name)).read())
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
EOF
write_if_changed /etc/systemd/system/vanillite-incidents.service <<'EOF'
[Unit]
Description=Save crashes and suspicious disconnects of Vanillite and the proxy (vanillite-incidents)
After=systemd-journald.service
[Service]
ExecStart=/usr/local/bin/vanillite-incidents watch
Restart=always
RestartSec=10
Nice=10
CPUWeight=20
MemoryMax=256M
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/vanillite-incidents
[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable --quiet paper.service paper-proxy.service
systemctl is-active --quiet paper.service || systemctl start paper.service
# Picks up a changed forwarding secret or properties.
systemctl restart paper-proxy.service
systemctl enable --quiet --now paper-creative.timer
for dir in /opt/vanillite/30[0-9][0-9][0-9]; do
    port=${dir##*/}
    [ "$port" = 30901 ] || systemctl enable --quiet --now "vanillite-creative@$port.path"
done
systemctl enable --quiet --now vanillite-autodeploy.timer
# Resumes from its saved journal cursor: nothing between two provisions is lost.
systemctl enable --quiet vanillite-incidents.service
systemctl restart vanillite-incidents.service

echo "== players (whitelisted and OP 4 on Paper; copied into Vanillite instances on deploy)"
for _ in $(seq 60); do
    paper-rcon list > /dev/null 2>&1 && break
    sleep 2
done
echo "$players" | jq -r '.[].name' | while read -r name; do
    jq -e --arg n "$name" 'any(.[]; .name == $n)' /opt/paper/whitelist.json > /dev/null \
        || paper-rcon whitelist add "$name"
    jq -e --arg n "$name" 'any(.[]; .name == $n and .level == 4)' /opt/paper/ops.json > /dev/null \
        || paper-rcon op "$name"
done
echo "== done"
