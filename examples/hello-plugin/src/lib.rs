//! Example Vanillite plugin: a `/hello` command with suggestions, `/hello pad` (bulk block
//! access by numeric id from a task on the player's owner), a join message from its config,
//! and a simple flat generator that replaces the `chunk-generator` slot when
//! `generator.enabled` is set in `plugins/hello/config.toml`.
//!
//! Build it with `cargo build -p vanillite-example-hello` and put `plugin.toml` and the library
//! into `plugins/hello/` (or `plugins/hello/native/<target-triple>/`).

use vanillite_plugin::prelude::*;
use vanillite_plugin::{ChunkBlocks, ChunkGenerator, ChunkGeneratorProvider, WorldInfo};

/// The plugin's state.
struct Hello {
    /// How often `/hello` ran, kept over development reloads.
    greetings: u64,
}

impl Plugin for Hello {
    fn load(registrar: &mut Registrar<Self>) -> Result<Self, Error> {
        registrar.config(include_str!("../config.toml"))?;
        let server = registrar.server();

        registrar.on_join(priority::NORMAL, |_, ctx, event| {
            let server = ctx.server();
            let template = server
                .config_string("messages.join")
                .unwrap_or_else(|| "Welcome, {name}!".to_owned());
            let text = template.replace("{name}", event.name());
            let _ = server.send_message(event.player(), Text::colored(&text, "gold"));
        });

        registrar.command(
            Command::literal("hello")
                .executes(|hello: &mut Hello, ctx, invocation| {
                    let name = match invocation.sender() {
                        vanillite_plugin::Sender::Player(player) => ctx
                            .server()
                            .player_name(player)
                            .unwrap_or_else(|| "player".to_owned()),
                        vanillite_plugin::Sender::Console => "console".to_owned(),
                    };
                    Ok(greet(hello, ctx, invocation, &name))
                })
                .then(
                    Command::literal("pad").executes(|_: &mut Hello, ctx, invocation| {
                        let vanillite_plugin::Sender::Player(player) = invocation.sender() else {
                            return Ok(0);
                        };
                        // Commands cannot touch blocks: the player's owner does it in a task.
                        let server = ctx.server();
                        let (Some(world), Some(glass)) = (
                            server.player_world(player),
                            server.block_state_id("minecraft:glass"),
                        ) else {
                            return Ok(0);
                        };
                        let air = server.block_state_id("minecraft:air").unwrap_or(0);
                        let task =
                            server.schedule(Owner::Entity(world, player), 0, 0, move |ctx| {
                                let _ = pad(ctx, world, player, glass, air);
                            });
                        Ok(i32::from(task.is_ok()))
                    }),
                )
                .then(
                    Command::argument("name", ArgType::Word)
                        .suggests(|_, ctx, _| {
                            let server = ctx.server();
                            server
                                .online_players()
                                .into_iter()
                                .filter_map(|p| server.player_name(p))
                                .collect()
                        })
                        .executes(|hello: &mut Hello, ctx, invocation| {
                            let name = invocation.string("name").unwrap_or("world").to_owned();
                            Ok(greet(hello, ctx, invocation, &name))
                        }),
                ),
        );

        if server.config_bool("generator.enabled") == Some(true) {
            let dimension = server
                .config_string("generator.dimension")
                .unwrap_or_else(|| "minecraft:overworld".to_owned());
            let surface = server.config_int("generator.surface").unwrap_or(64);
            registrar.chunk_generator(FlatProvider {
                dimension,
                surface: i32::try_from(surface).unwrap_or(64),
            });
        }
        vanillite_plugin::info!("Hello is loaded");
        Ok(Hello { greetings: 0 })
    }

    fn enable(&mut self, ctx: &mut Ctx<'_>) {
        let worlds = ctx.server().worlds().len();
        vanillite_plugin::info!("Hello is enabled ({worlds} worlds)");
    }

    fn disable(&mut self, _ctx: &mut Ctx<'_>) {
        vanillite_plugin::info!("Hello is disabled after {} greetings", self.greetings);
    }

    fn save_state(&mut self) -> Vec<u8> {
        self.greetings.to_le_bytes().to_vec()
    }

    fn restore_state(&mut self, state: &[u8]) {
        if let Ok(bytes) = <[u8; 8]>::try_from(state) {
            self.greetings = u64::from_le_bytes(bytes);
        }
    }
}

fn greet(
    hello: &mut Hello,
    ctx: &mut Ctx<'_>,
    invocation: &vanillite_plugin::Invocation<'_>,
    name: &str,
) -> i32 {
    hello.greetings += 1;
    let template = ctx
        .server()
        .config_string("messages.hello")
        .unwrap_or_else(|| "Hello, {name}!".to_owned());
    invocation.reply(
        ctx,
        Text::colored(&template.replace("{name}", name), "aqua"),
    );
    i32::try_from(hello.greetings).unwrap_or(i32::MAX)
}

/// `/hello pad`: fills the air of the 5 × 5 layer under the player with glass, reading and
/// writing the whole layer in one call each, by numeric block state id.
fn pad(ctx: &mut Ctx<'_>, world: World, player: Uuid, glass: u32, air: u32) -> Result<(), Error> {
    let at = ctx.player_position(player)?;
    let (x, y, z) = (
        at.x.floor() as i32,
        at.y.floor() as i32 - 1,
        at.z.floor() as i32,
    );
    let area = BlockBox::new(
        BlockPos::new(x - 2, y, z - 2),
        BlockPos::new(x + 2, y, z + 2),
    );
    let mut blocks = ctx.blocks(world, area)?;
    for state in &mut blocks {
        *state = if *state == air {
            glass
        } else {
            vanillite_plugin::NO_BLOCK
        };
    }
    ctx.set_blocks(world, area, &blocks, SetMode::Update)?;
    Ok(())
}

/// Flat stone with a grass surface in one dimension.
struct FlatProvider {
    dimension: String,
    surface: i32,
}

impl ChunkGeneratorProvider for FlatProvider {
    fn create(&self, world: &WorldInfo) -> Option<Box<dyn ChunkGenerator>> {
        if world.dimension != self.dimension {
            return None;
        }
        let server = Server::get()?;
        Some(Box::new(Flat {
            bedrock: server.block_state_id("minecraft:bedrock")?,
            stone: server.block_state_id("minecraft:stone")?,
            dirt: server.block_state_id("minecraft:dirt")?,
            grass: server.block_state_id("minecraft:grass_block[snowy=false]")?,
            surface: self.surface,
        }))
    }
}

struct Flat {
    bedrock: u32,
    stone: u32,
    dirt: u32,
    grass: u32,
    surface: i32,
}

impl ChunkGenerator for Flat {
    fn generate(&self, chunk: &mut ChunkBlocks<'_>) {
        let bottom = chunk.min_y();
        let top = self
            .surface
            .min(bottom + i32::try_from(chunk.height()).unwrap_or(0) - 1);
        for y in bottom..=top {
            let state = match top - y {
                _ if y == bottom => self.bedrock,
                0 => self.grass,
                1..=3 => self.dirt,
                _ => self.stone,
            };
            chunk.fill_layer(y, state);
        }
    }
}

vanillite_plugin::export_plugin!(Hello);
