//! `Cat` and `Ocelot`: taming and trust with fish, cats lying on beds and sitting on chests
//! and furnaces, relaxing on a sleeping owner, the ocelot's pounce attack, variants.

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, Vec3};
use vanillite_nbt::Compound;
use vanillite_world::entity::{EntityFlags, EntityId, Pose};

use super::{
    Cond, G, Goal as FamGoal, Interact, SpawnContext, TemptItems, common, consume, dirty, event,
    fam, guard, in_tag, owned_by, st, st_mut,
};
use crate::level::Level;
use crate::mob::animal::Interaction;
use crate::mob::common::{CommonGoal, MoveToBlock, TargetFilter};
use crate::mob::fam::{Hurt, SpawnReason};
use crate::mob::goals::{flag, reduced};
use crate::mob::kinds::Kind;
use crate::mob::{MobData, Tick, nav};
use crate::player::encode::{DataWriter, serializer};

/// `cat_variant` ids (alphabetical): all_black, black, british_shorthair, calico, jellie,
/// persian, ragdoll, red, siamese, tabby, white.
const CAT_VARIANTS: [&str; 11] = [
    "all_black",
    "black",
    "british_shorthair",
    "calico",
    "jellie",
    "persian",
    "ragdoll",
    "red",
    "siamese",
    "tabby",
    "white",
];
/// `CatVariants.DEFAULT` (black).
pub const CAT_DEFAULT: i32 = 1;
const CAT_SOUNDS: [&str; 2] = ["classic", "royal"];

/// The feline goals.
#[derive(Clone, Copy, Debug)]
pub enum Goal {
    /// `CatRelaxOnOwnerGoal`.
    RelaxOnOwner {
        owner: Option<EntityId>,
        goal_pos: Option<BlockPos>,
        on_bed_ticks: i32,
    },
    /// `CatLieOnBlockGoal`.
    LieOnBlock(MoveToBlock),
    /// `CatSitOnBlockGoal`.
    SitOnBlock(MoveToBlock),
    /// `OcelotAttackGoal`.
    Attack {
        target: Option<EntityId>,
        attack_time: i32,
    },
}

pub fn flags(g: &Goal) -> u8 {
    match g {
        Goal::RelaxOnOwner { .. } => 0,
        Goal::LieOnBlock(_) | Goal::SitOnBlock(_) => flag::MOVE | flag::JUMP,
        Goal::Attack { .. } => flag::MOVE | flag::LOOK,
    }
}

pub fn every_tick(g: &Goal) -> bool {
    matches!(
        g,
        Goal::LieOnBlock(_) | Goal::SitOnBlock(_) | Goal::Attack { .. }
    )
}

fn rabbit(t: &Tick<'_, '_>, id: EntityId) -> bool {
    crate::mob::common::mob_of(t, id, Kind::Rabbit)
}

fn chicken(t: &Tick<'_, '_>, id: EntityId) -> bool {
    crate::mob::common::mob_of(t, id, Kind::Chicken)
}

/// `EntitySelector.NO_CREATIVE_OR_SPECTATOR` players.
fn survival_player(t: &Tick<'_, '_>, id: EntityId) -> bool {
    crate::mob::common::is_survival_player(t, id)
}

/// `registerGoals` of cats and ocelots (with the avoid goal `reassess*Goals` adds).
pub fn register_goals(m: &mut MobData) {
    let cat = m.kind == Kind::Cat;
    let food = if cat {
        "minecraft:cat_food"
    } else {
        "minecraft:ocelot_food"
    };
    let g = &mut m.goals;
    if cat {
        g.add(1, G::Float);
        g.add(
            1,
            fam(FamGoal::TamePanic {
                speed: 1.5,
                env_only: false,
                pos: Vec3::ZERO,
            }),
        );
        g.add(2, G::SitWhenOrdered);
        g.add(
            3,
            fam(FamGoal::Feline(Goal::RelaxOnOwner {
                owner: None,
                goal_pos: None,
                on_bed_ticks: 0,
            })),
        );
        g.add(4, G::tempt(0.6, TemptItems::Tag(food)));
        let mut lie = MoveToBlock::new(1.1, 8, 6);
        lie.vertical_search_start = -2;
        g.add(5, fam(FamGoal::Feline(Goal::LieOnBlock(lie))));
        g.add(6, G::follow_owner(1.0, 10.0, 5.0));
        g.add(
            7,
            fam(FamGoal::Feline(Goal::SitOnBlock(MoveToBlock::new(
                0.8, 8, 1,
            )))),
        );
    } else {
        g.add(1, G::Float);
        g.add(3, G::tempt(0.6, TemptItems::Tag(food)));
    }
    g.add(
        if cat { 8 } else { 7 },
        G::Leap {
            yd: 0.3,
            target: None,
        },
    );
    g.add(
        if cat { 9 } else { 8 },
        fam(FamGoal::Feline(Goal::Attack {
            target: None,
            attack_time: 0,
        })),
    );
    g.add(if cat { 10 } else { 9 }, G::breed(0.8));
    g.add(
        if cat { 11 } else { 10 },
        G::stroll_with(0.8, 1.000_000_1e-5),
    );
    g.add(if cat { 12 } else { 11 }, G::look_at_player(10.0));
    let players: TargetFilter = survival_player;
    let cond = if cat {
        Cond::NotTame
    } else {
        Cond::NotTrusting
    };
    g.add(4, guard(cond, CommonGoal::avoid(players, 16.0, 0.8, 1.33)));
    let tg = &mut m.targets;
    let turtles: TargetFilter = crate::mob::common::baby_turtle_on_land;
    if cat {
        let rabbits: TargetFilter = rabbit;
        tg.add(
            1,
            guard(Cond::NotTame, CommonGoal::nearest(10, false, rabbits)),
        );
        tg.add(
            1,
            guard(Cond::NotTame, CommonGoal::nearest(10, false, turtles)),
        );
    } else {
        let chickens: TargetFilter = chicken;
        tg.add(1, common(CommonGoal::nearest(10, false, chickens)));
        tg.add(1, common(CommonGoal::nearest(10, false, turtles)));
    }
    m.nav.can_float = true;
}

/// The cat variant of a spawn (`VariantUtils.selectVariantToSpawn` over `cat_variant`): all
/// black in swamp huts (`#cats_spawn_as_black`), else a random variant (all black only under
/// a full moon). The cat spawner of villages and swamp huts uses it.
pub fn pick_cat_variant(
    random: &mut LegacyRandom,
    in_black_cat_structure: bool,
    full_moon: bool,
) -> i32 {
    if in_black_cat_structure {
        return 0;
    }
    if full_moon {
        random.next_int_bounded(11)
    } else {
        1 + random.next_int_bounded(10)
    }
}

/// `finalizeSpawn` of cats and ocelots.
pub fn finalize_spawn(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
    if m.kind == Kind::Ocelot {
        // `AgeableMobGroupData(1.0F)`: all but the first of a group are kittens.
        super::ageable_group(m, ctx, true, 1.0);
        return;
    }
    super::ageable_group(m, ctx, true, 0.05);
    m.variant = pick_cat_variant(&mut m.random, false, false);
    st_mut(m).sound_variant = u8::try_from(m.random.next_int_bounded(2)).unwrap_or(0);
}

/// A cat sound of its sound variant (`entity.cat.*`, `entity.cat_royal.*`,
/// `entity.baby_cat.*`).
pub fn cat_sound(m: &MobData, which: &str) -> &'static str {
    const KEYS: [&str; 9] = [
        "ambient",
        "beg_for_food",
        "death",
        "eat",
        "hiss",
        "hurt",
        "purr",
        "purreow",
        "stray_ambient",
    ];
    static TABLE: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut out = Vec::new();
        for prefix in ["cat", "cat_royal", "baby_cat"] {
            for k in KEYS {
                out.push(format!("minecraft:entity.{prefix}.{k}"));
            }
        }
        out
    });
    let set = if m.baby {
        2
    } else {
        usize::from(st(m).sound_variant.min(1))
    };
    let k = KEYS.iter().position(|x| *x == which).unwrap_or(0);
    table
        .get(set * KEYS.len() + k)
        .map_or("minecraft:entity.cat.ambient", String::as_str)
}

/// `Cat.getAmbientSound`.
pub fn cat_ambient_sound(m: &MobData) -> Option<&'static str> {
    Some(if m.tame {
        if m.in_love > 0 {
            cat_sound(m, "purr")
        } else if m.tick_count % 4 == 0 {
            // `random.nextInt(4) == 0` (the tick count stands in for the roll).
            cat_sound(m, "purreow")
        } else {
            cat_sound(m, "ambient")
        }
    } else {
        cat_sound(m, "stray_ambient")
    })
}

/// `Cat.tick`: begging while tempted and purring while lying.
pub fn cat_tick(t: &mut Tick<'_, '_>) {
    let tempted = t
        .mob_ref()
        .goals
        .slots
        .iter()
        .any(|s| s.running && matches!(s.goal, G::Tempt { .. }));
    if tempted && !t.mob_ref().tame && t.mob_ref().tick_count % 100 == 0 {
        let s = cat_sound(t.mob_ref(), "beg_for_food");
        crate::mob::kinds::play_sound(t, s, 1.0, 1.0);
    }
    let s = st(t.mob_ref());
    if (s.lying || s.relax_one) && t.mob_ref().tick_count % 5 == 0 {
        let v = {
            let r = &mut t.mob().random;
            0.6 + 0.4 * (r.next_float() - r.next_float())
        };
        let s = cat_sound(t.mob_ref(), "purr");
        crate::mob::kinds::play_sound(t, s, v, 1.0);
    }
}

/// `customServerAiStep`: crouching while sneaking up, sprinting when attacking.
pub fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
    let ctl = t.mob_ref().move_ctl;
    let (pose, sprint) = if ctl.has_wanted() {
        #[allow(clippy::float_cmp)]
        if ctl.speed_modifier == 0.6 {
            (Pose::Crouching, false)
        } else if ctl.speed_modifier == 1.33 {
            (Pose::Standing, true)
        } else {
            (Pose::Standing, false)
        }
    } else {
        (Pose::Standing, false)
    };
    let mut e = t.core_mut();
    if e.as_ref().pose() != pose {
        e.set_pose(pose);
    }
    e.set_flags(EntityFlags::CROUCHING, pose == Pose::Crouching);
    e.set_flags(EntityFlags::SPRINTING, sprint);
}

/// `Cat.mobInteract`.
pub fn cat_interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    let food = crate::mob::fam::is_food(Kind::Cat, ix.stack.item);
    if m.tame {
        if owned_by(ix) {
            if in_tag(ix.stack, "minecraft:cat_collar_dyes") {
                if let Some(c) = super::dye_color(ix.stack)
                    && c != st(m).collar
                {
                    if let Some(m) = ix.ents.mob_mut(ix.target) {
                        st_mut(m).collar = c;
                        m.persistent = true;
                    }
                    consume(ix);
                    dirty(ix);
                    return Interaction::Success;
                }
            } else if food && m.health < m.max_health() {
                let s = cat_sound(m, "eat");
                super::feed(ix, 1.0, 1.0, Some(s));
                return Interaction::Success;
            }
            let r = super::animal_interact(ix);
            if !r.consumes() {
                if let Some(m) = ix.ents.mob_mut(ix.target) {
                    m.order_to_sit = !m.order_to_sit;
                }
                return Interaction::Success;
            }
            return r;
        }
    } else if food {
        consume(ix);
        let owner = super::player_uuid(ix);
        let tamed = ix
            .ents
            .mob_mut(ix.target)
            .is_some_and(|m| m.random.next_int_bounded(3) == 0);
        if tamed {
            if let (Some(m), Some(o)) = (ix.ents.mob_mut(ix.target), owner) {
                super::tame_by(m, o);
                m.order_to_sit = true;
            }
            event(ix, 7);
        } else {
            event(ix, 6);
        }
        if let Some(m) = ix.ents.mob_mut(ix.target) {
            m.persistent = true;
        }
        let s = ix.ents.mob(ix.target).map(|m| cat_sound(m, "eat"));
        if let Some(s) = s {
            super::sound(ix, s, 1.0, 1.0);
        }
        dirty(ix);
        return Interaction::Success;
    }
    let r = super::animal_interact(ix);
    if r.consumes()
        && let Some(m) = ix.ents.mob_mut(ix.target)
    {
        m.persistent = true;
    }
    r
}

/// `Ocelot.mobInteract`: fish within 3 blocks may win its trust.
pub fn ocelot_interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    let tempted = m
        .goals
        .slots
        .iter()
        .any(|s| s.running && matches!(s.goal, G::Tempt { .. }));
    let close = match (ix.ents.store.get(ix.player), ix.ents.store.get(ix.target)) {
        (Some(p), Some(o)) => p.pos().distance_to_sqr(o.pos()) < 9.0,
        _ => false,
    };
    if tempted && !st(m).trusting && crate::mob::fam::is_food(Kind::Ocelot, ix.stack.item) && close
    {
        consume(ix);
        let trust = ix
            .ents
            .mob_mut(ix.target)
            .is_some_and(|m| m.random.next_int_bounded(3) == 0);
        if trust {
            if let Some(m) = ix.ents.mob_mut(ix.target) {
                st_mut(m).trusting = true;
            }
            event(ix, 41);
        } else {
            event(ix, 40);
        }
        dirty(ix);
        return Interaction::Success;
    }
    super::animal_interact(ix)
}

/// Ocelots forget nothing when hurt; kept for the hook table.
pub fn ocelot_after_hurt(_h: &mut Hurt<'_, '_>) {}

/// `checkOcelotSpawnRules` and `checkSpawnObstruction`: two in three, above sea level on grass
/// or leaves.
pub fn ocelot_spawn_rules(
    level: &Level,
    pos: BlockPos,
    random: &mut LegacyRandom,
    _reason: SpawnReason,
) -> bool {
    if random.next_int_bounded(3) == 0 {
        return false;
    }
    if pos.y < crate::mob::sea_level(level) {
        return false;
    }
    let below = level.block(BlockPos::new(pos.x, pos.y - 1, pos.z));
    below.block() == vanillite_data::block::ids::GRASS_BLOCK
        || below.block().name().ends_with("_leaves")
}

/// The cat's synched data.
pub fn cat_encode(w: &mut DataWriter<'_>, m: &MobData) {
    super::encode_tamable(w, m);
    let s = st(m);
    super::encode_holder(w, 20, serializer::CAT_VARIANT, m.variant);
    w.boolean(21, s.lying, false);
    w.boolean(22, s.relax_one, false);
    w.int(23, i32::from(s.collar), 14);
    super::encode_holder(
        w,
        24,
        serializer::CAT_SOUND_VARIANT,
        i32::from(s.sound_variant),
    );
}

/// `Cat.addAdditionalSaveData`.
pub fn cat_save(m: &MobData, out: &mut Compound) {
    let v = CAT_VARIANTS
        .get(usize::try_from(m.variant).unwrap_or(1))
        .copied()
        .unwrap_or("black");
    out.insert("variant", format!("minecraft:{v}"));
    let sv = CAT_SOUNDS
        .get(usize::from(st(m).sound_variant))
        .copied()
        .unwrap_or("classic");
    out.insert("sound_variant", format!("minecraft:{sv}"));
    out.insert("CollarColor", i8::try_from(st(m).collar).unwrap_or(14));
}

/// `Cat.readAdditionalSaveData`.
pub fn cat_load(m: &mut MobData, tag: &Compound) {
    if let Some(v) = super::string(tag, "variant") {
        let v = v.strip_prefix("minecraft:").unwrap_or(v);
        if let Some(i) = CAT_VARIANTS.iter().position(|n| *n == v) {
            m.variant = i32::try_from(i).unwrap_or(CAT_DEFAULT);
        }
    }
    if let Some(v) = super::string(tag, "sound_variant") {
        let v = v.strip_prefix("minecraft:").unwrap_or(v);
        if let Some(i) = CAT_SOUNDS.iter().position(|n| *n == v) {
            st_mut(m).sound_variant = u8::try_from(i).unwrap_or(0);
        }
    }
    st_mut(m).collar =
        u8::try_from(super::int(tag, "CollarColor").unwrap_or(14).clamp(0, 15)).unwrap_or(14);
    m.in_sitting_pose = m.order_to_sit;
}

// --- Goals --------------------------------------------------------------------------------

fn is_bed(name: &str) -> bool {
    name.ends_with("_bed")
}

/// `CatLieOnBlockGoal.isValidTarget`: a bed with air above.
fn lie_target(t: &Tick<'_, '_>, pos: BlockPos) -> bool {
    t.level
        .block(BlockPos::new(pos.x, pos.y + 1, pos.z))
        .is_air()
        && is_bed(t.level.block(pos).block().name())
}

/// `CatSitOnBlockGoal.isValidTarget`: a closed chest, a lit furnace or a bed's foot.
fn sit_target(t: &Tick<'_, '_>, pos: BlockPos) -> bool {
    if !t
        .level
        .block(BlockPos::new(pos.x, pos.y + 1, pos.z))
        .is_air()
    {
        return false;
    }
    let state = t.level.block(pos);
    let name = state.block().name();
    match name {
        // The open count of the chest is not tracked; chests count as closed.
        "minecraft:chest" => true,
        "minecraft:furnace" => state.value("lit") == Some("true"),
        n if is_bed(n) => state.value("part") != Some("head"),
        _ => false,
    }
}

fn next_start_40(_t: &mut Tick<'_, '_>) -> i32 {
    40
}

fn spot_occupied(t: &Tick<'_, '_>, pos: BlockPos) -> bool {
    let c = Vec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    let area = vanillite_base::Aabb::new(c.x, c.y, c.z, c.x + 1.0, c.y + 1.0, c.z + 1.0)
        .inflate(2.0, 2.0, 2.0);
    let mut occupied = false;
    let me = t.id;
    let ents = &*t.ents;
    ents.store.for_each_in(&area, false, |id| {
        if id != me
            && let Some(m) = ents.mob(id)
            && m.kind == Kind::Cat
            && (st(m).lying || st(m).relax_one)
        {
            occupied = true;
        }
    });
    occupied
}

fn set_lying(t: &mut Tick<'_, '_>, on: bool) {
    if st(t.mob_ref()).lying != on {
        st_mut(t.mob()).lying = on;
        t.core_mut().mark_data_dirty();
    }
}

fn set_relax(t: &mut Tick<'_, '_>, on: bool) {
    if st(t.mob_ref()).relax_one != on {
        st_mut(t.mob()).relax_one = on;
        t.core_mut().mark_data_dirty();
    }
}

pub fn can_use(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    match g {
        Goal::RelaxOnOwner {
            owner, goal_pos, ..
        } => {
            let m = t.mob_ref();
            if !m.tame || m.order_to_sit {
                return false;
            }
            let Some(o) = crate::mob::tame::owner_of(t.ents, m) else {
                return false;
            };
            *owner = Some(o);
            let Some(oe) = t.ents.store.get(o) else {
                return false;
            };
            if oe.pose() != Pose::Sleeping || oe.pos().distance_to_sqr(t.pos()) > 100.0 {
                return false;
            }
            let p = oe.pos();
            let op = BlockPos::containing(p.x, p.y, p.z);
            let state = t.level.block(op);
            if !is_bed(state.block().name()) {
                return false;
            }
            let target = match state.value("facing") {
                Some("north") => BlockPos::new(op.x, op.y, op.z + 1),
                Some("south") => BlockPos::new(op.x, op.y, op.z - 1),
                Some("west") => BlockPos::new(op.x + 1, op.y, op.z),
                Some("east") => BlockPos::new(op.x - 1, op.y, op.z),
                _ => op,
            };
            *goal_pos = Some(target);
            !spot_occupied(t, target)
        }
        Goal::LieOnBlock(s) => {
            let m = t.mob_ref();
            m.tame && !m.order_to_sit && !st(m).lying && s.can_use(t, lie_target, next_start_40)
        }
        Goal::SitOnBlock(s) => {
            let m = t.mob_ref();
            m.tame
                && !m.order_to_sit
                && s.can_use(t, sit_target, MoveToBlock::default_next_start_tick)
        }
        Goal::Attack { target, .. } => {
            *target = t.mob_ref().target;
            target.is_some()
        }
    }
}

pub fn can_continue(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    match g {
        Goal::RelaxOnOwner {
            owner, goal_pos, ..
        } => {
            let m = t.mob_ref();
            let sleeping = owner
                .and_then(|o| t.ents.store.get(o))
                .is_some_and(|e| e.pose() == Pose::Sleeping);
            m.tame && !m.order_to_sit && sleeping && goal_pos.is_some_and(|p| !spot_occupied(t, p))
        }
        Goal::LieOnBlock(s) => s.can_continue(t, lie_target),
        Goal::SitOnBlock(s) => s.can_continue(t, sit_target),
        Goal::Attack { target, .. } => {
            let Some(o) = target.and_then(|id| t.other(id)) else {
                return false;
            };
            if o.pos.distance_to_sqr(t.pos()) > 225.0 {
                return false;
            }
            !t.mob_ref().nav.is_done() || t.mob_ref().target.is_some()
        }
    }
}

pub fn start(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::RelaxOnOwner { goal_pos, .. } => {
            if let Some(p) = *goal_pos {
                crate::mob::tame::set_in_sitting_pose(t, false);
                let at = Vec3::new(f64::from(p.x), f64::from(p.y), f64::from(p.z));
                nav::move_to_pos(t, at, f64::from(1.1_f32));
            }
        }
        Goal::LieOnBlock(s) | Goal::SitOnBlock(s) => {
            s.start(t);
            crate::mob::tame::set_in_sitting_pose(t, false);
        }
        Goal::Attack { .. } => {}
    }
}

pub fn stop(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::RelaxOnOwner { on_bed_ticks, .. } => {
            set_lying(t, false);
            // The morning gift needs the owner's sleep timer, which players do not track yet.
            *on_bed_ticks = 0;
            set_relax(t, false);
            t.mob().nav.stop();
        }
        Goal::LieOnBlock(_) => set_lying(t, false),
        Goal::SitOnBlock(_) => crate::mob::tame::set_in_sitting_pose(t, false),
        Goal::Attack { target, .. } => {
            *target = None;
            t.mob().nav.stop();
        }
    }
}

pub fn tick(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::RelaxOnOwner {
            owner,
            goal_pos,
            on_bed_ticks,
        } => {
            let (Some(o), Some(p)) = (*owner, *goal_pos) else {
                return;
            };
            crate::mob::tame::set_in_sitting_pose(t, false);
            let at = Vec3::new(f64::from(p.x), f64::from(p.y), f64::from(p.z));
            nav::move_to_pos(t, at, f64::from(1.1_f32));
            let Some(op) = t.other(o) else {
                return;
            };
            if op.pos.distance_to_sqr(t.pos()) < 2.5 {
                *on_bed_ticks += 1;
                if *on_bed_ticks > reduced(16) {
                    set_lying(t, true);
                    set_relax(t, false);
                } else {
                    crate::mob::kinds::look_at_entity(t, op, 45.0, 45.0);
                    set_relax(t, true);
                }
            } else {
                set_lying(t, false);
            }
        }
        Goal::LieOnBlock(s) => {
            let target = BlockPos::new(s.block_pos.x, s.block_pos.y + 1, s.block_pos.z);
            s.tick(t, target, 1.0);
            crate::mob::tame::set_in_sitting_pose(t, false);
            let reached = s.reached_target;
            set_lying(t, reached);
        }
        Goal::SitOnBlock(s) => {
            let target = BlockPos::new(s.block_pos.x, s.block_pos.y + 1, s.block_pos.z);
            s.tick(t, target, 1.0);
            let reached = s.reached_target;
            crate::mob::tame::set_in_sitting_pose(t, reached);
        }
        Goal::Attack {
            target,
            attack_time,
        } => {
            let Some(id) = *target else {
                return;
            };
            let Some(o) = t.other(id) else {
                return;
            };
            t.mob()
                .look
                .set_look_at(Vec3::new(o.pos.x, o.eye_y, o.pos.z), 30.0, 30.0);
            let w = f64::from(t.core().dims().width) * 2.0;
            let melee = w * w;
            let d = o.pos.distance_to_sqr(t.pos());
            let speed = if d > melee && d < 16.0 {
                1.33
            } else if d < 225.0 {
                0.6
            } else {
                0.8
            };
            nav::move_to_entity(t, o.pos, 1, speed);
            *attack_time = (*attack_time - 1).max(0);
            if d <= melee && *attack_time <= 0 {
                *attack_time = 20;
                crate::mob::combat::mob_attack(t, id);
            }
        }
    }
}
