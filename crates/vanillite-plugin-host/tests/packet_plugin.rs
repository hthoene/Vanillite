//! Builds the packet example plugin (`examples/packet-plugin`) as a real `cdylib` and drives
//! its packet listeners through the C ABI and the packet tap, the way the network layer does:
//! the bit mask follows the listeners (also added and removed at run time), a listener per
//! receiver copies the packet for that receiver only, a listener per broadcast runs once and
//! keeps one shared buffer, a serverbound packet is cancelled, and packets the plugin sends
//! reach the server.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use vanillite_plugin_host::{
    BlockBox, PluginHost, Sender, ServerAccess, SetMode, Settings, Text, WorldAccess, loader,
};
use vanillite_protocol::frame::encode_body;
use vanillite_protocol::packets::play::clientbound::{
    KeepAlive, SetPlayerTeam, SystemChat, TeamAction, TeamParameters,
};
use vanillite_protocol::tap::{Peer, Verdict};
use vanillite_protocol::text::Component;
use vanillite_protocol::{Direction, Packet, Reader, SharedPacket, State};

const ALEX: u128 = 0x069a_79f4_44e9_4726_a5be_fca9_0e38_aaf5;
const STEVE: u128 = 0x8667_ba71_b85a_4004_af54_457a_9734_eed7;

#[derive(Default)]
struct FakeServer {
    messages: Mutex<Vec<String>>,
    /// Packets plugins sent: receiver, body, past the listeners.
    packets: Mutex<Vec<(u128, Vec<u8>, bool)>>,
}

impl ServerAccess for FakeServer {
    fn online_players(&self) -> Vec<u128> {
        vec![ALEX, STEVE]
    }
    fn player_name(&self, player: u128) -> Option<String> {
        match player {
            ALEX => Some("Alex".to_owned()),
            STEVE => Some("Steve".to_owned()),
            _ => None,
        }
    }
    fn player_world(&self, _: u128) -> Option<u32> {
        Some(0)
    }
    fn send_message(&self, _: u128, text: Text<'_>) -> bool {
        let (Text::Plain(s) | Text::Json(s)) = text;
        self.messages.lock().unwrap().push(s.to_owned());
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
    fn world_name(&self, _: u32) -> Option<String> {
        Some("minecraft:overworld".to_owned())
    }
    fn block_state_id(&self, _: &str) -> Option<u32> {
        None
    }
    fn block_state_name(&self, _: u32) -> Option<String> {
        None
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
    fn send_packet(&self, player: u128, body: Vec<u8>, skip_listeners: bool) -> bool {
        self.packets
            .lock()
            .unwrap()
            .push((player, body, skip_listeners));
        true
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
    fn has_entity(&self, _: u128) -> bool {
        false
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
            "vanillite-example-packets",
            "--target-dir",
        ])
        .arg(&target)
        .status()
        .expect("run cargo");
    assert!(status.success(), "building the example plugin failed");
    target
        .join("debug")
        .join(loader::library_file("vanillite_example_packets"))
}

fn decode<P: Packet>(body: &[u8]) -> P {
    let mut reader = Reader::new(body);
    assert_eq!(reader.varint().unwrap(), P::ID, "packet id");
    P::decode(&mut reader).expect("the packet decodes")
}

fn peer(player: u128) -> Peer {
    Peer {
        connection: 7,
        player,
    }
}

fn team(visibility: i32) -> SetPlayerTeam {
    SetPlayerTeam {
        name: "red".to_owned(),
        action: TeamAction::Add(
            Box::new(TeamParameters {
                display_name: Component::text("Red"),
                prefix: Component::empty(),
                suffix: Component::empty(),
                nametag_visibility: visibility,
                collision_rule: 0,
                color: Some(12),
                options: 0,
            }),
            vec!["Steve".to_owned()],
        ),
    }
}

#[test]
fn packet_plugin_listens_rewrites_and_cancels() {
    let library = build_example();
    let dir = std::env::temp_dir().join(format!("vanillite-packet-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let package = dir.join("plugins/packets");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::copy(
        root().join("examples/packet-plugin/plugin.toml"),
        package.join("plugin.toml"),
    )
    .unwrap();
    std::fs::copy(&library, package.join(library.file_name().unwrap())).unwrap();
    let config = std::fs::read_to_string(root().join("examples/packet-plugin/config.toml"))
        .unwrap()
        .replace("prefix = \"\"", "prefix = \"[Server] \"")
        .replace("read-only = false", "read-only = true");
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
    assert_eq!(host.plugins(), ["packets"]);
    let server = Arc::new(FakeServer::default());
    host.set_server(Arc::clone(&server) as Arc<dyn ServerAccess>);
    let mut dispatcher = vanillite_command::Dispatcher::<u32>::new();
    host.register_commands(&mut dispatcher, |n| n);
    let command = |name: &str| {
        let root = dispatcher.find(&["nametags"]).expect("/nametags exists");
        let node = dispatcher.child(root, name).unwrap();
        dispatcher.node(node).unwrap().command.unwrap()
    };
    host.enable_all();
    let tap = Arc::clone(host.packet_tap());
    let sign_update = vanillite_data::packets::play::serverbound::SIGN_UPDATE;
    let chat = vanillite_data::packets::play::serverbound::CHAT;

    // The mask holds exactly the packet types with a listener.
    assert!(tap.active(Direction::Clientbound) && tap.active(Direction::Serverbound));
    assert!(tap.wants(Direction::Clientbound, State::Play, SystemChat::ID));
    assert!(tap.wants(Direction::Serverbound, State::Play, sign_update));
    assert!(!tap.wants(Direction::Serverbound, State::Play, chat));
    assert!(!tap.wants(Direction::Clientbound, State::Play, KeepAlive::ID));
    assert!(!tap.wants(Direction::Clientbound, State::Configuration, SystemChat::ID));
    // Nobody hides name tags yet: team packets have no listener and cost nothing.
    assert!(!tap.wants(Direction::Clientbound, State::Play, SetPlayerTeam::ID));
    let red = SharedPacket::of(&team(0));
    assert!(matches!(tap.shared(State::Play, &red), Verdict::Pass));
    assert_eq!(
        tap.clientbound(peer(ALEX), State::Play, red.body(), false),
        Verdict::Pass
    );

    // `/nametags off`: the listener is added at run time, and Alex alone gets a team with
    // everyone else in it.
    let alex = Sender {
        kind: vanillite_abi::sender::PLAYER,
        player: Some(ALEX),
        world: Some(0),
        pos: [0.0; 3],
    };
    assert_eq!(
        host.execute_command(command("off"), &alex, "nametags off", &[], None),
        Some(1)
    );
    assert!(tap.wants(Direction::Clientbound, State::Play, SetPlayerTeam::ID));
    {
        let packets = server.packets.lock().unwrap();
        let [(to, body, skip)] = packets.as_slice() else {
            panic!("one packet for Alex, got {}", packets.len());
        };
        assert_eq!((*to, *skip), (ALEX, false));
        let sent: SetPlayerTeam = decode(body);
        let TeamAction::Add(parameters, members) = sent.action else {
            panic!("a new team");
        };
        assert_eq!(parameters.nametag_visibility, 1);
        assert_eq!(members, ["Steve"]);
    }

    // Per receiver: Alex gets a copy without name tags, Steve the shared buffer. A new
    // shared packet, as every broadcast is.
    let red = SharedPacket::of(&team(0));
    assert!(matches!(tap.shared(State::Play, &red), Verdict::Pass));
    let Verdict::Replace(for_alex) = tap.clientbound(peer(ALEX), State::Play, red.body(), false)
    else {
        panic!("Alex's team packet is rewritten");
    };
    let mut expected = team(1);
    assert_eq!(decode::<SetPlayerTeam>(&for_alex), expected);
    assert_eq!(
        tap.clientbound(peer(STEVE), State::Play, red.body(), false),
        Verdict::Pass
    );
    // A team that hides its name tags already, and packets without parameters, stay shared.
    expected.name = "blue".to_owned();
    for untouched in [
        expected,
        SetPlayerTeam {
            name: "red".to_owned(),
            action: TeamAction::Join(vec!["Alex".to_owned()]),
        },
    ] {
        assert_eq!(
            tap.clientbound(peer(ALEX), State::Play, &encode_body(&untouched), true),
            Verdict::Pass
        );
    }

    // Per broadcast: the prefix is put in front once, and all receivers share the result.
    let hello = SharedPacket::of(&SystemChat {
        content: Component::text("Hello"),
        overlay: false,
    });
    let (Verdict::Replace(first), Verdict::Replace(second)) = (
        tap.shared(State::Play, &hello),
        tap.shared(State::Play, &hello),
    ) else {
        panic!("the system message is rewritten");
    };
    assert!(Arc::ptr_eq(&first, &second), "one buffer for all receivers");
    assert_eq!(
        decode::<SystemChat>(first.body()).content.plain(),
        "[Server] Hello"
    );
    assert_eq!(
        tap.clientbound(peer(STEVE), State::Play, first.body(), false),
        Verdict::Pass
    );
    // A message for one player passes the same listener on its way out.
    let Verdict::Replace(single) = tap.clientbound(peer(STEVE), State::Play, hello.body(), true)
    else {
        panic!("the system message is rewritten");
    };
    assert_eq!(single, first.body());
    // Texts above the hotbar are left alone.
    let overlay = SharedPacket::of(&SystemChat {
        content: Component::text("Hello"),
        overlay: true,
    });
    assert!(matches!(tap.shared(State::Play, &overlay), Verdict::Pass));

    // Serverbound: the sign edit is cancelled before the game sees it.
    assert_eq!(
        tap.serverbound(
            peer(ALEX),
            State::Play,
            &[u8::try_from(sign_update).unwrap(), 0]
        ),
        Verdict::Drop
    );
    assert_eq!(
        tap.serverbound(peer(ALEX), State::Play, &[u8::try_from(chat).unwrap(), 0]),
        Verdict::Pass
    );
    assert_eq!(
        host.execute_command(command("status"), &alex, "nametags status", &[], None),
        Some(1)
    );
    assert!(
        server
            .messages
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.contains("1 viewers without name tags, 1 sign edits blocked")),
        "{:?}",
        server.messages.lock().unwrap()
    );

    // A player who joins is added to Alex's hidden team.
    server.packets.lock().unwrap().clear();
    host.player_join(&mut FakeWorld, STEVE, "Steve", "\"Steve joined the game\"");
    {
        let packets = server.packets.lock().unwrap();
        let [(to, body, _)] = packets.as_slice() else {
            panic!("one packet for Alex, got {}", packets.len());
        };
        assert_eq!(*to, ALEX);
        assert_eq!(
            decode::<SetPlayerTeam>(body).action,
            TeamAction::Join(vec!["Steve".to_owned()])
        );
    }

    // `/nametags on`: the team is removed and so is the listener.
    assert_eq!(
        host.execute_command(command("on"), &alex, "nametags on", &[], None),
        Some(1)
    );
    assert_eq!(
        decode::<SetPlayerTeam>(&server.packets.lock().unwrap().last().unwrap().1).action,
        TeamAction::Remove
    );
    assert!(!tap.wants(Direction::Clientbound, State::Play, SetPlayerTeam::ID));
    let red = SharedPacket::of(&team(0));
    assert_eq!(
        tap.clientbound(peer(ALEX), State::Play, red.body(), false),
        Verdict::Pass
    );

    host.disable_all();
    let _ = std::fs::remove_dir_all(&dir);
}
