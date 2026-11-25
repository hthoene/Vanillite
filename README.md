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
[Pterodactyl](https://pterodactyl.io) 1.x (Pelican imports it as well). Import it in the admin
panel under *Nests → Import Egg* (Pelican: *Eggs → Import*), then create a server with it; its
primary allocation is the game port.

- **Image:** *Beta, Minecraft <version>* (`thoenehannes/vanillite:mc<version>-beta`, e.g.
  `mc26.3-beta`; the default) stays on that Minecraft version; *Beta, newest Minecraft version*
  (`beta`) follows new Minecraft versions. Switch it on the *Startup* page.
- **EULA:** switch on *Accept the Minecraft EULA*, or accept the panel's EULA prompt on the first
  start.
- **Essentials first:** the *Startup* page starts with the settings most servers change (profile,
  MOTD, players, game mode, difficulty, whitelist, seed, distances, Bedrock players), as switches
  and dropdowns.
- **Everything else** follows, grouped by area (`Server: …` for `server.properties`,
  `Performance: …`, `Storage: …`, `Exploits: …`, `Bedrock: …`, …); each description names the
  default and the range. Technical settings keep the selected profile's value while they are
  empty or set to `default`.
- **Bedrock** players join on the server's port (UDP) unless *Bedrock: Port* names another
  allocation.

Variables apply to the run and are not written to the files. Startup is detected from the
`Done (…)! For help, type "help"` line and the panel's stop button sends `stop`. The egg is
generated from the configuration schema (`cargo xtask egg`), so re-import it after an update.
