#!/usr/bin/env bash
# Provisions the Vanillite test server (Ubuntu). Idempotent: safe to run again at any time.
# Run with `cargo xtask testserver provision` (pipes this script to `ssh <host> bash -s`).
#
# - Paper reference server on port 30000 (/opt/paper, paper.service, user minecraft).
# - Vanillite test servers on ports 30001-30999 (/opt/vanillite/<port>, vanillite@<port>.service,
#   user vanillite), deployed with `cargo xtask deploy` through /usr/local/bin/vanillite-deploy.
# - Load test instances (`cargo xtask loadtest`), running only during a run: Paper in offline
#   mode on 30900 (/opt/paper-loadtest, paper-loadtest.service) and Vanillite on 30901.
# - Docker engine for `cargo xtask docker` (image builds over SSH).
# - vanillite-proxy on 30010 (/opt/vanillite-proxy, vanillite-proxy.service, deployed with
#   `ssh <host> vanillite-proxy-deploy < target/release-fast/vanillite-proxy`) in front of
#   Vanillite 30001 and 30002 and a Paper with modern forwarding on 30011 (/opt/paper-proxy).
# - Bedrock players on UDP 30019: the Bedrock frontend of Vanillite 30001 (`[bedrock]` set by the
#   drop-in vanillite@30001.service.d/bedrock.conf), and on UDP 30020 the Bedrock frontend of
#   the proxy (`[bedrock]` in /opt/vanillite-proxy/proxy.toml).
# - vanillite-autodeploy (timer, every 30 minutes): builds the newest green dev commit in
#   /opt/vanillite-build and deploys it to 30001, 30002 and the proxy on 30010, with a chat
#   announcement and rollback; log in /var/log/vanillite-autodeploy.log. Manual deploys of a
#   branch go to 30003-30099.
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
Restart=on-failure
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
# 30001 and 30002 belong to vanillite-autodeploy (newest green dev), which sets
# VANILLITE_DEPLOY_AUTO=1 and the MOTD in VANILLITE_DEPLOY_MOTD.
# Installed by scripts/testserver/provision.sh.
set -euo pipefail
port=${1:?usage: vanillite-deploy <port> < binary}
if ! [[ "$port" =~ ^[0-9]+$ ]] || [ "$port" -lt 30001 ] || [ "$port" -gt 30999 ]; then
    echo "port must be 30001-30999 (30000 is Paper)" >&2
    exit 2
fi
if { [ "$port" = 30001 ] || [ "$port" = 30002 ]; } && [ "${VANILLITE_DEPLOY_AUTO:-}" != 1 ]; then
    echo "30001 and 30002 run the newest green dev (vanillite-autodeploy): land on dev instead," >&2
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
    set_prop online-mode false
    set_prop white-list false
    set_prop max-players 10000
    # Straight-flight bots (loadbot --movement straight:...) cruise above the terrain.
    set_prop allow-flight true
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

echo "== load test instances (paper-loadtest on 30900, vanillite@30901)"
install -d /etc/systemd/system/vanillite@30901.service.d
write_if_changed /etc/systemd/system/vanillite@30901.service.d/loadtest.conf <<'EOF'
[Service]
# Tick durations for `cargo xtask loadtest`; the same memory limit as the Paper instance's heap.
Environment=VANILLITE_TICK_LOG=tick.log
MemoryMax=6G
Restart=no
EOF
install -d -o minecraft -g minecraft /opt/paper-loadtest
ln -sfn "$jar" /opt/paper-loadtest/paper.jar
echo "eula=true" > /opt/paper-loadtest/eula.txt
props=/opt/paper-loadtest/server.properties
touch "$props"
set_prop() {
    if grep -q "^$1=" "$props"; then
        sed -i "s|^$1=.*|$1=$2|" "$props"
    else
        echo "$1=$2" >> "$props"
    fi
}
set_prop server-port 30900
set_prop online-mode false
set_prop white-list false
set_prop enforce-secure-profile false
set_prop max-players 10000
set_prop level-type minecraft:flat
set_prop generate-structures false
set_prop spawn-protection 0
# Straight-flight bots (loadbot --movement straight:...) cruise above the terrain.
set_prop allow-flight true
# No mobs, like Vanillite so far: hostile mobs would kill the bots, which do not respawn.
set_prop difficulty peaceful
set_prop spawn-monsters false
set_prop view-distance 10
set_prop simulation-distance 10
set_prop enable-rcon true
set_prop rcon.port 25576
set_prop rcon.password "$rcon_password"
# All bots connect from one address: no per-address connection throttle.
if [ -f /opt/paper-loadtest/bukkit.yml ]; then
    sed -i 's/^\(\s*connection-throttle:\).*/\1 -1/' /opt/paper-loadtest/bukkit.yml
else
    printf 'settings:\n  connection-throttle: -1\n' > /opt/paper-loadtest/bukkit.yml
fi
chown -R minecraft:minecraft /opt/paper-loadtest
write_if_changed /etc/systemd/system/paper-loadtest.service <<'EOF'
[Unit]
Description=Paper load test reference (offline mode, port 30900, only during xtask loadtest)
After=network-online.target
[Service]
User=minecraft
WorkingDirectory=/opt/paper-loadtest
ExecStart=/usr/bin/java -Xms1G -Xmx6G -jar paper.jar --nogui
LimitNOFILE=65536
EOF

write_if_changed /usr/local/bin/vanillite-loadtest-monitor 755 <<'EOF'
#!/usr/bin/env bash
# Usage: vanillite-loadtest-monitor <unit>
# Streams load test metrics of a systemd unit to stdout until the connection closes:
#   H <clock ticks per second>
#   S <rss KiB> <cpu ticks> <net rx bytes> <net tx bytes> <read syscalls> <storage read bytes>
#     <storage write bytes>                                  every second
#   T <tick nanoseconds>                                     every tick (vanillite@<port>)
#   Q <output of /tick query>                                every 5 s (paper-loadtest)
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
    paper-loadtest*)
        while sleep 5; do
            echo "Q $(paper-rcon --port 25576 tick query 2> /dev/null | tr '\n' ' ')"
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
install -d /opt/vanillite/bench/results

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
Restart=on-failure
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
Restart=on-failure
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

echo "== auto-deploy of dev (vanillite-autodeploy: 30001, 30002, proxy 30010)"
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
# /opt/vanillite-build, announces the restart in chat 10 s ahead, deploys 30001, 30002 and the
# proxy on 30010 (only those whose binary changed) and rolls all of them back to the previous
# binaries if one does not come up. An unbuildable or rolled back commit is not tried again; the
# next green one is.
# Log: /var/log/vanillite-autodeploy.log. Installed by scripts/testserver/provision.sh.
set -uo pipefail
repo=https://github.com/hthoene/Vanillite.git
api=https://api.github.com/repos/hthoene/Vanillite
ports=(30001 30002)
declare -A motd=([30001]="Vanillite test: own worldgen" [30002]="Vanillite test: Paper world import")
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
