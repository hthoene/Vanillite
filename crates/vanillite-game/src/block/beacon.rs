//! Beacons (`BeaconBlockEntity`, `BeaconMenu`) and conduits (`ConduitBlockEntity`): their
//! ticks, the effects they give and the beacon's power selection.

use vanillite_base::random::RandomSource;
use vanillite_base::{Aabb, BlockPos};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BlockState, ids};
use vanillite_nbt::{Compound, Tag};
use vanillite_world::entity::EntityId;

use crate::effect::{EffectInstance, MobEffect};
use crate::level::{BlockEntity, Level, SOUND_BLOCKS};

/// `BeaconBlockEntity.BEACON_EFFECTS`: the powers by the pyramid level they need.
const BEACON_EFFECTS: [&[MobEffect]; 4] = [
    &[MobEffect::SPEED, MobEffect::HASTE],
    &[MobEffect::RESISTANCE, MobEffect::JUMP_BOOST],
    &[MobEffect::STRENGTH],
    &[MobEffect::REGENERATION],
];

/// The live data of a beacon.
#[derive(Clone, Debug, PartialEq)]
pub struct Beacon {
    /// `primaryPower`.
    pub primary: Option<MobEffect>,
    /// `secondaryPower`.
    pub secondary: Option<MobEffect>,
    /// `levels`: the pyramid tiers below it (0–4).
    pub levels: i32,
    /// `lastCheckY`.
    last_check_y: i32,
    /// Sections of the beam being checked (only whether the beam is clear matters here).
    checking_sections: u32,
    /// `beamSections`: the sections of the last full check.
    beam_sections: u32,
    /// `CustomName`.
    pub custom_name: Option<vanillite_protocol::text::Component>,
    /// `lock` (`LockCode`).
    pub lock: Option<Tag>,
}

impl Default for Beacon {
    fn default() -> Beacon {
        Beacon {
            primary: None,
            secondary: None,
            levels: 0,
            last_check_y: i32::MIN,
            checking_sections: 0,
            beam_sections: 0,
            custom_name: None,
            lock: None,
        }
    }
}

/// `getRequiredLevelsFor`: the tier a power needs, 0 for none.
fn required_levels(effect: Option<MobEffect>) -> i32 {
    let Some(effect) = effect else {
        return 0;
    };
    BEACON_EFFECTS
        .iter()
        .position(|tier| tier.contains(&effect))
        .and_then(|i| i32::try_from(i + 1).ok())
        .unwrap_or(i32::MAX)
}

/// `filterEffect`: only beacon powers.
fn filter(effect: Option<MobEffect>) -> Option<MobEffect> {
    effect.filter(|e| BEACON_EFFECTS.iter().any(|tier| tier.contains(e)))
}

/// `BeaconBlockEntity.validateEffects`.
pub fn validate_effects(
    primary: Option<MobEffect>,
    secondary: Option<MobEffect>,
    levels: i32,
) -> bool {
    if secondary.is_some() && levels < 4 {
        return false;
    }
    let primary_level = required_levels(primary);
    let secondary_level = required_levels(secondary);
    if primary_level > levels || secondary_level > levels {
        return false;
    }
    primary_level < 4 && (secondary_level == 0 || secondary_level >= 4 || primary == secondary)
}

/// `BeaconMenu.encodeEffect`: registry id + 1, 0 for none.
pub fn encode_effect(effect: Option<MobEffect>) -> i32 {
    effect.map_or(0, |e| i32::from(e.0) + 1)
}

impl Beacon {
    /// The menu's data slots: levels, primary, secondary.
    pub fn data(&self, index: usize) -> i32 {
        match index {
            0 => self.levels,
            1 => encode_effect(self.primary),
            2 => encode_effect(self.secondary),
            _ => 0,
        }
    }

    /// `loadAdditional`.
    pub fn load(&mut self, nbt: &Compound) {
        let effect = |key: &str| {
            nbt.get(key)
                .and_then(Tag::as_str)
                .and_then(MobEffect::by_name)
        };
        self.primary = filter(effect("primary_effect"));
        self.secondary = filter(effect("secondary_effect"));
        self.custom_name = super::container::load_name(nbt);
        self.lock = super::lock::load(nbt);
    }

    /// `saveAdditional`.
    pub fn save(&self, nbt: &mut Compound) {
        if let Some(e) = self.primary {
            nbt.insert("primary_effect", e.key());
        }
        if let Some(e) = self.secondary {
            nbt.insert("secondary_effect", e.key());
        }
        nbt.insert("Levels", self.levels);
        super::container::save_name(&self.custom_name, nbt);
        super::lock::save(&self.lock, nbt);
    }
}

/// Whether a block is in a block tag of the loaded data packs.
fn in_block_tag(state: BlockState, tag: &str) -> bool {
    crate::player::play::game_data().is_some_and(|d| {
        d.registry_tags
            .get("minecraft:block")
            .and_then(|tags| tags.get(tag))
            .is_some_and(|ids| ids.contains(&u32::from(state.block().0)))
    })
}

/// `getHeight(WORLD_SURFACE, x, z)`.
fn world_surface(level: &Level, x: i32, z: i32) -> i32 {
    let Some(c) = level.chunks.chunk(BlockPos::new(x, 0, z).chunk()) else {
        return level.min_y();
    };
    let Some(h) = c.chunk.heightmaps().get(HeightmapKind::WorldSurface) else {
        return level.min_y();
    };
    let (lx, lz) = (
        u32::try_from(x & 15).unwrap_or(0),
        u32::try_from(z & 15).unwrap_or(0),
    );
    i32::try_from(h.get(lx, lz)).unwrap_or(0) + c.chunk.min_y()
}

/// `BeaconBeamBlock`: stained glass, stained glass panes and the beacon.
fn is_beam_block(state: BlockState) -> bool {
    let name = state.block().name();
    state.block() == ids::BEACON
        || name.ends_with("_stained_glass")
        || name.ends_with("_stained_glass_pane")
}

/// `EntitySelector.NO_SPECTATORS`.
fn not_spectator(level: &Level, id: EntityId) -> bool {
    level
        .entities
        .player(id)
        .is_none_or(|p| p.game_mode != vanillite_config::GameMode::Spectator)
}

fn play(level: &mut Level, pos: BlockPos, sound: &str) {
    let id = crate::block::sound_id(sound);
    level.play_block_sound(None, pos, id, SOUND_BLOCKS, 1.0, 1.0);
}

/// `BeaconBlockEntity.tick` on the server.
pub fn tick(level: &mut Level, pos: BlockPos, _state: BlockState) {
    let Some(BlockEntity::Beacon(b)) = level.block_entity(pos) else {
        return;
    };
    let mut b = (**b).clone();
    let (x, y, z) = (pos.x, pos.y, pos.z);
    let mut check = if b.last_check_y < y {
        b.checking_sections = 0;
        b.last_check_y = y - 1;
        pos
    } else {
        BlockPos::new(x, b.last_check_y + 1, z)
    };
    let last_set_block = world_surface(level, x, z);
    let mut i = 0;
    while i < 10 && check.y <= last_set_block {
        let state = level.block(check);
        if is_beam_block(state) {
            // Same colour or not: only the existence of a section matters on the server.
            if b.checking_sections <= 1 {
                b.checking_sections += 1;
            }
        } else if b.checking_sections == 0
            || (state.light_dampening() >= 15 && state.block() != ids::BEDROCK)
        {
            b.checking_sections = 0;
            b.last_check_y = last_set_block;
            break;
        }
        check = BlockPos::new(check.x, check.y + 1, check.z);
        b.last_check_y += 1;
        i += 1;
    }
    let previous_levels = b.levels;
    if level.game_time() % 80 == 0 {
        if b.beam_sections > 0 {
            b.levels = update_base(level, x, y, z);
        }
        if b.levels > 0 && b.beam_sections > 0 {
            apply_effects(level, pos, b.levels, b.primary, b.secondary);
            play(level, pos, "minecraft:block.beacon.ambient");
        }
    }
    if b.last_check_y >= last_set_block {
        b.last_check_y = level.min_y() - 1;
        let was_active = previous_levels > 0;
        b.beam_sections = b.checking_sections;
        let active = b.levels > 0;
        if !was_active && active {
            play(level, pos, "minecraft:block.beacon.activate");
            // `CONSTRUCT_BEACON` for the players near the base.
            let (fx, fy, fz) = (f64::from(x), f64::from(y), f64::from(z));
            let area = Aabb::new(
                fx - 10.0,
                fy - 9.0,
                fz - 10.0,
                fx + 10.0,
                fy + 5.0,
                fz + 10.0,
            );
            let mut players: Vec<EntityId> = Vec::new();
            level
                .entities
                .store
                .for_each_in(&area, true, |p| players.push(p));
            // `getEntitiesOfClass` leaves spectators out.
            players.retain(|&p| not_spectator(level, p));
            for p in players {
                level.trigger(
                    p,
                    crate::advancements::trigger::Event::ConstructBeacon(b.levels),
                );
            }
        } else if was_active && !active {
            play(level, pos, "minecraft:block.beacon.deactivate");
        }
    }
    if let Some(BlockEntity::Beacon(slot)) = level.block_entity_mut(pos) {
        **slot = b;
    }
}

/// `updateBase`: the complete pyramid tiers of beacon base blocks below.
fn update_base(level: &Level, x: i32, y: i32, z: i32) -> i32 {
    let mut levels = 0;
    for step in 1..=4 {
        let ly = y - step;
        if ly < level.min_y() {
            break;
        }
        let ok = (x - step..=x + step).all(|lx| {
            (z - step..=z + step).all(|lz| {
                in_block_tag(
                    level.block(BlockPos::new(lx, ly, lz)),
                    "minecraft:beacon_base_blocks",
                )
            })
        });
        if !ok {
            break;
        }
        levels = step;
    }
    levels
}

/// `applyEffects`: the primary power (level II with two primary choices at tier 4) and the
/// secondary power to every player in range, as ambient effects.
fn apply_effects(
    level: &mut Level,
    pos: BlockPos,
    levels: i32,
    primary: Option<MobEffect>,
    secondary: Option<MobEffect>,
) {
    let Some(primary) = primary else {
        return;
    };
    let range = f64::from(levels * 10 + 10);
    let amplifier = i32::from(levels >= 4 && Some(primary) == secondary);
    let duration = (9 + levels * 2) * 20;
    let height = f64::from(level.chunks.info().height);
    let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    // `new AABB(pos).inflate(range).expandTowards(0, height, 0)`.
    let area = Aabb::new(
        x - range,
        y - range,
        z - range,
        x + 1.0 + range,
        y + 1.0 + range + height,
        z + 1.0 + range,
    );
    let mut players: Vec<EntityId> = Vec::new();
    level
        .entities
        .store
        .for_each_in(&area, true, |p| players.push(p));
    players.retain(|&p| not_spectator(level, p));
    for &p in &players {
        let e = EffectInstance::with_flags(primary, duration, amplifier, true, true, true);
        crate::effect::living::add_to_player(&mut level.entities, p, e);
    }
    if levels >= 4
        && Some(primary) != secondary
        && let Some(secondary) = secondary
    {
        for &p in &players {
            let e = EffectInstance::with_flags(secondary, duration, 0, true, true, true);
            crate::effect::living::add_to_player(&mut level.entities, p, e);
        }
    }
}

/// `BeaconMenu.updateEffects` from `set_beacon`: with an item in the payment slot and valid
/// powers for the beacon's tier, the powers are set and the payment is used up. Returns whether
/// it happened.
pub fn update_effects(
    level: &mut Level,
    pos: BlockPos,
    payment: &mut crate::item::ItemStack,
    primary: Option<i32>,
    secondary: Option<i32>,
) -> bool {
    if payment.is_empty() {
        return false;
    }
    let primary = primary.and_then(MobEffect::from_id);
    let secondary = secondary.and_then(MobEffect::from_id);
    let Some(BlockEntity::Beacon(b)) = level.block_entity_mut(pos) else {
        return false;
    };
    if !validate_effects(primary, secondary, b.levels) {
        return false;
    }
    // `beaconData.set`: `filterEffect` keeps only beacon powers; setting the primary power
    // plays the selection sound while the beam shines.
    let beam = b.beam_sections > 0;
    b.primary = filter(primary);
    b.secondary = filter(secondary);
    if beam {
        play(level, pos, "minecraft:block.beacon.power_select");
    }
    payment.shrink(1);
    level.block_entity_changed(pos);
    true
}

/// `BeaconBlockEntity.setRemoved`: the deactivation sound.
pub fn removed(level: &mut Level, pos: BlockPos) {
    play(level, pos, "minecraft:block.beacon.deactivate");
}

/// The live data of a conduit.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Conduit {
    /// `isActive`.
    pub active: bool,
    /// `isHunting`.
    pub hunting: bool,
    /// `effectBlocks`: the frame blocks found by the last shape check.
    pub frame: u32,
    /// `destroyTarget`: the UUID of the hunted mob.
    pub target: Option<u128>,
    /// `nextAmbientSoundActivation`.
    next_ambient: i64,
    /// `tickCount`.
    tick_count: i32,
}

impl Conduit {
    /// `loadAdditional`.
    pub fn load(&mut self, nbt: &Compound) {
        self.target = match nbt.get("Target") {
            Some(Tag::IntArray(a)) => match a.as_slice() {
                [a, b, c, d] => Some(
                    u128::from(u32::from_ne_bytes(a.to_ne_bytes())) << 96
                        | u128::from(u32::from_ne_bytes(b.to_ne_bytes())) << 64
                        | u128::from(u32::from_ne_bytes(c.to_ne_bytes())) << 32
                        | u128::from(u32::from_ne_bytes(d.to_ne_bytes())),
                ),
                _ => None,
            },
            _ => None,
        };
    }

    /// `saveAdditional`.
    pub fn save(&self, nbt: &mut Compound) {
        if let Some(uuid) = self.target {
            let part = |shift: u32| {
                let v = u32::try_from((uuid >> shift) & 0xFFFF_FFFF).unwrap_or(0);
                i32::from_ne_bytes(v.to_ne_bytes())
            };
            nbt.insert(
                "Target",
                Tag::IntArray(vec![part(96), part(64), part(32), part(0)]),
            );
        }
    }
}

/// `ConduitBlockEntity.serverTick`.
pub fn conduit_tick(level: &mut Level, pos: BlockPos, _state: BlockState) {
    let Some(BlockEntity::Conduit(c)) = level.block_entity(pos) else {
        return;
    };
    let mut c = (**c).clone();
    c.tick_count = c.tick_count.wrapping_add(1);
    let game_time = level.game_time();
    if game_time % 40 == 0 {
        let frame = conduit_shape(level, pos);
        let active = frame.is_some_and(|n| n >= 16);
        c.frame = frame.unwrap_or(0);
        if active != c.active {
            let sound = if active {
                "minecraft:block.conduit.activate"
            } else {
                "minecraft:block.conduit.deactivate"
            };
            play(level, pos, sound);
        }
        c.active = active;
        c.hunting = c.frame >= 42;
        if active {
            conduit_effects(level, pos, c.frame);
            conduit_attack(level, pos, &mut c);
        }
    }
    if c.active {
        if game_time % 80 == 0 {
            play(level, pos, "minecraft:block.conduit.ambient");
        }
        if game_time > c.next_ambient {
            let extra = level.random().next_int_bounded(40);
            c.next_ambient = game_time + 60 + i64::from(extra);
            play(level, pos, "minecraft:block.conduit.ambient.short");
        }
    }
    if let Some(BlockEntity::Conduit(slot)) = level.block_entity_mut(pos) {
        **slot = c;
    }
}

/// `updateShape`: `None` when the 3×3×3 around it is not all water, else the number of frame
/// blocks (`#conduit_effect_block`) on the three rings.
fn conduit_shape(level: &Level, pos: BlockPos) -> Option<u32> {
    for ox in -1..=1 {
        for oy in -1..=1 {
            for oz in -1..=1 {
                let p = BlockPos::new(pos.x + ox, pos.y + oy, pos.z + oz);
                if !crate::block::fluid::is_water(level.fluid(p)) {
                    return None;
                }
            }
        }
    }
    let mut count = 0;
    for ox in -2_i32..=2 {
        for oy in -2_i32..=2 {
            for oz in -2_i32..=2 {
                let (ax, ay, az) = (ox.abs(), oy.abs(), oz.abs());
                if (ax > 1 || ay > 1 || az > 1)
                    && (ox == 0 && (ay == 2 || az == 2)
                        || oy == 0 && (ax == 2 || az == 2)
                        || oz == 0 && (ax == 2 || ay == 2))
                {
                    let p = BlockPos::new(pos.x + ox, pos.y + oy, pos.z + oz);
                    if in_block_tag(level.block(p), "minecraft:conduit_effect_block") {
                        count += 1;
                    }
                }
            }
        }
    }
    Some(count)
}

/// `applyEffects`: Conduit Power for players in range that are in water or rain.
fn conduit_effects(level: &mut Level, pos: BlockPos, frame: u32) {
    let range = i32::try_from(frame / 7 * 16).unwrap_or(0);
    let r = f64::from(range);
    let height = f64::from(level.chunks.info().height);
    let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    let area = Aabb::new(
        x - r,
        y - r,
        z - r,
        x + 1.0 + r,
        y + 1.0 + r + height,
        z + 1.0 + r,
    );
    let mut players: Vec<EntityId> = Vec::new();
    level
        .entities
        .store
        .for_each_in(&area, true, |p| players.push(p));
    players.retain(|&p| not_spectator(level, p));
    for p in players {
        let Some(e) = level.entities.store.get(p) else {
            continue;
        };
        let at = e.pos();
        let block = BlockPos::containing(at.x, at.y, at.z);
        // `closerThan(blockPosition, range)`: block centre distance below the range.
        let d = |a: i32, b: i32| f64::from(a) - f64::from(b);
        let dist =
            d(pos.x, block.x).powi(2) + d(pos.y, block.y).powi(2) + d(pos.z, block.z).powi(2);
        if dist >= r * r {
            continue;
        }
        let wet = level
            .entities
            .player(p)
            .is_some_and(|pd| pd.survival.fluids.in_water())
            || level.is_raining_at(block)
            || level.is_raining_at(BlockPos::containing(at.x, e.aabb().max_y, at.z));
        if wet {
            let e = EffectInstance::with_flags(MobEffect::CONDUIT_POWER, 260, 0, true, true, true);
            crate::effect::living::add_to_player(&mut level.entities, p, e);
        }
    }
}

/// `updateAndAttackTarget` with `updateDestroyTarget`: a full frame hunts one enemy in water or
/// rain within 8 blocks and deals it 4 magic damage every 40 ticks.
fn conduit_attack(level: &mut Level, pos: BlockPos, c: &mut Conduit) {
    let previous = c.target;
    let find = |level: &Level, uuid: u128| -> Option<EntityId> {
        let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
        let area = Aabb::new(x - 8.0, y - 8.0, z - 8.0, x + 9.0, y + 9.0, z + 9.0);
        let mut found = None;
        level.entities.store.for_each_in(&area, false, |id| {
            if found.is_none()
                && level
                    .entities
                    .store
                    .get(id)
                    .is_some_and(|e| e.uuid() == uuid)
            {
                found = Some(id);
            }
        });
        found
    };
    let in_range = |level: &Level, id: EntityId| -> bool {
        let Some(e) = level.entities.store.get(id) else {
            return false;
        };
        let p = e.pos();
        let b = BlockPos::containing(p.x, p.y, p.z);
        let d = |a: i32, b: i32| f64::from(a) - f64::from(b);
        d(pos.x, b.x).powi(2) + d(pos.y, b.y).powi(2) + d(pos.z, b.z).powi(2) < 64.0
    };
    let alive = |level: &Level, id: EntityId| -> bool {
        level
            .entities
            .mob(id)
            .is_some_and(crate::mob::MobData::is_alive)
            || level
                .entities
                .player(id)
                .is_some_and(|p| !p.survival.is_dead_or_dying())
    };
    let target = if !c.hunting {
        None
    } else if let Some(uuid) = c.target {
        find(level, uuid).filter(|&id| alive(level, id) && in_range(level, id))
    } else {
        // `selectNewTarget`: a random enemy in water or rain in the 8-block box.
        let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
        let area = Aabb::new(x - 8.0, y - 8.0, z - 8.0, x + 9.0, y + 9.0, z + 9.0);
        let mut candidates = Vec::new();
        level.entities.store.for_each_in(&area, false, |id| {
            let enemy = level.entities.store.get(id).is_some_and(|e| {
                crate::mob::spawn::Category::of(e.kind())
                    == Some(crate::mob::spawn::Category::Monster)
            });
            if enemy && level.entities.mob(id).is_some() {
                candidates.push(id);
            }
        });
        candidates.retain(|&id| {
            let wet = level.entities.mob(id).is_some_and(|m| m.fluids.in_water());
            let Some(e) = level.entities.store.get(id) else {
                return false;
            };
            let p = e.pos();
            wet || level.is_raining_at(BlockPos::containing(p.x, p.y, p.z))
                || level.is_raining_at(BlockPos::containing(p.x, e.aabb().max_y, p.z))
        });
        if candidates.is_empty() {
            None
        } else {
            let n = i32::try_from(candidates.len()).unwrap_or(i32::MAX);
            let pick = usize::try_from(level.random().next_int_bounded(n)).unwrap_or(0);
            candidates.get(pick).copied()
        }
    };
    if let Some(id) = target
        && let Some(p) = level.entities.store.get(id).map(|e| e.pos())
    {
        let sound = crate::block::sound_id("minecraft:block.conduit.attack.target");
        level.play_sound(None, p.x, p.y, p.z, sound, SOUND_BLOCKS, 1.0, 1.0);
        level.output().magic_hurts.push((id, 4.0));
    }
    c.target = target.and_then(|id| level.entities.store.get(id).map(|e| e.uuid()));
    if c.target != previous {
        level.send_block_updated(pos);
    }
}
