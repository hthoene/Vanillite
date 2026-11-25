//! The status effects of a mob (`LivingEntity.activeEffects`): adding them with Vanilla's
//! update rules, their duration, the periodic effects (poison, wither, regeneration) and the
//! effects mob code asks for (fire resistance, water breathing, invisibility, slow falling,
//! levitation). Attribute effects scale the mob's attributes from their base values.
//!
//! The full effect system for players (particles, icons, potions) is #74; it builds on this
//! storage.

use vanillite_nbt::{Compound, List, Tag};

use super::{Attrs, MobData};

/// `minecraft:mob_effect` registry ids of the effects mobs use (protocol ids, 26.3).
pub mod id {
    pub const SPEED: u16 = 0;
    pub const SLOWNESS: u16 = 1;
    pub const HASTE: u16 = 2;
    pub const MINING_FATIGUE: u16 = 3;
    pub const STRENGTH: u16 = 4;
    pub const INSTANT_HEALTH: u16 = 5;
    pub const INSTANT_DAMAGE: u16 = 6;
    pub const JUMP_BOOST: u16 = 7;
    pub const NAUSEA: u16 = 8;
    pub const REGENERATION: u16 = 9;
    pub const RESISTANCE: u16 = 10;
    pub const FIRE_RESISTANCE: u16 = 11;
    pub const WATER_BREATHING: u16 = 12;
    pub const INVISIBILITY: u16 = 13;
    pub const BLINDNESS: u16 = 14;
    pub const NIGHT_VISION: u16 = 15;
    pub const HUNGER: u16 = 16;
    pub const WEAKNESS: u16 = 17;
    pub const POISON: u16 = 18;
    pub const WITHER: u16 = 19;
    pub const HEALTH_BOOST: u16 = 20;
    pub const ABSORPTION: u16 = 21;
    pub const SATURATION: u16 = 22;
    pub const GLOWING: u16 = 23;
    pub const LEVITATION: u16 = 24;
    pub const LUCK: u16 = 25;
    pub const UNLUCK: u16 = 26;
    pub const SLOW_FALLING: u16 = 27;
    pub const CONDUIT_POWER: u16 = 28;
    pub const DOLPHINS_GRACE: u16 = 29;
    pub const BAD_OMEN: u16 = 30;
    pub const HERO_OF_THE_VILLAGE: u16 = 31;
    pub const DARKNESS: u16 = 32;
    pub const TRIAL_OMEN: u16 = 33;
    pub const RAID_OMEN: u16 = 34;
    pub const WIND_CHARGED: u16 = 35;
    pub const WEAVING: u16 = 36;
    pub const OOZING: u16 = 37;
    pub const INFESTED: u16 = 38;
    pub const BREATH_OF_THE_NAUTILUS: u16 = 39;

    /// The registry name of an effect id.
    pub fn name(id: u16) -> Option<&'static str> {
        vanillite_data::registry::MOB_EFFECT
            .entries
            .get(usize::from(id))
            .copied()
    }

    /// The id of an effect name (`minecraft:poison` or `poison`).
    pub fn of(name: &str) -> Option<u16> {
        let full;
        let name = if name.contains(':') {
            name
        } else {
            full = format!("minecraft:{name}");
            &full
        };
        vanillite_data::registry::MOB_EFFECT.id(name)
    }
}

/// `MobEffectInstance`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectInstance {
    pub effect: u16,
    pub amplifier: u8,
    /// Ticks left; `-1` is infinite.
    pub duration: i32,
    pub ambient: bool,
    pub visible: bool,
    pub show_icon: bool,
}

impl EffectInstance {
    /// An effect with the defaults of `new MobEffectInstance(effect, duration, amplifier)`.
    pub fn new(effect: u16, duration: i32, amplifier: u8) -> EffectInstance {
        EffectInstance {
            effect,
            amplifier,
            duration,
            ambient: false,
            visible: true,
            show_icon: true,
        }
    }

    fn is_infinite(&self) -> bool {
        self.duration == -1
    }

    /// `MobEffectInstance.update(other)`: a stronger or longer effect replaces this one.
    fn update(&mut self, other: &EffectInstance) -> bool {
        let mut changed = false;
        if other.amplifier > self.amplifier {
            self.amplifier = other.amplifier;
            self.duration = other.duration;
            changed = true;
        } else if self.is_shorter_than(other) && other.amplifier == self.amplifier {
            self.duration = other.duration;
            changed = true;
        }
        if (!other.ambient && self.ambient) || changed {
            self.ambient = other.ambient;
            changed = true;
        }
        if other.visible != self.visible {
            self.visible = other.visible;
            changed = true;
        }
        if other.show_icon != self.show_icon {
            self.show_icon = other.show_icon;
            changed = true;
        }
        changed
    }

    fn is_shorter_than(&self, other: &EffectInstance) -> bool {
        !self.is_infinite() && (self.duration < other.duration || other.is_infinite())
    }
}

/// The active effects of a mob and its attributes without effect modifiers.
#[derive(Clone, Debug, Default)]
pub struct Effects {
    pub active: Vec<EffectInstance>,
    /// The attributes before attribute effects, set while one is active.
    base: Option<Attrs>,
}

impl Effects {
    /// `hasEffect`.
    pub fn has(&self, effect: u16) -> bool {
        self.active.iter().any(|e| e.effect == effect)
    }

    /// `getEffect`.
    pub fn get(&self, effect: u16) -> Option<&EffectInstance> {
        self.active.iter().find(|e| e.effect == effect)
    }

    /// The amplifier of an effect, `None` without it.
    pub fn amplifier(&self, effect: u16) -> Option<u8> {
        self.get(effect).map(|e| e.amplifier)
    }

    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }
}

/// Whether an effect changes attributes.
fn is_attribute_effect(effect: u16) -> bool {
    matches!(
        effect,
        id::SPEED | id::SLOWNESS | id::STRENGTH | id::WEAKNESS | id::HEALTH_BOOST
    )
}

/// `canBeAffected`: the undead ignore regeneration and poison, spiders poison.
pub fn can_be_affected(m: &MobData, effect: u16) -> bool {
    use super::Kind;
    let undead = matches!(
        m.kind,
        Kind::Zombie
            | Kind::Skeleton
            | Kind::Husk
            | Kind::Drowned
            | Kind::ZombieVillager
            | Kind::ZombifiedPiglin
            | Kind::Stray
            | Kind::Bogged
            | Kind::Parched
            | Kind::WitherSkeleton
            | Kind::Phantom
            | Kind::SkeletonHorse
            | Kind::ZombieHorse
            | Kind::Zoglin
            | Kind::ZombieNautilus
            | Kind::CamelHusk
    );
    if undead && matches!(effect, id::REGENERATION | id::POISON) {
        return false;
    }
    if matches!(m.kind, Kind::Spider | Kind::CaveSpider) && effect == id::POISON {
        return false;
    }
    // `Parched.canBeAffected` and `WitherSkeleton.canBeAffected`.
    if (m.kind == Kind::Parched && effect == id::WEAKNESS)
        || (m.kind == Kind::WitherSkeleton && effect == id::WITHER)
    {
        return false;
    }
    true
}

/// `addEffect`: adds the effect or updates the active one; `false` when it does not apply.
pub fn add(m: &mut MobData, effect: EffectInstance) -> bool {
    if !can_be_affected(m, effect.effect) {
        return false;
    }
    if let Some(e) = m
        .effects
        .active
        .iter_mut()
        .find(|e| e.effect == effect.effect)
    {
        let changed = e.update(&effect);
        if changed && is_attribute_effect(effect.effect) {
            refresh_attributes(m);
        }
        return changed;
    }
    m.effects.active.push(effect);
    if is_attribute_effect(effect.effect) {
        refresh_attributes(m);
    }
    instant(m, effect);
    true
}

/// `removeEffect`.
pub fn remove(m: &mut MobData, effect: u16) -> bool {
    let before = m.effects.active.len();
    m.effects.active.retain(|e| e.effect != effect);
    let removed = m.effects.active.len() != before;
    if removed && is_attribute_effect(effect) {
        refresh_attributes(m);
    }
    removed
}

/// `removeAllEffects` (milk).
pub fn clear(m: &mut MobData) {
    m.effects.active.clear();
    refresh_attributes(m);
}

/// Instant health and damage (`InstantenousMobEffect.applyInstantenousEffect` without a
/// source): health heals the living and hurts the undead, damage the other way round.
fn instant(m: &mut MobData, effect: EffectInstance) {
    if !matches!(effect.effect, id::INSTANT_HEALTH | id::INSTANT_DAMAGE) {
        return;
    }
    m.effects.active.retain(|e| e.effect != effect.effect);
    let undead = !can_be_affected(m, id::REGENERATION);
    let heals = (effect.effect == id::INSTANT_HEALTH) != undead;
    let level = i32::from(effect.amplifier);
    #[allow(clippy::cast_precision_loss)]
    if heals {
        let amount = (4 << level) as f32;
        m.health = (m.health + amount).min(m.max_health());
    } else {
        let amount = (6 << level) as f32;
        m.health = (m.health - amount).max(0.0);
    }
}

/// Scales the attributes by the active attribute effects (`MobEffect.addAttributeModifiers`).
fn refresh_attributes(m: &mut MobData) {
    let any = m
        .effects
        .active
        .iter()
        .any(|e| is_attribute_effect(e.effect));
    if !any {
        if let Some(base) = m.effects.base.take() {
            m.attrs.movement_speed = base.movement_speed;
            m.attrs.attack_damage = base.attack_damage;
            m.attrs.max_health = base.max_health;
            m.health = m.health.min(m.max_health());
        }
        return;
    }
    let base = *m.effects.base.get_or_insert(m.attrs);
    let level = |effect| {
        m.effects
            .amplifier(effect)
            .map_or(0.0, |a| f64::from(a) + 1.0)
    };
    // `MOVEMENT_SPEED`: speed +20 %, slowness -15 % per level (`ADD_MULTIPLIED_TOTAL`).
    let speed = 1.0 + 0.2 * level(id::SPEED);
    let slow = 1.0 - 0.15 * level(id::SLOWNESS);
    let movement_speed = (base.movement_speed * speed * slow).max(0.0);
    // `ATTACK_DAMAGE`: strength +3, weakness -4 per level (`ADD_VALUE`).
    let attack_damage =
        (base.attack_damage + 3.0 * level(id::STRENGTH) - 4.0 * level(id::WEAKNESS)).max(0.0);
    let max_health = base.max_health + 4.0 * level(id::HEALTH_BOOST);
    m.attrs.movement_speed = movement_speed;
    m.attrs.attack_damage = attack_damage;
    m.attrs.max_health = max_health;
    m.health = m.health.min(m.max_health());
}

/// What the effect tick of one mob asks the world to do.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TickResult {
    /// Magic damage (poison).
    pub magic: f32,
    /// Wither damage.
    pub wither: f32,
}

/// `tickEffects`: the periodic effects, then the durations. The caller deals the damage.
pub fn tick(m: &mut MobData) -> TickResult {
    let mut r = TickResult::default();
    if m.effects.active.is_empty() {
        return r;
    }
    let mut expired = false;
    let mut i = 0;
    while i < m.effects.active.len() {
        let e = m.effects.active[i];
        let left = if e.is_infinite() { 1 } else { e.duration };
        // `shouldApplyEffectTickThisTick`.
        let interval = |base: i32| base >> e.amplifier.min(31);
        let applies = |base: i32| {
            let n = interval(base);
            n <= 0 || left % n == 0
        };
        match e.effect {
            id::POISON if applies(25) && m.health > 1.0 => r.magic += 1.0,
            id::WITHER if applies(40) => r.wither += 1.0,
            id::REGENERATION if applies(50) && m.health < m.max_health() => {
                m.health = (m.health + 1.0).min(m.max_health());
            }
            _ => {}
        }
        let e = &mut m.effects.active[i];
        if !e.is_infinite() {
            e.duration -= 1;
            if e.duration <= 0 {
                let effect = e.effect;
                m.effects.active.remove(i);
                expired |= is_attribute_effect(effect);
                continue;
            }
        }
        i += 1;
    }
    if expired {
        refresh_attributes(m);
    }
    r
}

/// The `active_effects` list of entity NBT.
pub fn save(m: &MobData, out: &mut Compound) {
    if m.effects.active.is_empty() {
        return;
    }
    let mut list = Vec::new();
    for e in &m.effects.active {
        let Some(name) = id::name(e.effect) else {
            continue;
        };
        let mut c = Compound::new();
        c.insert("id", name);
        c.insert("amplifier", i8::try_from(e.amplifier).unwrap_or(i8::MAX));
        c.insert("duration", e.duration);
        c.insert("ambient", e.ambient);
        c.insert("show_particles", e.visible);
        c.insert("show_icon", e.show_icon);
        list.push(Tag::Compound(c));
    }
    out.insert("active_effects", List(list));
}

/// Reads `active_effects` (without applying instant effects again).
pub fn load(m: &mut MobData, tag: &Compound) {
    let Some(Tag::List(list)) = tag.get("active_effects") else {
        return;
    };
    for t in &list.0 {
        let Tag::Compound(c) = t else {
            continue;
        };
        let Some(effect) = c.get("id").and_then(Tag::as_str).and_then(id::of) else {
            continue;
        };
        let int = |k: &str| match c.get(k) {
            Some(Tag::Byte(v)) => Some(i32::from(*v)),
            Some(Tag::Int(v)) => Some(*v),
            _ => None,
        };
        let flag = |k: &str, d: bool| match c.get(k) {
            Some(Tag::Byte(v)) => *v != 0,
            _ => d,
        };
        m.effects.active.push(EffectInstance {
            effect,
            amplifier: u8::try_from(int("amplifier").unwrap_or(0).clamp(0, 255)).unwrap_or(0),
            duration: int("duration").unwrap_or(1),
            ambient: flag("ambient", false),
            visible: flag("show_particles", true),
            show_icon: flag("show_icon", true),
        });
    }
    refresh_attributes(m);
}
