//! Builds the example plugin (`examples/hello-plugin`) as a real `cdylib` and runs it through
//! the C ABI: validation and `load`, its command in a Brigadier tree with suggestions, the join
//! event, the `chunk-generator` slot and a development reload that keeps its state.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use vanillite_base::ChunkPos;
use vanillite_chunk::slots::{
    ChunkGeneratorProvider, ChunkStatus, FlatGeneratorProvider, Neighbours, WorldInfo,
};
use vanillite_command::Dispatcher;
use vanillite_data::block::BlockId;
use vanillite_plugin_host::{
    Arg, ArgValue, PluginHost, Sender, ServerAccess, Settings, Text, WorldAccess, loader,
};

const PLAYER: u128 = 0x069a_79f4_44e9_4726_a5be_fca9_0e38_aaf5;

#[derive(Default)]
struct FakeServer {
    messages: Mutex<Vec<(u128, String)>>,
}

impl ServerAccess for FakeServer {
    fn online_players(&self) -> Vec<u128> {
        vec![PLAYER]
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

struct FakeWorld;

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
    fn player_position(&self, _: u128) -> Option<[f64; 3]> {
        None
    }
    fn has_entity(&self, entity: u128) -> bool {
        entity == PLAYER
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
        .replace("enabled = false", "enabled = true");
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
    host.player_join(&mut FakeWorld, PLAYER, "Alex");
    let sender = Sender {
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
    let _ = std::fs::remove_dir_all(&dir);
}
