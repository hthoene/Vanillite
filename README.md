# Vanillite

A Minecraft Java Edition server written in Rust: 1:1 Vanilla behaviour in game, fast,
multithreaded, low on RAM and modular. Licensed under Apache 2.0.

Development: see [docs/development.md](docs/development.md).

## Docker

The image `docker.hthoene.de/vanillite/vanillite` (tags `dev` and the commit hash, linux/amd64
and linux/arm64) holds a single static binary. World, configuration and cache live in the
volume `/data`; the server listens on 25565 inside the container. Every key of
`vanillite.toml` and `server.properties` can be set with an environment variable
`VANILLITE_<KEY>`: upper case, `-` becomes `_`, `.` becomes `__` (e.g. `VANILLITE_MOTD`,
`VANILLITE_MAX_PLAYERS`, `VANILLITE_PERFORMANCE__TICK__PARALLEL_DIMENSIONS`). Variables apply
to the run only and are not written to the files. `VANILLITE_EULA=true` accepts the
[Minecraft EULA](https://aka.ms/MinecraftEULA).

```sh
docker run -d --name vanillite -p 25565:25565 -v vanillite-data:/data \
    -e VANILLITE_EULA=true -e VANILLITE_MOTD="A Vanillite server" \
    --stop-timeout 60 docker.hthoene.de/vanillite/vanillite:dev
```

```yaml
services:
  vanillite:
    image: docker.hthoene.de/vanillite/vanillite:dev
    ports:
      - "25565:25565"
    volumes:
      - vanillite-data:/data
    environment:
      VANILLITE_EULA: "true"
      VANILLITE_MAX_PLAYERS: "50"
    stop_grace_period: 60s # SIGTERM saves all worlds before the server exits
    restart: unless-stopped
volumes:
  vanillite-data:
```

The first start downloads the Vanilla server jar into `/data/cache`. The health check
(`vanillite health`) sends a status ping. A bind-mounted `/data` must be writable by UID
10001. Console commands go through `docker attach` (start the container with `-it` for that).

## Pterodactyl

[`deploy/pterodactyl/egg-vanillite.json`](deploy/pterodactyl/egg-vanillite.json) is an egg for
[Pterodactyl](https://pterodactyl.io) with the image `thoenehannes/vanillite`. Import it in the
admin panel under *Nests → Import Egg*, then create a server with it. The server's primary
allocation is its port. Every `server.properties` key and every `vanillite.toml` rule is a
variable on the *Startup* page; rule variables left empty keep the value of the selected profile
(or of `vanillite.toml`). Set *Accept the Minecraft EULA* to `true`, or accept the EULA prompt
in the console. Variables apply to the run and are not written to the files. The egg is
generated from the configuration schema (`cargo xtask egg`), so re-import it after an update.
