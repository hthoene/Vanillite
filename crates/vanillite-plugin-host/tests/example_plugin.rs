//! Builds the example plugin (`examples/hello-plugin`) as a real `cdylib` and runs it through
//! the C ABI: validation and `load`, its command in a Brigadier tree with suggestions, the join
//! event with persistent data and the action bar, login, interaction and death events, a
//! periodic background task, the `chunk-generator` slot and a development reload that keeps its
//! state. ABI 1.5: the join message, scoreboard, boss bar, sound, particles and MOTD, player
//! properties, effects, items and entity spawning from tasks, `Vec3` arguments, plugin
//! messages, advancements, plugin events with `ignore_cancelled`, services, permission
//! defaults and saving the config.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vanillite_base::ChunkPos;
use vanillite_chunk::slots::{
    ChunkGeneratorProvider, ChunkStatus, FlatGeneratorProvider, Neighbours, WorldInfo,
};
use vanillite_command::Dispatcher;
use vanillite_data::block::BlockId;
use vanillite_nbt::{Compound, Tag};
use vanillite_plugin_host::{
    Arg, ArgValue, BlockBox, BossBarOp, InteractTarget, Location, NewText, Particles, PluginHost,
    ScoreReply, ScoreboardOp, Sender, ServerAccess, SetMode, Settings, Sound, Text, WorldAccess,
    loader,
};

const PLAYER: u128 = 0x069a_79f4_44e9_4726_a5be_fca9_0e38_aaf5;

#[derive(Default)]
struct FakeServer {
    messages: Mutex<Vec<(u128, String)>>,
    action_bars: Mutex<Vec<String>>,
    online_calls: AtomicUsize,
    /// Everything else the plugin did, as debug text.
    calls: Mutex<Vec<String>>,
}

impl FakeServer {
    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }

    fn called(&self, prefix: &str) -> Vec<String> {
        let calls = self.calls.lock().unwrap();
        calls
            .iter()
            .filter(|c| c.starts_with(prefix))
            .cloned()
            .collect()
    }
}

impl ServerAccess for FakeServer {
    fn play_sound(&self, _: u128, sound: &Sound<'_>) -> bool {
        self.record(format!("sound {}", sound.sound));
        true
    }
    fn spawn_particle(&self, _: u128, particles: &Particles<'_>) -> Result<(), i32> {
        self.record(format!(
            "particle {} {}",
            particles.particle, particles.count
        ));
        Ok(())
    }
    fn send_plugin_message(&self, _: u128, channel: &str, data: &[u8]) -> bool {
        self.record(format!(
            "message {channel} {}",
            String::from_utf8_lossy(data)
        ));
        true
    }
    fn scoreboard(&self, op: ScoreboardOp<'_>) -> Result<ScoreReply, i32> {
        self.record(format!("scoreboard {op:?}"));
        match op {
            ScoreboardOp::GetScore { .. } => Err(vanillite_abi::status::NOT_FOUND),
            _ => Ok(ScoreReply::Done),
        }
    }
    fn boss_bar(&self, op: BossBarOp<'_>) -> Result<(), i32> {
        self.record(format!("bossbar {op:?}"));
        Ok(())
    }
    fn set_motd(&self, motd: Text<'_>) -> bool {
        self.record(format!("motd {motd:?}"));
        true
    }
    fn online_players(&self) -> Vec<u128> {
        self.online_calls.fetch_add(1, Ordering::Relaxed);
        vec![PLAYER]
    }
    fn send_action_bar(&self, _: u128, text: Text<'_>) -> bool {
        let (Text::Plain(s) | Text::Json(s)) = text;
        self.action_bars.lock().unwrap().push(s.to_owned());
        true
    }
    fn player_name(&self, player: u128) -> Option<String> {
        (player == PLAYER).then(|| "Alex".to_owned())
    }
    fn player_world(&self, player: u128) -> Option<u32> {
        (player == PLAYER).then_some(0)
    }
    fn send_message(&self, player: u128, text: Text<'_>) -> bool {
        let text = match text {
            Text::Plain(s) | Text::Json(s) => s.to_owned(),
        };
        self.messages.lock().unwrap().push((player, text));
        true
    }
    fn kick(&self, _: u128, _: Text<'_>) -> bool {
        true
    }
    fn set_tab_list(&self, _: u128, _: Text<'_>, _: Text<'_>) -> bool {
        true
    }
    fn broadcast(&self, _: Text<'_>) {}
    fn is_operator(&self, _: u128) -> bool {
        false
    }
    fn world_name(&self, world: u32) -> Option<String> {
        ["minecraft:overworld", "minecraft:the_nether"]
            .get(world as usize)
            .map(|s| (*s).to_owned())
    }
    fn block_state_id(&self, state: &str) -> Option<u32> {
        let name = state.split('[').next()?;
        Some(u32::from(BlockId::by_name(name)?.default_state().0))
    }
    fn block_state_name(&self, state: u32) -> Option<String> {
        let state = vanillite_data::block::BlockState(u16::try_from(state).ok()?);
        state.is_valid().then(|| state.block().name().to_owned())
    }
    fn rule(&self, _: &str) -> Option<String> {
        None
    }
    fn run_command(&self, _: &str) {}
    fn spawn_background(&self, job: Box<dyn FnOnce() + Send>) {
        job();
    }
    fn shutdown(&self, reason: &str) {
        panic!("unexpected shutdown: {reason}");
    }
}

/// A world holding the player's persistent data, recording what plugins change.
#[derive(Default)]
struct FakeWorld {
    data: Compound,
    changes: Vec<String>,
}

impl WorldAccess for FakeWorld {
    fn id(&self) -> u32 {
        0
    }
    fn block(&self, _: [i32; 3]) -> Option<String> {
        None
    }
    fn set_block(&mut self, _: [i32; 3], _: &str) -> Result<(), i32> {
        Ok(())
    }
    fn player_position(&self, player: u128) -> Option<[f64; 3]> {
        (player == PLAYER).then_some([1.0, 64.0, 2.0])
    }
    fn entity_location(&self, entity: u128) -> Result<Location, i32> {
        if entity != PLAYER {
            return Err(vanillite_abi::status::NOT_FOUND);
        }
        Ok(Location {
            world: 0,
            pos: [1.0, 64.0, 2.0],
            yaw: 0.0,
            pitch: 0.0,
        })
    }
    fn player_set(&mut self, _: u128, prop: u32, value: f64) -> Result<(), i32> {
        self.changes.push(format!("prop {prop} {value}"));
        Ok(())
    }
    fn add_effect(
        &mut self,
        _: u128,
        effect: &str,
        duration: i32,
        _: i32,
        _: u32,
    ) -> Result<(), i32> {
        self.changes.push(format!("effect {effect} {duration}"));
        Ok(())
    }
    fn give_item(&mut self, _: u128, item: &Compound) -> Result<(), i32> {
        self.changes.push(format!(
            "give {}",
            vanillite_nbt::snbt::to_string(&Tag::Compound(item.clone()))
        ));
        Ok(())
    }
    fn spawn_entity(&mut self, kind: &str, pos: [f64; 3], nbt: &Compound) -> Result<u128, i32> {
        let glowing = nbt.get("Glowing").is_some();
        self.changes
            .push(format!("spawn {kind} {pos:?} glowing={glowing}"));
        Ok(42)
    }
    fn has_entity(&self, entity: u128) -> bool {
        entity == PLAYER
    }
    fn block_id(&self, _: [i32; 3]) -> Option<u32> {
        None
    }
    fn set_block_id(&mut self, _: [i32; 3], _: u32, _: SetMode) -> Result<bool, i32> {
        Ok(false)
    }
    fn set_blocks(&mut self, _: BlockBox, _: &[u32], _: SetMode) -> Result<usize, i32> {
        Ok(0)
    }
    fn entity_data(&self, entity: u128) -> Result<Option<&Compound>, i32> {
        if entity != PLAYER {
            return Err(vanillite_abi::status::NOT_FOUND);
        }
        Ok((!self.data.is_empty()).then_some(&self.data))
    }
    fn entity_data_mut(&mut self, entity: u128) -> Result<&mut Compound, i32> {
        if entity != PLAYER {
            return Err(vanillite_abi::status::NOT_FOUND);
        }
        Ok(&mut self.data)
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Builds the example in its own target directory (the outer cargo holds the workspace's).
fn build_example() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| root().join("target"), PathBuf::from)
        .join("plugin-tests");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(root())
        .args([
            "build",
            "-q",
            "-p",
            "vanillite-example-hello",
            "--target-dir",
        ])
        .arg(&target)
        .status()
        .expect("run cargo");
    assert!(status.success(), "building the example plugin failed");
    target
        .join("debug")
        .join(loader::library_file("vanillite_example_hello"))
}

fn world(dimension: &str) -> WorldInfo {
    WorldInfo {
        dimension: dimension.to_owned(),
        dir: PathBuf::new(),
        min_y: -64,
        height: 384,
        seed: 0,
        has_sky_light: true,
        biomes: Arc::from(vec!["minecraft:plains".to_owned()]),
        default_biome: 0,
        air: 0,
    }
}

/// Chunk facts: only air matters for writing blocks.
fn install_facts() {
    use vanillite_chunk::facts;
    let flags: Vec<u8> = (0..vanillite_data::block::STATE_COUNT)
        .map(|i| {
            if i == 0 {
                facts::AIR
            } else {
                facts::BLOCKS_MOTION
            }
        })
        .collect();
    facts::install(facts::Tables {
        block_flags: Box::leak(flags.into_boxed_slice()),
        biome_count: 1,
    });
}

#[test]
fn example_plugin_runs_through_the_abi() {
    install_facts();
    let library = build_example();
    let dir = std::env::temp_dir().join(format!("vanillite-plugin-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let package = dir.join("plugins/hello");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::copy(
        root().join("examples/hello-plugin/plugin.toml"),
        package.join("plugin.toml"),
    )
    .unwrap();
    std::fs::copy(&library, package.join(library.file_name().unwrap())).unwrap();
    let config = std::fs::read_to_string(root().join("examples/hello-plugin/config.toml"))
        .unwrap()
        .replace("enabled = false", "enabled = true")
        .replace("refuse = \"\"", "refuse = \"Herobrine\"");
    std::fs::write(package.join("config.toml"), config).unwrap();

    let host = PluginHost::load(
        &dir.join("plugins"),
        Settings {
            enabled: true,
            safe_mode_after_crash: true,
            development: true,
            pins: String::new(),
            data_version: 1,
        },
    );
    assert_eq!(host.plugins(), ["hello"]);
    let server = Arc::new(FakeServer::default());
    host.set_server(Arc::clone(&server) as Arc<dyn ServerAccess>);

    let mut dispatcher = Dispatcher::<u32>::new();
    host.register_commands(&mut dispatcher, |n| n);
    let hello = dispatcher.find(&["hello"]).expect("/hello is in the tree");
    let hello_command = dispatcher.node(hello).unwrap().command.unwrap();
    let name = dispatcher.child(hello, "name").unwrap();
    let name_node = dispatcher.node(name).unwrap();
    let (name_command, provider) = (name_node.command.unwrap(), name_node.suggests.unwrap());

    host.enable_all();
    // Enabling creates the objective (shown in the player list), the boss bar and the MOTD.
    assert_eq!(
        server.called("scoreboard"),
        [
            "scoreboard GetScore { objective: \"hello_joins\", holder: \"#check\" }",
            "scoreboard AddObjective { name: \"hello_joins\", criteria: \"dummy\", display: Json(\"{\\\"text\\\":\\\"Joins\\\",\\\"color\\\":\\\"gold\\\"}\") }",
            "scoreboard Display { slot: 0, objective: Some(\"hello_joins\") }",
        ]
    );
    assert_eq!(
        server.called("bossbar").len(),
        2,
        "{:?}",
        server.called("bossbar")
    );
    assert!(server.called("bossbar")[0].contains("hello:welcome"));
    assert_eq!(
        server.called("motd"),
        ["motd Plain(\"A Vanillite server with Hello\")"]
    );
    let mut overworld = FakeWorld::default();
    let joined = host.player_join(&mut overworld, PLAYER, "Alex", "\"Alex joined the game\"");
    assert!(
        matches!(&joined, Some(NewText::Json(m)) if m.contains("Alex joined the fun")),
        "{joined:?}"
    );
    assert!(
        server
            .called("scoreboard SetScore")
            .first()
            .is_some_and(|c| c.contains("value: 1")),
        "{:?}",
        server.called("scoreboard")
    );
    assert!(server.called("bossbar Player").len() == 1);
    assert_eq!(
        server.called("sound"),
        ["sound minecraft:entity.player.levelup"]
    );
    let sender = Sender {
        kind: vanillite_abi::sender::CONSOLE,
        player: Some(PLAYER),
        world: Some(0),
        pos: [0.0; 3],
    };
    assert_eq!(
        host.execute_command(hello_command, &sender, "hello", &[], None),
        Some(1)
    );
    let steve = [Arg {
        name: "name".into(),
        value: ArgValue::Text("Steve".into()),
    }];
    assert_eq!(
        host.execute_command(name_command, &sender, "hello Steve", &steve, None),
        Some(2)
    );
    assert_eq!(host.suggest(provider, &sender, &[], ""), ["Alex"]);
    let messages: Vec<String> = server
        .messages
        .lock()
        .unwrap()
        .iter()
        .map(|(_, m)| m.clone())
        .collect();
    assert!(
        messages[0].contains("Welcome to Vanillite, Alex!"),
        "{messages:?}"
    );
    assert!(messages[1].contains("Hello, Alex!"), "{messages:?}");
    assert!(messages[2].contains("Hello, Steve!"), "{messages:?}");

    // Persistent data: the join counter under the plugin's namespace, and the action bar.
    host.player_join(&mut overworld, PLAYER, "Alex", "");
    assert_eq!(overworld.data.get("hello:joins"), Some(&Tag::Int(2)));
    assert_eq!(
        *server.action_bars.lock().unwrap(),
        ["Visit #1", "Visit #2"]
    );

    // Login: the configured name is refused with its message, others pass untouched.
    let refused = host
        .player_login(
            7,
            "Herobrine",
            "127.0.0.1",
            vanillite_abi::login_reason::ALLOWED,
            "",
        )
        .expect("the example listens to logins");
    assert!(refused.refused);
    assert!(
        matches!(&refused.message, Some(NewText::Json(m)) if m.contains("This name is reserved.")),
        "{refused:?}"
    );
    let full = host
        .player_login(
            8,
            "Steve",
            "127.0.0.1",
            vanillite_abi::login_reason::FULL,
            "\"full\"",
        )
        .unwrap();
    assert!(full.refused && full.message.is_none());

    // Protection: fire starting is cancelled, other items are not; deaths keep the inventory.
    let block = InteractTarget {
        pos: [0, 64, 0],
        face: 1,
        state: "minecraft:stone",
    };
    assert!(!host.player_interact(0, PLAYER, 0, "minecraft:flint_and_steel", Some(block)));
    assert!(host.player_interact(0, PLAYER, 0, "minecraft:stick", None));
    let death = host
        .player_death(0, PLAYER, "minecraft:fall", None, "\"Alex fell\"", false)
        .expect("the example listens to deaths");
    assert!(death.keep_inventory);
    assert!(
        matches!(&death.message, Some(NewText::Json(m)) if m.contains("Alex died and kept everything")),
        "{death:?}"
    );

    // `/hello treat` and `/hello pig <pos>` change the world from tasks on its owner.
    let treat = dispatcher.child(hello, "treat").unwrap();
    let treat = dispatcher.node(treat).unwrap().command.unwrap();
    assert_eq!(
        host.execute_command(treat, &sender, "hello treat", &[], None),
        Some(1)
    );
    let pig = dispatcher.child(hello, "pig").unwrap();
    let pos = dispatcher.child(pig, "pos").unwrap();
    let pig = dispatcher.node(pos).unwrap().command.unwrap();
    let at = [Arg {
        name: "pos".into(),
        value: ArgValue::Vec3([3.5, 65.0, 4.5]),
    }];
    assert_eq!(
        host.execute_command(pig, &sender, "hello pig 3.5 65 4.5", &at, None),
        Some(1)
    );
    host.run_world_tasks(&mut overworld, 0);
    let food = vanillite_abi::player_prop::FOOD;
    let saturation = vanillite_abi::player_prop::SATURATION;
    assert_eq!(
        overworld.changes,
        [
            format!("prop {food} 20"),
            format!("prop {saturation} 5"),
            "effect minecraft:speed 200".to_owned(),
            "give {id:\"minecraft:cookie\",count:1}".to_owned(),
            "spawn minecraft:pig [3.5, 65.0, 4.5] glowing=true".to_owned(),
        ]
    );
    assert_eq!(server.called("particle"), ["particle minecraft:heart 5"]);

    // Plugin messages on the registered channel are answered; others do not reach it.
    host.plugin_message(0, PLAYER, "hello:ping", b"abc");
    host.plugin_message(0, PLAYER, "other:ping", b"abc");
    assert_eq!(server.called("message"), ["message hello:pong abc"]);

    // Advancements and another plugin's event (skipped while cancelled).
    host.advancement_done(0, PLAYER, "minecraft:story/root");
    assert_eq!(
        host.fire_event("economy:payment", "economy", b"5 coins", false),
        Some(false)
    );
    assert_eq!(
        host.fire_event("economy:payment", "economy", b"6 coins", true),
        Some(true)
    );
    assert_eq!(
        host.fire_event("nobody:listens", "economy", b"", false),
        None
    );
    assert_eq!(
        server.action_bars.lock().unwrap()[3..],
        ["Well done: minecraft:story/root", "Payment: 5 coins"]
    );

    // The service other plugins call: two greetings so far.
    #[repr(C)]
    struct GreeterApi {
        size: u32,
        greetings: extern "C" fn() -> u64,
    }
    let (version, table) = host
        .service("hello:greeter")
        .expect("the example offers it");
    assert_eq!(version, 1);
    // SAFETY: the example documents this layout; its library stays loaded.
    let greeter = unsafe { &*table.cast::<GreeterApi>() };
    assert!(greeter.size as usize >= size_of::<GreeterApi>());
    assert_eq!((greeter.greetings)(), 2);

    // Permission defaults without a provider.
    assert!(!host.has_permission(PLAYER, "hello.pad", false));
    assert!(host.has_permission(PLAYER, "hello.pad", true));
    assert!(host.has_permission(PLAYER, "hello.treat", false));
    assert!(!host.has_permission(PLAYER, "other.node", false));

    // The periodic background task runs every 6000 ticks.
    let before = server.online_calls.load(Ordering::Relaxed);
    host.run_global_tasks(5999);
    assert_eq!(server.online_calls.load(Ordering::Relaxed), before);
    host.run_global_tasks(6000);
    host.run_global_tasks(12000);
    assert_eq!(server.online_calls.load(Ordering::Relaxed), before + 2);

    let (id, provider) = host
        .take_chunk_generator(|| Box::new(FlatGeneratorProvider))
        .expect("the example fills the chunk-generator slot");
    assert_eq!(id, "hello");
    let overworld = world("minecraft:overworld");
    let generator = provider.create(&overworld).unwrap();
    let mut chunk = generator.new_chunk(ChunkPos::new(3, -2));
    generator.generate(
        ChunkStatus::Terrain,
        std::slice::from_mut(&mut chunk),
        &mut Neighbours::new(&mut []),
    );
    let id = |name: &str| BlockId::by_name(name).unwrap().default_state().0;
    assert_eq!(chunk.chunk.block(5, -64, 7), Some(id("minecraft:bedrock")));
    assert_eq!(chunk.chunk.block(5, 0, 7), Some(id("minecraft:stone")));
    assert_eq!(
        chunk.chunk.block(5, 64, 7),
        Some(id("minecraft:grass_block"))
    );
    assert_eq!(chunk.chunk.block(5, 65, 7), Some(0));
    // Other dimensions keep the built-in generator.
    assert!(provider.create(&world("minecraft:the_nether")).is_ok());

    // A development reload keeps the state (two greetings so far).
    host.reload("hello").expect("reload");
    assert_eq!(
        host.execute_command(hello_command, &sender, "hello", &[], None),
        Some(3)
    );
    host.disable_all();
    // `disable` saved the greeting count into the config.
    let saved = std::fs::read_to_string(package.join("config.toml")).unwrap();
    assert!(saved.contains("greetings = 3"), "{saved}");
    let _ = std::fs::remove_dir_all(&dir);
}
