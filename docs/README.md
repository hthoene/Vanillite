# Vanillite documentation

The user guide (installation, administration, commands and features) is the documentation
site [docs.vanillite.org](https://docs.vanillite.org). This folder holds the reference that is
kept with the code:

| Page | What |
|---|---|
| [Configuration reference](configuration.md) | Every `server.properties` key and `vanillite.toml` rule with its values in each profile. Generated from `vanillite-config` by `cargo xtask config-docs` (also part of `cargo xtask egg`). |
| [Vanillite commands](commands.md) | `/vanillite` (`/vl`): version, status, TPS, tick times, profiler, chunks, entities, memory, config reload and player debug, with their permission nodes. |
| [Worlds](worlds.md) | World formats, folder layouts, conversion and export, saving and backups. |
| [Plugin development](plugins.md) | Writing native plugins with the Rust SDK, `plugin.toml`, threads, the API and ABI versioning. |
| [Architecture](architecture/README.md) | Crates, principles, threads and the way of a chunk; details on the server, configuration and world access. |
| [Development](development.md) | Setup, everyday commands, merging, Minecraft version updates, parity tests, benchmarks, Docker, releases and CI. |
| [Benchmarks](../benchmarks/README.md) | Nightly measurements against Paper, Vanilla and Pumpkin (generated). |
