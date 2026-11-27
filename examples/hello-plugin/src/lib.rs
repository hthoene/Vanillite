//! Example Vanillite plugin: a `/hello` command with suggestions, `/hello pad` (bulk block
//! access by numeric id from a task on the player's owner), a join message from its config,
//! a join counter in the player's persistent data shown above the hotbar, in a scoreboard
//! objective and with a boss bar and a sound, a refused login name, fire protection and kept
//! inventories (`protect.enabled`), a periodic background task, and a simple flat generator
//! that replaces the `chunk-generator` slot when `generator.enabled` is set in
//! `plugins/hello/config.toml`.
//!
//! It also shows the rest of the API: `/hello treat` (food, an effect, an item and particles
//! on the player's owner), `/hello pig <pos>` (spawns a glowing pig), a `hello:ping` plugin
//! message channel that answers on `hello:pong`, an advancement message, a permission node
//! with a default, a `hello:greeter` service for other plugins, a `hello:greeted` plugin
//! event, handlers of another plugin's `economy:payment` event that skip cancelled ones, and
//! the greeting count saved to its config at shutdown.
//!
//! ABI 1.6: `/hello menu` opens a plugin menu whose button (an item marked with persistent
//! data) gives a cookie, `/hello pig` names its pig, makes it faster and gives it a goal that
//! keeps it near its spawn point with a "retired" callback when it is gone, `/hello look`
//! traces a ray along the player's view, mobs drop twice the experience and, with
//! `protect.enabled`, phantoms do not spawn naturally.
//!
//! ABI 1.8: `/hello lobby` creates the flat extra world `hello_lobby` (own folder, like a
//! Multiverse world) on first use and moves the player there once it is loaded.
//!
//! Build it with `cargo build -p vanillite-example-hello` and put `plugin.toml` and the library
//! into `plugins/hello/` (or `plugins/hello/native/<target-triple>/`).

use vanillite_plugin::abi;
use vanillite_plugin::prelude::*;
use vanillite_plugin::{
    ChunkBlocks, ChunkGenerator, ChunkGeneratorProvider, Goal, GoalAction, GoalMob, RayHit,
    WorldInfo,
};

/// The `hello:greeter` service other plugins call (version 1): a `repr(C)` table, so plugins
/// in any language can use it.
#[repr(C)]
#[derive(Debug)]
pub struct GreeterApi {
    /// Size of this table in bytes.
    pub size: u32,
    /// How often `/hello` ran.
    pub greetings: extern "C" fn() -> u64,
}

static GREETER: GreeterApi = GreeterApi {
    size: size_of::<GreeterApi>() as u32,
    greetings,
};

/// The greeting count the service reports.
static GREETINGS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

extern "C" fn greetings() -> u64 {
    GREETINGS.load(std::sync::atomic::Ordering::Relaxed)
}

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
            // The join counter lives in the player's persistent data (`hello:joins`).
            let (world, player) = (event.world(), event.player());
            let joins = match ctx.entity_data(world, player, "joins") {
                Ok(Some(DataValue::Int(n))) => n,
                _ => 0,
            } + 1;
            if ctx
                .set_entity_data(world, player, "joins", &DataValue::Int(joins))
                .is_ok()
            {
                let visit = server
                    .config_string("messages.visit")
                    .unwrap_or_else(|| "Visit #{count}".to_owned());
                let _ =
                    server.send_action_bar(player, visit.replace("{count}", &joins.to_string()));
            }
            // The count in the player list, a welcome boss bar and a sound.
            let _ = server.set_score("hello_joins", event.name(), joins);
            let _ = server.boss_bar("welcome").show(player);
            if let Ok(pos) = ctx.player_position(player) {
                let mut sound = Sound::new("minecraft:entity.player.levelup", pos);
                sound.source = vanillite_plugin::abi::sound_source::PLAYER;
                let _ = server.play_sound(player, &sound);
            }
            if let Some(text) = server.config_string("messages.broadcast") {
                event.set_message(Text::colored(
                    &text.replace("{name}", event.name()),
                    "yellow",
                ));
            }
        });

        registrar.on_plugin_message("hello:ping", priority::NORMAL, |_, ctx, message| {
            let _ =
                ctx.server()
                    .send_plugin_message(message.player(), "hello:pong", message.data());
        });

        registrar.on_advancement(priority::MONITOR, |_, ctx, done| {
            let _ = ctx
                .server()
                .send_action_bar(done.player(), format!("Well done: {}", done.advancement()));
        });

        // Another plugin's event; cancelled payments are skipped.
        registrar.ignore_cancelled().on_plugin_event(
            "economy:payment",
            priority::MONITOR,
            |_, ctx, event| {
                let text = String::from_utf8_lossy(event.data()).into_owned();
                for player in ctx.server().online_players() {
                    let _ = ctx
                        .server()
                        .send_action_bar(player, format!("Payment: {text}"));
                }
            },
        );

        registrar.permission("hello.pad", PermissionDefault::Op);
        registrar.permission("hello.treat", PermissionDefault::True);
        server.register_service("hello:greeter", 1, &GREETER)?;

        registrar.on_login(priority::NORMAL, |_, ctx, login| {
            let server = ctx.server();
            let refused = server.config_string("login.refuse").unwrap_or_default();
            if !refused.is_empty() && login.name() == refused {
                let message = server
                    .config_string("login.message")
                    .unwrap_or_else(|| "This name is reserved.".to_owned());
                login.refuse(Text::colored(&message, "red"));
            }
        });

        // The button of `/hello menu`: clicks in the plugin's menu never move items.
        registrar.on_inventory_click(priority::NORMAL, |_, ctx, click| {
            if click.flags() & abi::menu_flag::PLUGIN == 0 {
                return;
            }
            click.set_cancelled(true);
            let server = ctx.server();
            let button = server.item_data(click.item(), "button");
            let (Some(player), Ok(Some(_))) = (click.entity(), button) else {
                return;
            };
            let world = click.world();
            let _ = server.schedule(Owner::Entity(world, player), 0, 0, move |ctx| {
                let _ = ctx.close_menu(world, player);
                let _ = ctx.give_item(world, player, "{id:\"minecraft:cookie\",count:1}");
            });
        });

        // Mobs drop twice the experience.
        registrar.on_entity_drops(priority::NORMAL, |_, _, drops| {
            let doubled = drops.value().saturating_mul(2);
            drops.set_value(doubled);
        });

        if server.config_bool("protect.enabled") == Some(true) {
            registrar.on_entity_spawn(priority::NORMAL, |_, _, spawn| {
                if spawn.entity_type() == "minecraft:phantom"
                    && spawn.cause() == abi::spawn_reason::NATURAL
                {
                    spawn.set_cancelled(true);
                }
            });
            registrar.on_interact(priority::NORMAL, |_, ctx, interact| {
                if matches!(
                    interact.item(),
                    "minecraft:flint_and_steel" | "minecraft:fire_charge"
                ) {
                    interact.set_cancelled(true);
                    let _ = ctx
                        .server()
                        .send_action_bar(interact.player(), Text::colored("No fire here", "red"));
                }
            });
            registrar.on_death(priority::NORMAL, |_, ctx, death| {
                death.set_keep_inventory(true);
                let name = ctx
                    .server()
                    .player_name(death.player())
                    .unwrap_or_else(|| "A player".to_owned());
                death.set_message(Text::colored(
                    &format!("{name} died and kept everything"),
                    "gray",
                ));
            });
        }

        registrar.command(
            Command::literal("hello")
                .executes(|hello: &mut Hello, ctx, invocation| {
                    let name = match invocation.sender() {
                        vanillite_plugin::Sender::Player(player) => ctx
                            .server()
                            .player_name(player)
                            .unwrap_or_else(|| "player".to_owned()),
                        vanillite_plugin::Sender::Console => "console".to_owned(),
                        vanillite_plugin::Sender::CommandBlock => "command block".to_owned(),
                        vanillite_plugin::Sender::Entity(_) => "entity".to_owned(),
                    };
                    Ok(greet(hello, ctx, invocation, &name))
                })
                .then(
                    Command::literal("treat")
                        .requires_permission("hello.treat")
                        .executes(|_: &mut Hello, ctx, invocation| {
                            let vanillite_plugin::Sender::Player(player) = invocation.sender()
                            else {
                                return Ok(0);
                            };
                            let server = ctx.server();
                            let Some(world) = server.player_world(player) else {
                                return Ok(0);
                            };
                            let task =
                                server.schedule(Owner::Entity(world, player), 0, 0, move |ctx| {
                                    let _ = treat(ctx, world, player);
                                });
                            Ok(i32::from(task.is_ok()))
                        }),
                )
                .then(Command::literal("pig").then(
                    Command::argument("pos", ArgType::Vec3).executes(
                        |_: &mut Hello, ctx, invocation| {
                            let (Some(world), Some(pos)) =
                                (invocation.world(), invocation.vec3("pos"))
                            else {
                                return Ok(0);
                            };
                            let task =
                                ctx.server()
                                    .schedule(Owner::World(world), 0, 0, move |ctx| {
                                        if let Ok(pig) = ctx.spawn_entity(
                                            world,
                                            "minecraft:pig",
                                            pos,
                                            "{Glowing:1b,CustomName:\"Hello\"}",
                                        ) {
                                            let _ = home_pig(ctx, world, pig, pos);
                                        }
                                    });
                            Ok(i32::from(task.is_ok()))
                        },
                    ),
                ))
                .then(
                    Command::literal("menu").executes(|_: &mut Hello, ctx, invocation| {
                        let vanillite_plugin::Sender::Player(player) = invocation.sender() else {
                            return Ok(0);
                        };
                        let server = ctx.server();
                        let Some(world) = server.player_world(player) else {
                            return Ok(0);
                        };
                        let task =
                            server.schedule(Owner::Entity(world, player), 0, 0, move |ctx| {
                                let _ = open_menu(ctx, world, player);
                            });
                        Ok(i32::from(task.is_ok()))
                    }),
                )
                .then(
                    Command::literal("lobby").executes(|_: &mut Hello, ctx, invocation| {
                        let vanillite_plugin::Sender::Player(player) = invocation.sender() else {
                            return Ok(0);
                        };
                        Ok(i32::from(to_lobby(ctx.server(), player)))
                    }),
                )
                .then(
                    Command::literal("look").executes(|_: &mut Hello, ctx, invocation| {
                        let vanillite_plugin::Sender::Player(player) = invocation.sender() else {
                            return Ok(0);
                        };
                        let server = ctx.server();
                        let Some(world) = server.player_world(player) else {
                            return Ok(0);
                        };
                        let task =
                            server.schedule(Owner::Entity(world, player), 0, 0, move |ctx| {
                                let _ = look(ctx, world, player);
                            });
                        Ok(i32::from(task.is_ok()))
                    }),
                )
                .then(
                    Command::literal("pad")
                        .requires_permission("hello.pad")
                        .executes(|_: &mut Hello, ctx, invocation| {
                            let vanillite_plugin::Sender::Player(player) = invocation.sender()
                            else {
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
        registrar.on_world_load(priority::MONITOR, |_, _, event| {
            vanillite_plugin::info!("World {} is loaded", event.key());
        });
        vanillite_plugin::info!("Hello is loaded");
        Ok(Hello { greetings: 0 })
    }

    fn enable(&mut self, ctx: &mut Ctx<'_>) {
        let server = ctx.server();
        let worlds = server.worlds().len();
        vanillite_plugin::info!("Hello is enabled ({worlds} worlds)");
        // Both are saved with the world: they exist after the first start.
        if server.score("hello_joins", "#check").is_none() {
            let _ = server.add_objective("hello_joins", "dummy", Text::colored("Joins", "gold"));
        }
        let _ = server.display_objective(
            vanillite_plugin::abi::display_slot::LIST,
            Some("hello_joins"),
        );
        if let Ok(bar) = server.create_boss_bar("welcome", Text::colored("Welcome!", "green")) {
            let _ = bar.set_color(vanillite_plugin::abi::bossbar_color::GREEN);
        }
        if let Some(motd) = server.config_string("messages.motd") {
            let _ = server.set_motd(motd);
        }
        // Every five minutes off the tick: how many players are online (a real plugin would
        // write statistics to a database here).
        let _ = server.run_async_timer(6000, 6000, move || {
            let online = server.online_players().len();
            vanillite_plugin::debug!("{online} players online");
        });
    }

    fn disable(&mut self, ctx: &mut Ctx<'_>) {
        vanillite_plugin::info!("Hello is disabled after {} greetings", self.greetings);
        let server = ctx.server();
        if server
            .set_config("stats.greetings", &self.greetings.to_string())
            .is_ok()
        {
            let _ = server.save_config();
        }
    }

    fn save_state(&mut self) -> Vec<u8> {
        self.greetings.to_le_bytes().to_vec()
    }

    fn restore_state(&mut self, state: &[u8]) {
        if let Ok(bytes) = <[u8; 8]>::try_from(state) {
            self.greetings = u64::from_le_bytes(bytes);
            GREETINGS.store(self.greetings, std::sync::atomic::Ordering::Relaxed);
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
    GREETINGS.store(hello.greetings, std::sync::atomic::Ordering::Relaxed);
    // Other plugins may listen (this plugin's own handlers are never called for it).
    let _ = ctx.fire_event("hello:greeted", name.as_bytes());
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

/// `/hello treat`: a full food bar, a short speed boost, a cookie and hearts, on the player's
/// owner.
fn treat(ctx: &mut Ctx<'_>, world: World, player: Uuid) -> Result<(), Error> {
    use vanillite_plugin::abi::player_prop;
    ctx.set_player_prop(world, player, player_prop::FOOD, 20.0)?;
    ctx.set_player_prop(world, player, player_prop::SATURATION, 5.0)?;
    ctx.add_effect(world, player, &Effect::new("minecraft:speed", 200, 0))?;
    ctx.give_item(world, player, "{id:\"minecraft:cookie\",count:1}")?;
    let at = ctx.location(world, player)?;
    let mut hearts = Particle::new("minecraft:heart", at.pos, 5);
    hearts.pos.y += 2.0;
    hearts.delta = Vec3 {
        x: 0.5,
        y: 0.3,
        z: 0.5,
    };
    ctx.server().spawn_particle(player, &hearts)
}

/// `/hello menu`: a one-row plugin menu with a named diamond marked as the button.
fn open_menu(ctx: &mut Ctx<'_>, world: World, player: Uuid) -> Result<(), Error> {
    let server = ctx.server();
    let item = "{id:\"minecraft:diamond\",count:1}";
    let item = server.item_with_name(item, &Text::colored("Take a cookie", "gold"))?;
    let item = server.item_with_data(&item, "button", &DataValue::Byte(1))?;
    // A stack with its slot: `{Slot:4b,` and the rest of the item compound.
    let slotted = format!("[{{Slot:4b,{}]", item.strip_prefix('{').unwrap_or(&item));
    ctx.open_menu(
        world,
        player,
        "minecraft:generic_9x1",
        Text::colored("Hello menu", "dark_green"),
        &slotted,
    )?;
    Ok(())
}

/// `/hello look`: what the player looks at within 50 blocks.
fn look(ctx: &mut Ctx<'_>, world: World, player: Uuid) -> Result<(), Error> {
    let at = ctx.location(world, player)?;
    let (yaw, pitch) = (
        f64::from(at.yaw).to_radians(),
        f64::from(at.pitch).to_radians(),
    );
    let eye = Vec3 {
        x: at.pos.x,
        y: at.pos.y + 1.62,
        z: at.pos.z,
    };
    let to = Vec3 {
        x: eye.x - yaw.sin() * pitch.cos() * 50.0,
        y: eye.y - pitch.sin() * 50.0,
        z: eye.z + yaw.cos() * pitch.cos() * 50.0,
    };
    let flags = abi::raytrace_flag::BLOCKS | abi::raytrace_flag::ENTITIES;
    let text = match ctx.raytrace(world, eye, to, flags, Some(player))? {
        Some(RayHit::Block(_, pos, _)) => {
            format!(
                "{} at {} {} {}",
                ctx.block(world, pos)?,
                pos.x,
                pos.y,
                pos.z
            )
        }
        Some(RayHit::Entity(_, entity)) => ctx.entity_type(world, entity)?,
        None => "nothing".to_owned(),
    };
    ctx.server()
        .send_message(player, format!("You look at {text}"))
}

/// `/hello pig`: the pig gets a name, more speed and a goal that walks it home; a task on the
/// pig tells the players when it is gone.
fn home_pig(ctx: &mut Ctx<'_>, world: World, pig: Uuid, home: Vec3) -> Result<(), Error> {
    ctx.set_entity_name(world, pig, Some(Text::colored("Homebody", "light_purple")))?;
    ctx.add_attribute_modifier(
        world,
        pig,
        "minecraft:movement_speed",
        "homebody",
        0.5,
        abi::attribute_op::ADD_MULTIPLIED_BASE,
    )?;
    ctx.add_goal(
        world,
        pig,
        1,
        abi::goal_flag::MOVE,
        false,
        HomeGoal { home },
    )?;
    ctx.server().schedule_entity(
        world,
        pig,
        0,
        200,
        |_| {},
        |ctx| {
            let server = ctx.server();
            for player in server.online_players() {
                let _ = server.send_message(player, "The homebody pig is gone");
            }
        },
    )?;
    Ok(())
}

/// Walks the mob back when it is more than four blocks from home.
struct HomeGoal {
    home: Vec3,
}

impl HomeGoal {
    fn far(&self, mob: &GoalMob) -> bool {
        let (dx, dz) = (mob.pos.x - self.home.x, mob.pos.z - self.home.z);
        dx * dx + dz * dz > 16.0
    }
}

impl Goal for HomeGoal {
    fn can_use(&mut self, mob: &GoalMob) -> bool {
        self.far(mob)
    }

    fn can_continue(&mut self, mob: &GoalMob) -> bool {
        mob.navigating || self.far(mob)
    }

    fn tick(&mut self, _: &GoalMob) -> GoalAction {
        GoalAction {
            move_to: Some((self.home, 1.0)),
            look_at: Some(self.home),
            ..GoalAction::default()
        }
    }
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

/// The extra world of `/hello lobby`.
const LOBBY: &str = "hello_lobby";

/// Moves a player onto the flat lobby world, which is created on first use; `false` while it
/// is still loading.
fn to_lobby(server: Server, player: Uuid) -> bool {
    let creator = vanillite_plugin::WorldCreator::new(LOBBY).generator("flat");
    let Ok(world) = server.create_world(&creator) else {
        return false;
    };
    if !server.world_details(world).is_some_and(|d| d.loaded) {
        let _ = server.send_message(player, "The lobby is being created, try again in a moment");
        return false;
    }
    // The classic flat layers end at y -61.
    let to = Location {
        world,
        pos: Vec3 {
            x: 0.5,
            y: -60.0,
            z: 0.5,
        },
        yaw: 0.0,
        pitch: 0.0,
    };
    server.move_player(player, to).is_ok()
}
