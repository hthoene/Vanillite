//! Striders (`Strider`): lava walkers of the Nether that shiver out of warm blocks, carry a
//! saddle and are steered with a warped fungus on a stick.
//!
//! Standing on lava (`canStandOnFluid`) needs fluid collision in the shared movement; until
//! then `floatStrider` keeps them at the surface.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_world::entity::EntityFlags;

use super::{Goal, ageable_group_baby, block_tags, st, st_mut};
use crate::item::EquipmentSlot;
use crate::level::Level;
use crate::mob::animal::{Interaction, animal_interact, use_item};
use crate::mob::common::MoveToBlock;
use crate::mob::fam::{ExtraMob, FamGoal, Interact, Relation, SpawnContext, SpawnReason};
use crate::mob::goals::{Goal as MobGoal, TemptItems};
use crate::mob::kinds::{self, Kind};
use crate::mob::{Attrs, MobData, Tick, combat, equipment, is_lava};
use crate::survival::rules::Difficulty;

/// `Strider.registerGoals` (`LookAtPlayerGoal(Strider.class)` looks at players only here).
pub(super) fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    g.add(1, MobGoal::panic(1.65));
    g.add(2, MobGoal::breed(1.0));
    g.add(3, MobGoal::tempt(1.4, TemptItems::Food));
    g.add(
        4,
        MobGoal::Fam(FamGoal::Wildlife(Goal::StriderGoToLava(MoveToBlock::new(
            1.0, 8, 2,
        )))),
    );
    g.add(
        5,
        MobGoal::FollowParent {
            speed: 1.0,
            parent: None,
            recalc: 0,
        },
    );
    g.add(
        7,
        MobGoal::Stroll {
            speed: 1.0,
            interval: 60,
            probability: 1.0,
            wanted: Vec3::ZERO,
        },
    );
    g.add(8, MobGoal::look_at_player(8.0));
    g.add(8, MobGoal::random_look());
}

/// Whether the tempt goal runs (`isBeingTempted`).
pub(crate) fn is_tempted(m: &MobData) -> bool {
    m.goals
        .slots
        .iter()
        .any(|s| s.running && matches!(s.goal, MobGoal::Tempt { .. }))
}

/// `Strider.checkStriderSpawnRules`: on top of a lava column with air above.
pub(super) fn spawn_rules(level: &Level, pos: BlockPos) -> bool {
    let mut p = BlockPos::new(pos.x, pos.y + 1, pos.z);
    while is_lava(level.block(p)) && p.y < level.max_y() {
        p = BlockPos::new(p.x, p.y + 1, p.z);
    }
    level.block(p).is_air()
}

/// `Strider.finalizeSpawn`: jockeys (a zombified piglin or a baby strider) and babies.
pub(super) fn finalize_spawn(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
    if m.baby {
        return;
    }
    if m.random.next_int_bounded(30) == 0 && ctx.difficulty != Difficulty::Peaceful {
        let seed = m.random.next_long();
        let mut rider = MobData::new(Kind::ZombifiedPiglin, seed);
        let mut inner = SpawnContext {
            reason: SpawnReason::Jockey,
            extra: Vec::new(),
            ride: None,
            ..*ctx
        };
        kinds::finalize_spawn(&mut rider, &mut inner);
        rider.equipment.set(
            EquipmentSlot::MainHand,
            equipment::stack("minecraft:warped_fungus_on_a_stick"),
        );
        m.equipment
            .set(EquipmentSlot::Saddle, equipment::stack("minecraft:saddle"));
        m.equipment.set_guaranteed_drop(EquipmentSlot::Saddle);
        ctx.extra.push(ExtraMob {
            mob: rider,
            offset: Vec3::ZERO,
            relation: Relation::RidesIt,
        });
    } else if m.random.next_int_bounded(10) == 0 {
        let seed = m.random.next_long();
        let mut rider = MobData::new(Kind::Strider, seed);
        rider.age = -24000;
        rider.baby = true;
        let mut inner = SpawnContext {
            reason: SpawnReason::Jockey,
            extra: Vec::new(),
            ride: None,
            ..*ctx
        };
        kinds::finalize_spawn(&mut rider, &mut inner);
        ctx.extra.push(ExtraMob {
            mob: rider,
            offset: Vec3::ZERO,
            relation: Relation::RidesIt,
        });
    } else {
        ageable_group_baby(m, ctx, 0.5);
    }
}

/// Sets `DATA_SUFFOCATING` and the suffocating speed modifier (-34 % of the base).
fn set_suffocating(t: &mut Tick<'_, '_>, on: bool) {
    if st(t.mob_ref()).suffocating == on {
        return;
    }
    let base = Attrs::of(Kind::Strider.type_id()).movement_speed;
    let m = t.mob();
    st_mut(m).suffocating = on;
    m.attrs.movement_speed = if on { base - base * 0.34 } else { base };
    t.core_mut().mark_data_dirty();
}

/// `Strider.tick` before `super.tick()`: sounds and the warmth check.
pub(super) fn before_tick(t: &mut Tick<'_, '_>) {
    let id = t.id;
    if is_tempted(t.mob_ref()) && t.mob().random.next_int_bounded(140) == 0 {
        combat::mob_sound(t.ents, t.env, id, "minecraft:entity.strider.happy");
    } else if crate::mob::animal::is_panicking(t.mob_ref())
        && t.mob().random.next_int_bounded(60) == 0
    {
        combat::mob_sound(t.ents, t.env, id, "minecraft:entity.strider.retreat");
    }
    if t.mob_ref().no_ai {
        return;
    }
    let pos = t.block_pos();
    let inside = t.level.block(pos);
    let on = t.level.block(BlockPos::new(pos.x, pos.y - 1, pos.z));
    let tags = block_tags();
    let warm = tags.strider_warm_blocks.contains(inside)
        || tags.strider_warm_blocks.contains(on)
        || t.mob_ref().fluids.lava_height > 0.0;
    let on_warm_strider = t
        .ents
        .vehicle_of(t.id)
        .and_then(|v| t.ents.mob(v))
        .is_some_and(|v| v.kind == Kind::Strider && !st(v).suffocating);
    set_suffocating(t, !warm && !on_warm_strider);
}

/// `Strider.tick` after `super.tick()`: `floatStrider`.
pub(super) fn after_tick(t: &mut Tick<'_, '_>) {
    if !t.mob_ref().fluids.in_lava() {
        return;
    }
    let pos = t.block_pos();
    let above_lava = is_lava(t.level.block(BlockPos::new(pos.x, pos.y + 1, pos.z)));
    // `isAbove(liquid collision shape)`: the feet at or over the top half of the lava block.
    if t.pos().y >= f64::from(pos.y) + 0.5 - 1.0e-5 && !above_lava {
        t.core_mut().set_flags(EntityFlags::ON_GROUND, true);
    } else {
        let v = t.velocity() * 0.5;
        t.set_velocity(Vec3::new(v.x, v.y + 0.05, v.z));
    }
}

/// `Strider.mobInteract`: get on a saddled strider, feed it, or saddle it.
pub(super) fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let food = crate::mob::fam::is_food(Kind::Strider, ix.stack.item);
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    let saddled = !m.equipment.get(EquipmentSlot::Saddle).is_empty();
    let ridden = !ix.ents.riding.passengers_of(ix.target).is_empty();
    if !food && saddled && !ridden && !ix.sneaking {
        crate::riding::start_riding(ix.ents, ix.env, ix.player, ix.target, false);
        return Interaction::Success;
    }
    let r = animal_interact(ix.ents, ix.env, ix.player, ix.target, ix.hand, ix.stack);
    if r.consumes() {
        if food
            && let Some(m) = ix.ents.mob_mut(ix.target)
            && !m.silent
        {
            let pitch = 1.0 + (m.random.next_float() - m.random.next_float()) * 0.2;
            crate::mob::animal::sound_at(
                ix.ents,
                ix.env,
                ix.target,
                "minecraft:entity.strider.eat",
                vanillite_protocol::packets::play::clientbound::SoundSource::Neutral,
                pitch,
            );
        }
        return r;
    }
    // The saddle's `equippable` component (`equip_on_interact`).
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    if crate::item::item_id(ix.stack).name() == "minecraft:saddle"
        && m.is_alive()
        && !m.baby
        && !saddled
    {
        if let Some(m) = ix.ents.mob_mut(ix.target) {
            m.equipment
                .set(EquipmentSlot::Saddle, ix.stack.with_count(1));
            m.equipment.set_guaranteed_drop(EquipmentSlot::Saddle);
        }
        use_item(ix.ents, ix.player, ix.hand);
        crate::mob::animal::sound_at(
            ix.ents,
            ix.env,
            ix.target,
            "minecraft:entity.strider.saddle",
            vanillite_protocol::packets::play::clientbound::SoundSource::Neutral,
            1.0,
        );
        broadcast_equipment(ix);
        return Interaction::Success;
    }
    Interaction::Pass
}

/// `set_equipment` of the strider to its trackers.
fn broadcast_equipment(ix: &mut Interact<'_, '_>) {
    let Some(e) = ix.ents.store.get(ix.target) else {
        return;
    };
    let Some(m) = ix.ents.mob(ix.target) else {
        return;
    };
    let packet = vanillite_protocol::packets::play::clientbound::SetEquipment {
        entity_id: e.network_id().0,
        slots: equipment::SLOTS
            .iter()
            .map(|&s| (s.ordinal(), m.equipment.get(s).clone()))
            .collect(),
    };
    ix.env.out.push(crate::survival::Effect::Tracking {
        entity: ix.target,
        with_self: false,
        packet: vanillite_protocol::SharedPacket::of(&packet),
    });
}

/// `StriderGoToLavaGoal.isValidTarget`: lava with room to stand above.
fn lava_target(t: &Tick<'_, '_>, pos: BlockPos) -> bool {
    t.level.block(pos).block() == vanillite_data::block::ids::LAVA
        && t.level
            .block(BlockPos::new(pos.x, pos.y + 1, pos.z))
            .collision_shape()
            .is_empty()
}

pub(super) fn can_use(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    let Goal::StriderGoToLava(s) = g else {
        return false;
    };
    !t.mob_ref().fluids.in_lava() && s.can_use(t, lava_target, MoveToBlock::default_next_start_tick)
}

pub(super) fn can_continue(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    let Goal::StriderGoToLava(s) = g else {
        return false;
    };
    !t.mob_ref().fluids.in_lava() && lava_target(t, s.block_pos)
}
