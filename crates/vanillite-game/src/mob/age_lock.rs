//! The golden dandelion age lock of `AgeableMob` (and tadpoles): a baby used with a golden
//! dandelion stops or resumes growing up.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_protocol::packets::play::clientbound::SoundSource;

use vanillite_protocol::packets::play::serverbound::Hand;
use vanillite_world::entity::EntityId;

use super::animal::Interaction;
use super::{MobData, Tick};
use crate::entities::Entities;
use crate::item::ItemStack;
use crate::survival::player::Env;

/// `AGE_LOCK_COOLDOWN_TICKS`.
const COOLDOWN: u8 = 40;

/// `AgeableMob.canUseGoldenDandelion(item, isBaby, ageLockParticleTimer, mob)`.
pub fn can_use(m: &MobData, stack: &ItemStack, baby: bool) -> bool {
    Some(stack.item) == vanillite_data::registry::ITEM.id("minecraft:golden_dandelion")
        && baby
        && m.age_lock_timer == 0
        && !crate::effect::entity_in_tag(m.kind.type_id(), "minecraft:cannot_be_age_locked")
}

/// `AgeableMob.setAgeLocked(mob, ..)` with `setAgeLockedData`: toggles the lock, resets the age
/// with `reset_age` (`setAge(getBabyStartAge())`, tadpoles `setAge(0)`), uses up the item, makes
/// a locked mob persistent and plays the use or unuse sound.
pub fn toggle(
    ents: &mut Entities,
    env: &mut Env<'_>,
    player: EntityId,
    target: EntityId,
    hand: Hand,
    reset_age: impl FnOnce(&mut MobData),
) {
    let Some(m) = ents.mob_mut(target) else {
        return;
    };
    m.age_locked = !m.age_locked;
    reset_age(m);
    m.age_lock_timer = COOLDOWN;
    let locked = m.age_locked;
    if locked {
        m.persistent = true;
    }
    crate::mob::animal::use_item(ents, player, hand);
    let Some(p) = ents.store.get(target).map(|e| e.pos()) else {
        return;
    };
    if let Some(mut e) = ents.store.get_mut(target) {
        e.mark_data_dirty();
    }
    let b = BlockPos::containing(p.x, p.y, p.z);
    let at = Vec3::new(
        f64::from(b.x) + 0.5,
        f64::from(b.y) + 0.5,
        f64::from(b.z) + 0.5,
    );
    let sound = if locked {
        "minecraft:item.golden_dandelion.use"
    } else {
        "minecraft:item.golden_dandelion.unuse"
    };
    let seed = crate::survival::sound_seed();
    crate::survival::play_sound(
        env.out,
        None,
        at,
        sound,
        SoundSource::Players,
        1.0,
        1.0,
        seed,
    );
}

/// `AgeableMob.mobInteract`: the golden dandelion on a baby (`setAge(getBabyStartAge())`).
pub fn interact_pub(ix: &mut crate::mob::fam::Interact<'_, '_>) -> Option<Interaction> {
    interact(ix.ents, ix.env, ix.player, ix.target, ix.hand, ix.stack)
}

/// `AgeableMob.mobInteract` with raw arguments.
pub fn interact(
    ents: &mut Entities,
    env: &mut Env<'_>,
    player: EntityId,
    target: EntityId,
    hand: Hand,
    stack: &ItemStack,
) -> Option<Interaction> {
    let m = ents.mob(target)?;
    if !can_use(m, stack, m.baby) {
        return None;
    }
    let start = baby_start_age(m.kind);
    toggle(ents, env, player, target, hand, |m| {
        m.age = start;
        m.baby = start < 0;
    });
    Some(Interaction::Success)
}

/// `getBabyStartAge`: sniffers take twice as long.
pub fn baby_start_age(kind: super::Kind) -> i32 {
    if kind == super::Kind::Sniffer {
        -48_000
    } else {
        -24_000
    }
}

/// `AgeableMob.makeAgeLockedParticle`: a pause or reset particle every other tick of the
/// cooldown.
pub fn tick(t: &mut Tick<'_, '_>) {
    let timer = t.mob_ref().age_lock_timer;
    if timer == 0 {
        return;
    }
    if timer.is_multiple_of(2) {
        let locked = t.mob_ref().age_locked;
        let p = t.pos();
        let dims = t.core().dims();
        let (w, h) = (f64::from(dims.width), f64::from(dims.height));
        let r = &mut t.mob().random;
        let x = p.x + w * (2.0 * r.next_double() - 1.0);
        let y = p.y + h * (2.0 * r.next_double() - 1.0) * 0.2;
        let z = p.z + w * (2.0 * r.next_double() - 1.0);
        let at = Vec3::new(x, y + h + if locked { f64::from(0.2_f32) } else { 0.0 }, z);
        let name = if locked {
            "minecraft:pause_mob_growth"
        } else {
            "minecraft:reset_mob_growth"
        };
        let particle = crate::level::particles::simple(name);
        t.cx.send_particles(particle, at, 1, Vec3::ZERO, 0.0);
    }
    t.mob().age_lock_timer = timer - 1;
}
