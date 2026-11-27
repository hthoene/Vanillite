//! Random parity scenarios (#132): a seed makes a flat world with towers, pools and loose
//! blocks, some mobs and a packet-level player whose client sends what a real client would
//! (walking, sprinting, sneaking, jumping, falling, swimming, mining, building, item use,
//! combat, inventory clicks and crafting, riding). [`generate`] writes it as a scenario
//! ([`crate::scenario`]); `cargo xtask parity fuzz` records it in Vanilla, runs it here, and
//! shrinks a divergence ([`shrink_candidates`]) to a small scenario.
//!
//! The client simulation is deliberately simple (axis-aligned collision with the blocks the
//! generator placed, gravity, drag in fluids): when it is wrong, the server corrects the
//! player like a real one (moved wrongly, teleports), the same on both sides.

use std::collections::HashMap;

use serde_json::{Value, json};
use vanillite_protocol::Packet;
use vanillite_protocol::packets::play::serverbound::{
    Attack, ContainerClick, ContainerClose, ContainerInput, DigAction, Hand, HashedStack, Input,
    Interact, MoveFlags, MovePlayerPos, MovePlayerPosRot, MovePlayerRot, MovePlayerStatusOnly,
    PaddleBoat, PlayerAction, PlayerActionPacket, PlayerCommand, PlayerInput, Punch,
    SetCarriedItem, UseItem, UseItemOn,
};
use vanillite_protocol::types::{BlockPos as WirePos, LpVec3, Vec3 as WireVec3};

use crate::scenario::to_hex;

/// A small deterministic random (SplitMix64).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x005E_ED0F_1324_u64)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `0..n` (`n > 0`).
    pub fn below(&mut self, n: u32) -> u32 {
        (self.next_u64() % u64::from(n.max(1))) as u32
    }

    /// A number in `lo..=hi`.
    pub fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.below(u32::try_from(hi - lo + 1).unwrap_or(1)) as i32
    }

    pub fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    /// A number in `0..1`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn pick<'a, T>(&mut self, list: &'a [T]) -> &'a T {
        &list[self.below(list.len() as u32) as usize]
    }

    /// An index into `weights`, by weight.
    pub fn weighted(&mut self, weights: &[u32]) -> usize {
        let total: u32 = weights.iter().sum();
        let mut r = self.below(total);
        for (i, &w) in weights.iter().enumerate() {
            if r < w {
                return i;
            }
            r -= w;
        }
        weights.len() - 1
    }
}

/// Lowest block of the scenario world and the top of its ground.
const GROUND_TOP: i32 = -61;
/// The observed region: one chunk, from just below the ground to above the towers.
const REGION_MIN: [i32; 3] = [0, -62, 0];
const REGION_MAX: [i32; 3] = [15, -36, 15];

/// What a block is to the client's collision.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Cell {
    /// Collides up to this height within the block.
    Solid(f64),
    Water,
    Lava,
    /// No collision (air, plants, cobweb, powder snow).
    Open,
}

/// Blocks that stand alone, with their collision height.
const BLOCKS: &[(&str, f64)] = &[
    ("minecraft:stone", 1.0),
    ("minecraft:cobblestone", 1.0),
    ("minecraft:dirt", 1.0),
    ("minecraft:oak_planks", 1.0),
    ("minecraft:oak_log", 1.0),
    ("minecraft:glass", 1.0),
    ("minecraft:sand", 1.0),
    ("minecraft:gravel", 1.0),
    ("minecraft:hay_block", 1.0),
    ("minecraft:slime_block", 1.0),
    ("minecraft:honey_block", 0.9375),
    ("minecraft:soul_sand", 0.875),
    ("minecraft:magma_block", 1.0),
    ("minecraft:oak_leaves", 1.0),
    ("minecraft:ice", 1.0),
    ("minecraft:blue_ice", 1.0),
    ("minecraft:oak_slab", 0.5),
    ("minecraft:snow[layers=3]", 0.25),
    ("minecraft:farmland", 0.9375),
    ("minecraft:mud", 0.875),
    ("minecraft:white_wool", 1.0),
    ("minecraft:pointed_dripstone", 1.0),
];

/// Blocks without collision a falling player may land in.
const SOFT: &[&str] = &[
    "minecraft:cobweb",
    "minecraft:powder_snow",
    "minecraft:short_grass",
    "minecraft:poppy",
    "minecraft:sweet_berry_bush[age=3]",
    "minecraft:scaffolding[distance=0]",
    "minecraft:vine[north=true]",
];

/// Mobs that may stand near the player.
const MOBS: &[&str] = &[
    "minecraft:zombie",
    "minecraft:skeleton",
    "minecraft:creeper",
    "minecraft:spider",
    "minecraft:pig",
    "minecraft:cow",
    "minecraft:sheep",
    "minecraft:chicken",
    "minecraft:wolf",
    "minecraft:iron_golem",
    "minecraft:villager",
    "minecraft:slime",
];

/// Stacks the player may carry: item, count.
const ITEMS: &[(&str, i32)] = &[
    ("minecraft:stone", 64),
    ("minecraft:dirt", 32),
    ("minecraft:oak_log", 8),
    ("minecraft:oak_planks", 16),
    ("minecraft:sand", 16),
    ("minecraft:cobblestone", 32),
    ("minecraft:torch", 16),
    ("minecraft:water_bucket", 1),
    ("minecraft:lava_bucket", 1),
    ("minecraft:bucket", 1),
    ("minecraft:bread", 8),
    ("minecraft:cooked_beef", 4),
    ("minecraft:golden_apple", 2),
    ("minecraft:bow", 1),
    ("minecraft:arrow", 16),
    ("minecraft:snowball", 16),
    ("minecraft:egg", 16),
    ("minecraft:ender_pearl", 4),
    ("minecraft:wooden_pickaxe", 1),
    ("minecraft:stone_pickaxe", 1),
    ("minecraft:iron_pickaxe", 1),
    ("minecraft:diamond_shovel", 1),
    ("minecraft:iron_sword", 1),
    ("minecraft:wooden_axe", 1),
    ("minecraft:shears", 1),
    ("minecraft:flint_and_steel", 1),
    ("minecraft:bone_meal", 8),
    ("minecraft:wheat", 8),
    ("minecraft:iron_chestplate", 1),
    ("minecraft:leather_boots", 1),
    ("minecraft:shield", 1),
    ("minecraft:minecart", 1),
    ("minecraft:rail", 8),
];

/// Ticks of digging after which the client takes a block as broken (a rough guess; the
/// servers decide).
fn dig_ticks(block: &str, tool: Option<&str>) -> u32 {
    let shovel = tool.is_some_and(|t| t.ends_with("_shovel"));
    let pickaxe = tool.is_some_and(|t| t.ends_with("_pickaxe"));
    match block {
        "minecraft:dirt"
        | "minecraft:grass_block"
        | "minecraft:sand"
        | "minecraft:gravel"
        | "minecraft:farmland"
        | "minecraft:mud"
        | "minecraft:soul_sand" => {
            if shovel {
                3
            } else {
                15
            }
        }
        "minecraft:stone" | "minecraft:cobblestone" if pickaxe => 12,
        "minecraft:oak_planks" | "minecraft:oak_log" => 45,
        "minecraft:glass" | "minecraft:ice" | "minecraft:oak_leaves" | "minecraft:hay_block" => 10,
        "minecraft:slime_block" | "minecraft:honey_block" => 1,
        _ => 1000,
    }
}

/// A scenario being written.
struct Writer {
    ticks: u32,
    actions: Vec<Value>,
}

impl Writer {
    fn packet<P: Packet + std::fmt::Debug>(&mut self, tick: u32, p: &P, target: Option<&str>) {
        let mut body = Vec::new();
        p.encode(&mut body);
        let mut op = json!({
            "tick": tick,
            "op": "packet",
            "entity": PLAYER,
            "packet": P::NAME,
            "data": to_hex(&body),
            "what": format!("{p:?}"),
        });
        if let Some(t) = target {
            op["target"] = json!(t);
        }
        self.actions.push(op);
    }
}

/// Name of the client player.
const PLAYER: &str = "p";

/// The client's view of the world and of itself.
struct Client {
    cells: HashMap<(i32, i32, i32), (Cell, String)>,
    pos: [f64; 3],
    vel: [f64; 3],
    yaw: f32,
    pitch: f32,
    on_ground: bool,
    input: Input,
    sneaking: bool,
    sprinting: bool,
    /// Position and rotation last sent.
    sent_pos: [f64; 3],
    sent_rot: (f32, f32),
    sent_ground: bool,
    since_pos: u32,
    sequence: i32,
    selected: usize,
    hotbar: [Option<(String, i32)>; 9],
    /// Item use in progress: ticks left to hold.
    using: Option<u32>,
}

impl Client {
    fn cell(&self, x: i32, y: i32, z: i32) -> Cell {
        match self.cells.get(&(x, y, z)) {
            Some((c, _)) => *c,
            None if (-64..=GROUND_TOP).contains(&y) => Cell::Solid(1.0),
            None => Cell::Open,
        }
    }

    fn block_name(&self, x: i32, y: i32, z: i32) -> String {
        match self.cells.get(&(x, y, z)) {
            Some((_, n)) => n.clone(),
            None if y == GROUND_TOP => "minecraft:grass_block".to_owned(),
            None if y < GROUND_TOP && y > -64 => "minecraft:dirt".to_owned(),
            None if y == -64 => "minecraft:bedrock".to_owned(),
            None => "minecraft:air".to_owned(),
        }
    }

    fn height(&self) -> f64 {
        if self.sneaking { 1.5 } else { 1.8 }
    }

    /// Whether the player's box at `p` collides with a block.
    fn collides(&self, p: [f64; 3]) -> bool {
        let (x0, x1) = (p[0] - 0.3, p[0] + 0.3);
        let (y0, y1) = (p[1], p[1] + self.height());
        let (z0, z1) = (p[2] - 0.3, p[2] + 0.3);
        for x in x0.floor() as i32..=(x1 - 1e-7).floor() as i32 {
            for z in z0.floor() as i32..=(z1 - 1e-7).floor() as i32 {
                for y in (y0.floor() as i32 - 1)..=(y1 - 1e-7).floor() as i32 {
                    if let Cell::Solid(h) = self.cell(x, y, z) {
                        let top = f64::from(y) + h;
                        if top > y0 + 1e-7 && f64::from(y) < y1 - 1e-7 {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// The highest block top under the player's footprint between `from` and `to` (falling).
    fn landing(&self, from: f64, to: f64) -> Option<f64> {
        let p = self.pos;
        let mut best: Option<f64> = None;
        for x in (p[0] - 0.3).floor() as i32..=(p[0] + 0.3 - 1e-7).floor() as i32 {
            for z in (p[2] - 0.3).floor() as i32..=(p[2] + 0.3 - 1e-7).floor() as i32 {
                for y in (to.floor() as i32 - 1)..=from.floor() as i32 {
                    if let Cell::Solid(h) = self.cell(x, y, z) {
                        let top = f64::from(y) + h;
                        if top <= from + 1e-7 && top >= to - 1e-7 {
                            best = Some(best.map_or(top, |b: f64| b.max(top)));
                        }
                    }
                }
            }
        }
        best
    }

    fn feet_cell(&self) -> Cell {
        let p = self.pos;
        self.cell(
            p[0].floor() as i32,
            (p[1] + 0.1).floor() as i32,
            p[2].floor() as i32,
        )
    }

    /// One client tick of movement with the keys held; returns whether it hit a wall.
    fn travel(&mut self) -> bool {
        let fluid = self.feet_cell();
        let speed = if self.sneaking {
            0.065
        } else if self.sprinting {
            0.28
        } else {
            0.215
        };
        let (fwd, strafe) = (
            f64::from(i8::from(self.input.forward) - i8::from(self.input.backward)),
            f64::from(i8::from(self.input.left) - i8::from(self.input.right)),
        );
        let yaw = f64::from(self.yaw).to_radians();
        let (sin, cos) = (yaw.sin(), yaw.cos());
        let want = [
            (strafe * cos - fwd * sin) * speed,
            (fwd * cos + strafe * sin) * speed,
        ];
        let control = if self.on_ground { 1.0 } else { 0.3 };
        let drag = if self.on_ground { 0.0 } else { 0.7 };
        self.vel[0] = self.vel[0] * drag + want[0] * control;
        self.vel[2] = self.vel[2] * drag + want[1] * control;
        if matches!(fluid, Cell::Water | Cell::Lava) {
            self.vel[0] *= 0.5;
            self.vel[2] *= 0.5;
        }
        let mut hit = false;
        // Sneaking keeps the player on its block edge.
        for axis in [0, 2] {
            let mut p = self.pos;
            p[axis] += self.vel[axis];
            let edge = self.sneaking && self.on_ground && {
                let mut below = p;
                below[1] -= 0.6;
                !self.collides(below)
            };
            if self.collides(p) || edge {
                hit = true;
                self.vel[axis] = 0.0;
            } else {
                self.pos = p;
            }
        }
        // Keep inside the observed chunk.
        for axis in [0, 2] {
            if self.pos[axis] < 0.4 || self.pos[axis] > 15.6 {
                self.pos[axis] = self.pos[axis].clamp(0.4, 15.6);
                self.vel[axis] = 0.0;
                hit = true;
            }
        }
        if self.input.jump && self.on_ground {
            self.vel[1] = 0.42;
            if self.sprinting {
                self.vel[0] -= sin * 0.2;
                self.vel[2] += cos * 0.2;
            }
        } else if self.input.jump && matches!(fluid, Cell::Water | Cell::Lava) {
            self.vel[1] = 0.04;
        }
        self.vel[1] = match fluid {
            Cell::Water => self.vel[1] * 0.8 - 0.02,
            Cell::Lava => self.vel[1] * 0.5 - 0.02,
            _ => (self.vel[1] - 0.08) * 0.98,
        };
        let from = self.pos[1];
        let mut to = from + self.vel[1];
        if self.vel[1] < 0.0 {
            if let Some(top) = self.landing(from, to) {
                to = top;
                self.vel[1] = 0.0;
                self.on_ground = true;
            } else {
                self.on_ground = false;
            }
            self.pos[1] = to;
        } else {
            let mut p = self.pos;
            p[1] = to;
            if self.collides(p) {
                self.vel[1] = 0.0;
            } else {
                self.pos[1] = to;
            }
            self.on_ground = false;
        }
        hit
    }

    /// The move packet a client sends after its tick (`LocalPlayer.sendPosition`).
    fn send_move(&mut self, w: &mut Writer, tick: u32, hit: bool) {
        let moved =
            (0..3).any(|i| (self.pos[i] - self.sent_pos[i]).abs() > 2.0e-4) || self.since_pos >= 19;
        let turned = (self.yaw, self.pitch) != self.sent_rot;
        let flags = MoveFlags {
            on_ground: self.on_ground,
            horizontal_collision: hit,
        };
        let [x, y, z] = self.pos;
        if moved && turned {
            w.packet(
                tick,
                &MovePlayerPosRot {
                    x,
                    y,
                    z,
                    y_rot: self.yaw,
                    x_rot: self.pitch,
                    flags,
                },
                None,
            );
        } else if moved {
            w.packet(tick, &MovePlayerPos { x, y, z, flags }, None);
        } else if turned {
            w.packet(
                tick,
                &MovePlayerRot {
                    y_rot: self.yaw,
                    x_rot: self.pitch,
                    flags,
                },
                None,
            );
        } else if self.on_ground != self.sent_ground {
            w.packet(tick, &MovePlayerStatusOnly { flags }, None);
        }
        if moved {
            self.sent_pos = self.pos;
            self.since_pos = 0;
        } else {
            self.since_pos += 1;
        }
        self.sent_rot = (self.yaw, self.pitch);
        self.sent_ground = self.on_ground;
    }

    fn set_input(&mut self, w: &mut Writer, tick: u32, input: Input) {
        if input != self.input {
            self.input = input;
            w.packet(tick, &PlayerInput(input), None);
        }
        self.sneaking = input.shift;
    }

    fn set_sprint(&mut self, w: &mut Writer, tick: u32, on: bool) {
        if on != self.sprinting {
            self.sprinting = on;
            let action = if on {
                PlayerAction::StartSprinting
            } else {
                PlayerAction::StopSprinting
            };
            w.packet(
                tick,
                &PlayerCommand {
                    entity_id: 0,
                    action,
                    data: 0,
                },
                Some(PLAYER),
            );
        }
    }

    /// Turns the head to look at a point.
    fn look_at(&mut self, at: [f64; 3]) {
        let eye = [self.pos[0], self.pos[1] + 1.62, self.pos[2]];
        let (dx, dy, dz) = (at[0] - eye[0], at[1] - eye[1], at[2] - eye[2]);
        let flat = (dx * dx + dz * dz).sqrt();
        self.yaw = ((-dx).atan2(dz).to_degrees() as f32 * 64.0).round() / 64.0;
        self.pitch = ((-dy.atan2(flat).to_degrees()) as f32 * 64.0).round() / 64.0;
    }

    fn next_sequence(&mut self) -> i32 {
        self.sequence += 1;
        self.sequence
    }

    fn select(&mut self, w: &mut Writer, tick: u32, slot: usize) {
        if slot != self.selected {
            self.selected = slot;
            w.packet(tick, &SetCarriedItem { slot: slot as i16 }, None);
        }
    }

    /// A hotbar slot holding an item the predicate likes.
    fn find(&self, rng: &mut Rng, like: impl Fn(&str) -> bool) -> Option<usize> {
        let slots: Vec<usize> = (0..9)
            .filter(|&i| {
                self.hotbar[i]
                    .as_ref()
                    .is_some_and(|(n, c)| *c > 0 && like(n))
            })
            .collect();
        (!slots.is_empty()).then(|| *rng.pick(&slots))
    }

    fn held(&self) -> Option<&str> {
        self.hotbar[self.selected].as_ref().map(|(n, _)| n.as_str())
    }
}

/// The behaviours of the client, with their weights.
#[derive(Clone, Copy, Debug)]
enum Behaviour {
    Walk,
    Jump,
    Idle,
    Look,
    Mine,
    Place,
    UseItem,
    Attack,
    Interact,
    Inventory,
    Drop,
    Swap,
    Pillar,
    Ride,
}

const BEHAVIOURS: &[(Behaviour, u32)] = &[
    (Behaviour::Walk, 24),
    (Behaviour::Jump, 8),
    (Behaviour::Idle, 6),
    (Behaviour::Look, 3),
    (Behaviour::Mine, 12),
    (Behaviour::Place, 10),
    (Behaviour::UseItem, 8),
    (Behaviour::Attack, 9),
    (Behaviour::Interact, 4),
    (Behaviour::Inventory, 6),
    (Behaviour::Drop, 2),
    (Behaviour::Swap, 2),
    (Behaviour::Pillar, 3),
    (Behaviour::Ride, 2),
];

/// The scenario of `seed`.
pub fn generate(seed: u64) -> Value {
    let mut rng = Rng::new(seed);
    let mut setup = Vec::new();
    let mut cells: HashMap<(i32, i32, i32), (Cell, String)> = HashMap::new();
    let put = |setup: &mut Vec<Value>,
               cells: &mut HashMap<(i32, i32, i32), (Cell, String)>,
               op: &str,
               p: [i32; 3],
               block: &str,
               cell: Cell| {
        setup.push(json!({"op": op, "pos": p, "block": block}));
        cells.insert(
            (p[0], p[1], p[2]),
            (cell, block.split('[').next().unwrap_or(block).to_owned()),
        );
    };
    // Towers to fall from, with something to land on beside them.
    let mut tops = Vec::new();
    for _ in 0..rng.range(0, 3) {
        let (x, z) = (rng.range(2, 13), rng.range(2, 13));
        let h = *rng.pick(&[2, 3, 4, 5, 8, 12, 16, 22]);
        let (block, _) = *rng.pick(&BLOCKS[..8]);
        for y in GROUND_TOP + 1..=GROUND_TOP + h {
            put(
                &mut setup,
                &mut cells,
                "set",
                [x, y, z],
                block,
                Cell::Solid(1.0),
            );
        }
        tops.push([x, GROUND_TOP + h + 1, z]);
        if rng.chance(0.6) {
            let (dx, dz) = *rng.pick(&[(1, 0), (-1, 0), (0, 1), (0, -1)]);
            let pad = [x + dx, GROUND_TOP + 1, z + dz];
            if rng.chance(0.5) {
                let (b, h) = *rng.pick(BLOCKS);
                put(&mut setup, &mut cells, "set", pad, b, Cell::Solid(h));
            } else {
                let b = *rng.pick(SOFT);
                put(&mut setup, &mut cells, "set", pad, b, Cell::Open);
            }
        }
    }
    // A pool of water or lava in the ground.
    if rng.chance(0.35) {
        let (x0, z0) = (rng.range(1, 11), rng.range(1, 11));
        let (w, d) = (rng.range(1, 3), rng.range(1, 3));
        let lava = rng.chance(0.25);
        let depth = rng.range(1, 2);
        for x in x0..=x0 + w {
            for z in z0..=z0 + d {
                for y in GROUND_TOP - depth + 1..=GROUND_TOP {
                    put(
                        &mut setup,
                        &mut cells,
                        "set",
                        [x, y, z],
                        "minecraft:air",
                        Cell::Open,
                    );
                }
            }
        }
        let (fluid, cell) = if lava {
            ("minecraft:lava", Cell::Lava)
        } else {
            ("minecraft:water", Cell::Water)
        };
        for x in x0..=x0 + w {
            for z in z0..=z0 + d {
                for y in GROUND_TOP - depth + 1..=GROUND_TOP {
                    put(&mut setup, &mut cells, "place", [x, y, z], fluid, cell);
                }
            }
        }
    }
    // Loose blocks, some of them floating (falling blocks).
    for _ in 0..rng.range(0, 8) {
        let p = [
            rng.range(0, 15),
            GROUND_TOP + rng.range(1, 3),
            rng.range(0, 15),
        ];
        if cells.contains_key(&(p[0], p[1], p[2])) {
            continue;
        }
        if rng.chance(0.7) {
            let (b, h) = *rng.pick(BLOCKS);
            put(&mut setup, &mut cells, "set", p, b, Cell::Solid(h));
        } else {
            let b = *rng.pick(SOFT);
            put(&mut setup, &mut cells, "set", p, b, Cell::Open);
        }
    }

    // The player: on a tower or on the ground.
    let start = if !tops.is_empty() && rng.chance(0.6) {
        let t = *rng.pick(&tops);
        [
            f64::from(t[0]) + 0.5,
            f64::from(t[1]),
            f64::from(t[2]) + 0.5,
        ]
    } else {
        let (x, z) = (rng.range(1, 14), rng.range(1, 14));
        let mut y = GROUND_TOP + 1;
        while cells
            .get(&(x, y, z))
            .is_some_and(|(c, _)| matches!(c, Cell::Solid(_)))
        {
            y += 1;
        }
        [f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5]
    };
    let mut hotbar: [Option<(String, i32)>; 9] = Default::default();
    let mut inventory = Vec::new();
    let stacks = rng.range(1, 6) as usize;
    for (slot, held) in hotbar.iter_mut().enumerate().take(stacks) {
        let (item, count) = *rng.pick(ITEMS);
        *held = Some((item.to_owned(), count));
        inventory.push(json!([slot, item, count]));
    }
    // A few stacks in the main inventory for the clicks.
    for _ in 0..rng.range(0, 3) {
        let (item, count) = *rng.pick(ITEMS);
        inventory.push(json!([rng.range(9, 35), item, count]));
    }
    let yaw = (rng.below(8) * 45) as f32;
    let mut player = json!({
        "name": PLAYER, "type": "minecraft:player", "client": true,
        "pos": start, "rot": [yaw, 0.0], "inventory": inventory,
    });
    if rng.chance(0.3) {
        player["food"] = json!(rng.range(6, 19));
    }
    let mut entities = vec![player];
    let mut mob_names = Vec::new();
    for i in 0..rng.range(0, 3) {
        let kind = *rng.pick(MOBS);
        let name = format!("m{i}");
        let x = (start[0] + f64::from(rng.range(-4, 4)))
            .clamp(0.5, 15.5)
            .floor()
            + 0.5;
        let z = (start[2] + f64::from(rng.range(-4, 4)))
            .clamp(0.5, 15.5)
            .floor()
            + 0.5;
        let mut y = GROUND_TOP + 1;
        while cells
            .get(&(x.floor() as i32, y, z.floor() as i32))
            .is_some_and(|(c, _)| !matches!(c, Cell::Open))
        {
            y += 1;
        }
        let mut mob = json!({
            "name": name, "type": kind, "pos": [x, y, z],
            "rot": [(rng.below(4) * 90) as f32, 0.0], "seed": rng.range(1, 1_000_000),
        });
        // Mostly without `finalizeSpawn`: its known divergences (scenarios/divergences/
        // fuzz_*) would hide everything after tick 0.
        if rng.chance(0.8) {
            mob["finalize"] = json!(false);
        }
        entities.push(mob);
        mob_names.push((name, [x, f64::from(y), z]));
    }
    if rng.chance(0.15) {
        let x = (start[0] + 1.5).min(15.5);
        entities.push(json!({
            "name": "cart", "type": "minecraft:minecart",
            "pos": [x, f64::from(GROUND_TOP + 1), start[2]], "seed": 1,
        }));
        mob_names.push(("cart".to_owned(), [x, f64::from(GROUND_TOP + 1), start[2]]));
    }

    let ticks = rng.range(40, 160) as u32;
    let mut w = Writer {
        ticks,
        actions: Vec::new(),
    };
    let mut c = Client {
        cells,
        pos: start,
        vel: [0.0; 3],
        yaw,
        pitch: 0.0,
        on_ground: false,
        input: Input::default(),
        sneaking: false,
        sprinting: false,
        sent_pos: start,
        sent_rot: (yaw, 0.0),
        sent_ground: false,
        since_pos: 0,
        sequence: 0,
        selected: 0,
        hotbar,
        using: None,
    };
    let mut tick = 1;
    while tick <= w.ticks {
        let weights: Vec<u32> = BEHAVIOURS.iter().map(|b| b.1).collect();
        let behaviour = BEHAVIOURS[rng.weighted(&weights)].0;
        tick = act(&mut rng, &mut w, &mut c, behaviour, tick, &mob_names);
    }
    let w_ticks = w.ticks;
    json!({
        "description": format!("Random parity scenario of seed {seed} (#132, `cargo xtask parity fuzz`)."),
        "tier": "fast",
        "time": *rng.pick(&[1000, 6000, 13000, 18000]),
        "difficulty": *rng.pick(&["easy", "normal", "normal", "hard"]),
        "region": [REGION_MIN, REGION_MAX],
        "ticks": w_ticks,
        "setup": setup,
        "entities": entities,
        "actions": w.actions,
    })
}

/// Runs one behaviour from `tick` on; returns the next free tick.
fn act(
    rng: &mut Rng,
    w: &mut Writer,
    c: &mut Client,
    behaviour: Behaviour,
    mut tick: u32,
    mobs: &[(String, [f64; 3])],
) -> u32 {
    // One client tick: the extra packets of the tick, the movement, then the move packet.
    let step = |w: &mut Writer, c: &mut Client, tick: u32| {
        let hit = c.travel();
        c.send_move(w, tick, hit);
        if let Some(left) = c.using {
            if left == 0 {
                c.using = None;
                let seq = 0;
                w.packet(
                    tick,
                    &PlayerActionPacket {
                        action: DigAction::ReleaseUseItem,
                        pos: WirePos { x: 0, y: 0, z: 0 },
                        direction: 0,
                        sequence: seq,
                    },
                    None,
                );
            } else {
                c.using = Some(left - 1);
            }
        }
    };
    let end = w.ticks;
    match behaviour {
        Behaviour::Walk | Behaviour::Jump => {
            let n = rng.range(4, 30) as u32;
            c.yaw = (rng.below(8) * 45) as f32 + if rng.chance(0.3) { 10.0 } else { 0.0 };
            let sprint = rng.chance(0.3);
            let sneak = !sprint && rng.chance(0.2);
            let mut input = Input {
                forward: rng.chance(0.85),
                left: rng.chance(0.1),
                shift: sneak,
                sprint,
                ..Input::default()
            };
            input.backward = !input.forward && !input.left;
            c.set_input(w, tick, input);
            c.set_sprint(w, tick, sprint && input.forward);
            for _ in 0..n {
                if tick > end {
                    break;
                }
                let jump = matches!(behaviour, Behaviour::Jump) && c.on_ground && rng.chance(0.6);
                if jump != c.input.jump {
                    let mut i = c.input;
                    i.jump = jump;
                    c.set_input(w, tick, i);
                }
                step(w, c, tick);
                tick += 1;
            }
            c.set_sprint(w, tick.min(end), false);
            c.set_input(w, tick.min(end), Input::default());
        }
        Behaviour::Idle => {
            for _ in 0..rng.range(1, 20) {
                if tick > end {
                    break;
                }
                step(w, c, tick);
                tick += 1;
            }
        }
        Behaviour::Look => {
            c.yaw = rng.range(-180, 180) as f32;
            c.pitch = rng.range(-90, 90) as f32;
            step(w, c, tick);
            tick += 1;
        }
        Behaviour::Mine => {
            // A block next to or under the player.
            let (bx, by, bz) = (
                c.pos[0].floor() as i32,
                c.pos[1].floor() as i32,
                c.pos[2].floor() as i32,
            );
            let options = [
                (bx, by - 1, bz, 1u8),
                (bx + 1, by, bz, 4),
                (bx - 1, by, bz, 5),
                (bx, by, bz + 1, 2),
                (bx, by, bz - 1, 3),
                (bx + 1, by - 1, bz, 4),
                (bx, by + 1, bz + 1, 2),
            ];
            let solid: Vec<_> = options
                .iter()
                .filter(|o| matches!(c.cell(o.0, o.1, o.2), Cell::Solid(_)))
                .copied()
                .collect();
            let Some(&(x, y, z, face)) = (!solid.is_empty()).then(|| rng.pick(&solid)) else {
                step(w, c, tick);
                return tick + 1;
            };
            if let Some(slot) = c.find(rng, |n| {
                n.ends_with("_pickaxe") || n.ends_with("_shovel") || n.ends_with("_axe")
            }) && rng.chance(0.7)
            {
                c.select(w, tick, slot);
            }
            c.look_at([f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(z) + 0.5]);
            let pos = WirePos { x, y, z };
            let seq = c.next_sequence();
            w.packet(
                tick,
                &PlayerActionPacket {
                    action: DigAction::StartDestroyBlock,
                    pos,
                    direction: face,
                    sequence: seq,
                },
                None,
            );
            let name = c.block_name(x, y, z);
            let need = dig_ticks(&name, c.held());
            let hold = if rng.chance(0.6) {
                need.min(80)
            } else {
                rng.range(1, 40) as u32
            };
            for _ in 0..hold {
                if tick > end {
                    break;
                }
                w.packet(tick, &Punch, None);
                step(w, c, tick);
                tick += 1;
            }
            if tick > end {
                return tick;
            }
            let abort = rng.chance(0.2);
            let seq = c.next_sequence();
            w.packet(
                tick,
                &PlayerActionPacket {
                    action: if abort {
                        DigAction::AbortDestroyBlock
                    } else {
                        DigAction::StopDestroyBlock
                    },
                    pos,
                    direction: face,
                    sequence: seq,
                },
                None,
            );
            if !abort && hold >= need {
                c.cells
                    .insert((x, y, z), (Cell::Open, "minecraft:air".to_owned()));
            }
            step(w, c, tick);
            tick += 1;
        }
        Behaviour::Place | Behaviour::Pillar => {
            let pillar = matches!(behaviour, Behaviour::Pillar);
            let Some(slot) = c.find(rng, |n| {
                !n.contains("pickaxe")
                    && (BLOCKS.iter().any(|b| b.0 == n)
                        || n.ends_with("bucket")
                        || n == "minecraft:torch"
                        || n == "minecraft:rail"
                        || n == "minecraft:bone_meal"
                        || n == "minecraft:flint_and_steel"
                        || n == "minecraft:minecart")
            }) else {
                step(w, c, tick);
                return tick + 1;
            };
            c.select(w, tick, slot);
            let (bx, by, bz) = (
                c.pos[0].floor() as i32,
                c.pos[1].floor() as i32,
                c.pos[2].floor() as i32,
            );
            if pillar {
                // Jump and place under the feet at the top of the jump.
                let mut i = c.input;
                i.jump = true;
                c.set_input(w, tick, i);
                for _ in 0..4 {
                    if tick > end {
                        return tick;
                    }
                    step(w, c, tick);
                    tick += 1;
                    let mut i = c.input;
                    i.jump = false;
                    c.set_input(w, tick, i);
                }
            }
            if tick > end {
                return tick;
            }
            // Click the top of the block under (pillar) or the side of a neighbour.
            let (target, face, hit) = if pillar || rng.chance(0.4) {
                ((bx, by - 1, bz), 1, [0.5, 1.0, 0.5])
            } else {
                let (dx, dz, face) = *rng.pick(&[(1, 0, 4u8), (-1, 0, 5), (0, 1, 2), (0, -1, 3)]);
                let hit = match face {
                    4 => [0.0, 0.5, 0.5],
                    5 => [1.0, 0.5, 0.5],
                    2 => [0.5, 0.5, 0.0],
                    _ => [0.5, 0.5, 1.0],
                };
                (
                    (bx + dx * 2, by - 1 + rng.range(0, 1), bz + dz * 2),
                    face,
                    hit,
                )
            };
            let (tx, ty, tz) = target;
            c.look_at([
                f64::from(tx) + hit[0],
                f64::from(ty) + hit[1],
                f64::from(tz) + hit[2],
            ]);
            step(w, c, tick);
            let seq = c.next_sequence();
            w.packet(
                tick,
                &UseItemOn {
                    hand: Hand::MainHand,
                    pos: WirePos {
                        x: tx,
                        y: ty,
                        z: tz,
                    },
                    face: i32::from(face),
                    cursor_x: hit[0] as f32,
                    cursor_y: hit[1] as f32,
                    cursor_z: hit[2] as f32,
                    inside: false,
                    world_border_hit: false,
                    sequence: seq,
                },
                None,
            );
            w.packet(tick, &Punch, None);
            if let Some((name, count)) = c.hotbar[c.selected].as_mut()
                && let Some(&(_, h)) = BLOCKS.iter().find(|b| b.0 == name.as_str())
            {
                let (dx, dy, dz) = match face {
                    0 => (0, -1, 0),
                    1 => (0, 1, 0),
                    2 => (0, 0, -1),
                    3 => (0, 0, 1),
                    4 => (-1, 0, 0),
                    _ => (1, 0, 0),
                };
                if matches!(
                    c.cells.get(&(tx, ty, tz)).map(|x| x.0),
                    Some(Cell::Solid(_)) | None
                ) && ty <= GROUND_TOP + 30
                {
                    *count -= 1;
                    let n = name.clone();
                    c.cells
                        .insert((tx + dx, ty + dy, tz + dz), (Cell::Solid(h), n));
                }
            }
            tick += 1;
        }
        Behaviour::UseItem => {
            let Some(slot) = c.find(rng, |n| {
                [
                    "minecraft:bread",
                    "minecraft:cooked_beef",
                    "minecraft:golden_apple",
                    "minecraft:bow",
                    "minecraft:snowball",
                    "minecraft:egg",
                    "minecraft:ender_pearl",
                    "minecraft:shield",
                    "minecraft:iron_chestplate",
                    "minecraft:leather_boots",
                    "minecraft:water_bucket",
                    "minecraft:bucket",
                ]
                .contains(&n)
            }) else {
                step(w, c, tick);
                return tick + 1;
            };
            c.select(w, tick, slot);
            if rng.chance(0.5) {
                c.yaw = (rng.below(8) * 45) as f32;
                c.pitch = rng.range(-60, 30) as f32;
            }
            let seq = c.next_sequence();
            w.packet(
                tick,
                &UseItem {
                    hand: Hand::MainHand,
                    sequence: seq,
                    y_rot: c.yaw,
                    x_rot: c.pitch,
                },
                None,
            );
            let held = c.held().unwrap_or_default();
            if [
                "minecraft:bow",
                "minecraft:shield",
                "minecraft:bread",
                "minecraft:cooked_beef",
                "minecraft:golden_apple",
            ]
            .contains(&held)
            {
                c.using = Some(rng.range(3, 40) as u32);
            }
            for _ in 0..rng.range(1, 45) {
                if tick > end {
                    break;
                }
                step(w, c, tick);
                tick += 1;
            }
        }
        Behaviour::Attack | Behaviour::Interact if !mobs.is_empty() => {
            let (target, at) = rng.pick(mobs).clone();
            // A real client hits what it looks at: mostly aim at where the target started.
            if rng.chance(0.7) && tick < end {
                c.look_at([at[0], at[1] + 0.6, at[2]]);
                step(w, c, tick);
                tick += 1;
            }
            if rng.chance(0.5)
                && let Some(slot) = c.find(rng, |n| {
                    n.ends_with("_sword")
                        || n.ends_with("_axe")
                        || n == "minecraft:shears"
                        || n == "minecraft:wheat"
                        || n == "minecraft:bucket"
                })
            {
                c.select(w, tick, slot);
            }
            for _ in 0..rng.range(1, 4) {
                if tick > end {
                    break;
                }
                if matches!(behaviour, Behaviour::Attack) {
                    w.packet(tick, &Attack { entity_id: 0 }, Some(&target));
                    w.packet(tick, &Punch, None);
                } else {
                    w.packet(
                        tick,
                        &Interact {
                            entity_id: 0,
                            hand: Hand::MainHand,
                            location: LpVec3(WireVec3 {
                                x: 0.0,
                                y: 0.5,
                                z: 0.0,
                            }),
                            using_secondary_action: c.sneaking,
                        },
                        Some(&target),
                    );
                }
                for _ in 0..rng.range(1, 15) {
                    if tick > end {
                        break;
                    }
                    step(w, c, tick);
                    tick += 1;
                }
            }
        }
        Behaviour::Attack | Behaviour::Interact => {
            step(w, c, tick);
            tick += 1;
        }
        Behaviour::Inventory => {
            // Clicks in the inventory menu (container 0): pick up, place, shift-click, swap,
            // throw, and sometimes a 2x2 craft (logs to planks).
            let empty = HashedStack { item: None };
            let click =
                |w: &mut Writer, tick: u32, slot: i16, button: i8, input: ContainerInput| {
                    w.packet(
                        tick,
                        &ContainerClick {
                            container_id: 0,
                            state_id: 0,
                            slot,
                            button,
                            input,
                            changed: Vec::new(),
                            carried: HashedStack { item: None },
                        },
                        None,
                    );
                };
            let _ = empty;
            for _ in 0..rng.range(1, 5) {
                if tick > end {
                    break;
                }
                // Menu slots: 0 result, 1-4 crafting, 5-8 armor, 9-35 main, 36-44 hotbar,
                // 45 off hand.
                match rng.below(6) {
                    0 => {
                        let from = rng.range(9, 44) as i16;
                        click(w, tick, from, 0, ContainerInput::Pickup);
                        click(w, tick, rng.range(1, 4) as i16, 1, ContainerInput::Pickup);
                        click(w, tick, from, 0, ContainerInput::Pickup);
                        click(w, tick, 0, 0, ContainerInput::QuickMove);
                    }
                    1 => click(
                        w,
                        tick,
                        rng.range(9, 44) as i16,
                        0,
                        ContainerInput::QuickMove,
                    ),
                    2 => click(
                        w,
                        tick,
                        rng.range(9, 44) as i16,
                        rng.range(0, 8) as i8,
                        ContainerInput::Swap,
                    ),
                    3 => click(
                        w,
                        tick,
                        rng.range(9, 44) as i16,
                        rng.range(0, 1) as i8,
                        ContainerInput::Throw,
                    ),
                    4 => {
                        click(w, tick, rng.range(5, 45) as i16, 0, ContainerInput::Pickup);
                        click(
                            w,
                            tick,
                            rng.range(5, 45) as i16,
                            rng.range(0, 1) as i8,
                            ContainerInput::Pickup,
                        );
                    }
                    _ => click(
                        w,
                        tick,
                        rng.range(9, 44) as i16,
                        0,
                        ContainerInput::PickupAll,
                    ),
                }
                step(w, c, tick);
                tick += 1;
            }
            if tick <= end {
                w.packet(tick, &ContainerClose { container_id: 0 }, None);
            }
        }
        Behaviour::Drop => {
            let action = if rng.chance(0.7) {
                DigAction::DropItem
            } else {
                DigAction::DropAllItems
            };
            w.packet(
                tick,
                &PlayerActionPacket {
                    action,
                    pos: WirePos { x: 0, y: 0, z: 0 },
                    direction: 0,
                    sequence: 0,
                },
                None,
            );
            step(w, c, tick);
            tick += 1;
        }
        Behaviour::Swap => {
            w.packet(
                tick,
                &PlayerActionPacket {
                    action: DigAction::SwapItemWithOffhand,
                    pos: WirePos { x: 0, y: 0, z: 0 },
                    direction: 0,
                    sequence: 0,
                },
                None,
            );
            step(w, c, tick);
            tick += 1;
        }
        Behaviour::Ride => {
            if mobs.iter().any(|m| m.0 == "cart") {
                w.packet(
                    tick,
                    &Interact {
                        entity_id: 0,
                        hand: Hand::MainHand,
                        location: LpVec3(WireVec3 {
                            x: 0.0,
                            y: 0.3,
                            z: 0.0,
                        }),
                        using_secondary_action: false,
                    },
                    Some("cart"),
                );
                for _ in 0..rng.range(5, 30) {
                    if tick > end {
                        break;
                    }
                    w.packet(
                        tick,
                        &PaddleBoat {
                            left: false,
                            right: false,
                        },
                        None,
                    );
                    tick += 1;
                }
                if tick <= end {
                    let mut i = c.input;
                    i.shift = true;
                    c.set_input(w, tick, i);
                    tick += 1;
                    c.set_input(w, tick.min(end), Input::default());
                }
            } else {
                step(w, c, tick);
                tick += 1;
            }
        }
    }
    tick
}

/// Smaller variants of a diverging scenario, most aggressive first: fewer ticks (up to the
/// first divergence), fewer actions (halves, quarters, ... down to single ones), fewer setup
/// steps and entities. `first` is the tick of the first divergence.
pub fn shrink_candidates(scenario: &Value, first: u32, granularity: usize) -> Vec<Value> {
    let mut out = Vec::new();
    let ticks = scenario["ticks"].as_u64().unwrap_or(0) as u32;
    if first < ticks {
        let mut s = scenario.clone();
        s["ticks"] = json!(first.max(1));
        if let Some(a) = s["actions"].as_array_mut() {
            a.retain(|op| {
                op["tick"]
                    .as_u64()
                    .is_some_and(|t| t <= u64::from(first.max(1)))
            });
        }
        out.push(s);
        return out;
    }
    for key in ["actions", "setup", "entities"] {
        let Some(list) = scenario[key].as_array() else {
            continue;
        };
        let n = list.len();
        if n == 0 {
            continue;
        }
        let parts = granularity.clamp(1, n);
        let size = n.div_ceil(parts);
        for start in (0..n).step_by(size) {
            let mut s = scenario.clone();
            let kept: Vec<Value> = list
                .iter()
                .enumerate()
                .filter(|(i, v)| {
                    let removed = *i >= start && *i < start + size;
                    // The client player stays.
                    !removed || (key == "entities" && v["name"] == PLAYER)
                })
                .map(|(_, v)| v.clone())
                .collect();
            if kept.len() == n {
                continue;
            }
            // Actions that name removed entities go with them.
            if key == "entities" {
                let names: Vec<&str> = kept.iter().filter_map(|e| e["name"].as_str()).collect();
                if let Some(a) = s["actions"].as_array_mut() {
                    a.retain(|op| {
                        ["entity", "target", "by"]
                            .iter()
                            .all(|k| op[*k].as_str().is_none_or(|n| names.contains(&n)))
                    });
                }
            }
            s[key] = Value::Array(kept);
            out.push(s);
        }
    }
    // Drop the player's start inventory stack by stack.
    if let Some(entities) = scenario["entities"].as_array() {
        for (i, e) in entities.iter().enumerate() {
            let Some(inv) = e["inventory"].as_array() else {
                continue;
            };
            for j in 0..inv.len() {
                let mut s = scenario.clone();
                if let Some(list) = s["entities"][i]["inventory"].as_array_mut() {
                    list.remove(j);
                }
                out.push(s);
            }
        }
    }
    out
}

/// A short signature of a divergence (what differs, without the values), so shrinking keeps
/// the same divergence instead of finding another one: `block`, `<entity> (<type>): <field>`,
/// `block ticks`, `entities missing`, ...
pub fn signature(diff: &[String]) -> String {
    let Some(first) = diff.first() else {
        return String::new();
    };
    // Without `tick N `.
    let rest = first
        .strip_prefix("tick ")
        .and_then(|r| r.split_once(' '))
        .map_or(first.as_str(), |(_, r)| r);
    if rest.starts_with("block ") && !rest.starts_with("block ticks") {
        return "block".to_owned();
    }
    if rest.starts_with("light ") {
        return "light".to_owned();
    }
    match rest.split_once(": ") {
        Some((who, what)) => {
            let field = what.split(' ').next().unwrap_or_default();
            format!("{who}: {field}")
        }
        None => rest
            .split(' ')
            .take_while(|w| {
                !w.chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit() || c == '-' || c == '[')
                    && !w.starts_with("Some(")
                    && !w.starts_with("None")
            })
            .take(4)
            .collect::<Vec<_>>()
            .join(" "),
    }
}
