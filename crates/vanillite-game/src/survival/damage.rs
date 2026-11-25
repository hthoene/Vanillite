//! Damage types (the `minecraft:damage_type` registry of the data packs and its tags) and damage
//! sources.

use vanillite_base::Vec3;
use vanillite_world::entity::EntityId;

/// `DamageScaling`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scaling {
    Never,
    WhenCausedByLivingNonPlayer,
    Always,
}

/// `DamageEffects`: which hurt sound a player makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effects {
    Hurt,
    Thorns,
    Drowning,
    Burning,
    Poking,
    Freezing,
}

impl Effects {
    /// `DamageEffects.sound()`.
    pub fn sound(self) -> &'static str {
        match self {
            Effects::Hurt => "minecraft:entity.player.hurt",
            Effects::Thorns => "minecraft:enchant.thorns.hit",
            Effects::Drowning => "minecraft:entity.player.hurt_drown",
            Effects::Burning => "minecraft:entity.player.hurt_on_fire",
            Effects::Poking => "minecraft:entity.player.hurt_sweet_berry_bush",
            Effects::Freezing => "minecraft:entity.player.hurt_freeze",
        }
    }
}

/// `DeathMessageType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeathMessageType {
    Default,
    FallVariants,
    IntentionalGameDesign,
}

/// Damage type tags the game checks (`DamageTypeTags`), as bits.
pub mod tag {
    pub const IS_FALL: u32 = 1 << 0;
    pub const IS_FIRE: u32 = 1 << 1;
    pub const IS_DROWNING: u32 = 1 << 2;
    pub const IS_FREEZING: u32 = 1 << 3;
    pub const IS_LIGHTNING: u32 = 1 << 4;
    pub const BYPASSES_INVULNERABILITY: u32 = 1 << 5;
    pub const BYPASSES_ARMOR: u32 = 1 << 6;
    pub const BYPASSES_COOLDOWN: u32 = 1 << 7;
    pub const BYPASSES_EFFECTS: u32 = 1 << 8;
    pub const BYPASSES_RESISTANCE: u32 = 1 << 9;
    pub const BYPASSES_ENCHANTMENTS: u32 = 1 << 10;
    pub const NO_IMPACT: u32 = 1 << 11;
    pub const NO_KNOCKBACK: u32 = 1 << 12;
    pub const ALWAYS_MOST_SIGNIFICANT_FALL: u32 = 1 << 13;
    pub const DAMAGES_HELMET: u32 = 1 << 14;
    pub const PANIC_CAUSES: u32 = 1 << 15;
    pub const IS_EXPLOSION: u32 = 1 << 16;
    pub const IS_PROJECTILE: u32 = 1 << 17;
    pub const BYPASSES_SHIELD: u32 = 1 << 18;

    /// Tag names and their bits.
    pub const NAMES: [(&str, u32); 19] = [
        ("minecraft:is_fall", IS_FALL),
        ("minecraft:is_fire", IS_FIRE),
        ("minecraft:is_drowning", IS_DROWNING),
        ("minecraft:is_freezing", IS_FREEZING),
        ("minecraft:is_lightning", IS_LIGHTNING),
        (
            "minecraft:bypasses_invulnerability",
            BYPASSES_INVULNERABILITY,
        ),
        ("minecraft:bypasses_armor", BYPASSES_ARMOR),
        ("minecraft:bypasses_cooldown", BYPASSES_COOLDOWN),
        ("minecraft:bypasses_effects", BYPASSES_EFFECTS),
        ("minecraft:bypasses_resistance", BYPASSES_RESISTANCE),
        ("minecraft:bypasses_enchantments", BYPASSES_ENCHANTMENTS),
        ("minecraft:no_impact", NO_IMPACT),
        ("minecraft:no_knockback", NO_KNOCKBACK),
        (
            "minecraft:always_most_significant_fall",
            ALWAYS_MOST_SIGNIFICANT_FALL,
        ),
        ("minecraft:damages_helmet", DAMAGES_HELMET),
        ("minecraft:panic_causes", PANIC_CAUSES),
        ("minecraft:is_explosion", IS_EXPLOSION),
        ("minecraft:is_projectile", IS_PROJECTILE),
        ("minecraft:bypasses_shield", BYPASSES_SHIELD),
    ];
}

/// One `minecraft:damage_type` entry.
#[derive(Clone, Debug, PartialEq)]
pub struct DamageType {
    /// Registry id, e.g. `minecraft:fall`.
    pub id: String,
    /// `message_id`.
    pub msg_id: String,
    pub scaling: Scaling,
    pub exhaustion: f32,
    pub effects: Effects,
    pub death_message_type: DeathMessageType,
    /// [`tag`] bits.
    pub tags: u32,
}

/// A damage type by its network id (index in the registry).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DamageKind(pub u16);

/// The damage types of the server, with the ids of those the game creates itself.
#[derive(Clone, Debug)]
pub struct DamageTypes {
    types: Vec<DamageType>,
    pub in_fire: DamageKind,
    pub lightning_bolt: DamageKind,
    pub on_fire: DamageKind,
    pub lava: DamageKind,
    pub hot_floor: DamageKind,
    pub in_wall: DamageKind,
    pub drown: DamageKind,
    pub starve: DamageKind,
    pub fall: DamageKind,
    pub stalagmite: DamageKind,
    pub out_of_world: DamageKind,
    pub generic: DamageKind,
    pub generic_kill: DamageKind,
    pub cramming: DamageKind,
    pub mob_attack: DamageKind,
    pub player_attack: DamageKind,
    pub arrow: DamageKind,
    pub explosion: DamageKind,
    pub player_explosion: DamageKind,
    pub magic: DamageKind,
    pub indirect_magic: DamageKind,
    pub wither: DamageKind,
    pub wither_skull: DamageKind,
    pub dragon_breath: DamageKind,
    pub sonic_boom: DamageKind,
    pub thorns: DamageKind,
    pub mob_projectile: DamageKind,
    pub fireworks: DamageKind,
    pub fly_into_wall: DamageKind,
}

/// The built-in types of Vanilla 26.3 with their values, for servers without data packs (tests).
const BUILT_IN: &[(&str, &str, Scaling, f32, Effects, DeathMessageType, u32)] = {
    use DeathMessageType as D;
    use Effects as E;
    use Scaling as S;
    &[
        (
            "minecraft:fall",
            "fall",
            S::WhenCausedByLivingNonPlayer,
            0.0,
            E::Hurt,
            D::FallVariants,
            tag::IS_FALL | tag::BYPASSES_ARMOR,
        ),
        (
            "minecraft:generic",
            "generic",
            S::WhenCausedByLivingNonPlayer,
            0.0,
            E::Hurt,
            D::Default,
            tag::BYPASSES_ARMOR,
        ),
        (
            "minecraft:generic_kill",
            "genericKill",
            S::WhenCausedByLivingNonPlayer,
            0.0,
            E::Hurt,
            D::Default,
            tag::BYPASSES_INVULNERABILITY
                | tag::BYPASSES_ARMOR
                | tag::BYPASSES_EFFECTS
                | tag::BYPASSES_RESISTANCE
                | tag::BYPASSES_ENCHANTMENTS
                | tag::BYPASSES_COOLDOWN,
        ),
        (
            "minecraft:out_of_world",
            "outOfWorld",
            S::WhenCausedByLivingNonPlayer,
            0.0,
            E::Hurt,
            D::Default,
            tag::BYPASSES_INVULNERABILITY
                | tag::BYPASSES_ARMOR
                | tag::BYPASSES_EFFECTS
                | tag::BYPASSES_RESISTANCE
                | tag::BYPASSES_ENCHANTMENTS,
        ),
    ]
};

impl DamageTypes {
    /// The types in network id order. Missing well-known types map to the first entry.
    pub fn new(types: Vec<DamageType>) -> DamageTypes {
        let find = |id: &str| {
            DamageKind(
                types
                    .iter()
                    .position(|t| t.id == id)
                    .and_then(|i| u16::try_from(i).ok())
                    .unwrap_or(0),
            )
        };
        DamageTypes {
            in_fire: find("minecraft:in_fire"),
            lightning_bolt: find("minecraft:lightning_bolt"),
            on_fire: find("minecraft:on_fire"),
            lava: find("minecraft:lava"),
            hot_floor: find("minecraft:hot_floor"),
            in_wall: find("minecraft:in_wall"),
            drown: find("minecraft:drown"),
            starve: find("minecraft:starve"),
            fall: find("minecraft:fall"),
            stalagmite: find("minecraft:stalagmite"),
            out_of_world: find("minecraft:out_of_world"),
            generic: find("minecraft:generic"),
            generic_kill: find("minecraft:generic_kill"),
            cramming: find("minecraft:cramming"),
            mob_attack: find("minecraft:mob_attack"),
            player_attack: find("minecraft:player_attack"),
            arrow: find("minecraft:arrow"),
            explosion: find("minecraft:explosion"),
            player_explosion: find("minecraft:player_explosion"),
            magic: find("minecraft:magic"),
            indirect_magic: find("minecraft:indirect_magic"),
            wither: find("minecraft:wither"),
            wither_skull: find("minecraft:wither_skull"),
            dragon_breath: find("minecraft:dragon_breath"),
            sonic_boom: find("minecraft:sonic_boom"),
            thorns: find("minecraft:thorns"),
            mob_projectile: find("minecraft:mob_projectile"),
            fireworks: find("minecraft:fireworks"),
            fly_into_wall: find("minecraft:fly_into_wall"),
            types,
        }
    }

    /// A few built-in types, for servers without data packs.
    pub fn built_in() -> DamageTypes {
        DamageTypes::new(
            BUILT_IN
                .iter()
                .map(
                    |&(id, msg, scaling, exhaustion, effects, dm, tags)| DamageType {
                        id: id.to_owned(),
                        msg_id: msg.to_owned(),
                        scaling,
                        exhaustion,
                        effects,
                        death_message_type: dm,
                        tags,
                    },
                )
                .collect(),
        )
    }

    /// The kind of a damage type id (`minecraft:thorns`).
    pub fn by_id(&self, id: &str) -> Option<DamageKind> {
        self.types
            .iter()
            .position(|t| t.id == id)
            .and_then(|i| u16::try_from(i).ok())
            .map(DamageKind)
    }

    /// The type of a kind.
    pub fn get(&self, kind: DamageKind) -> &DamageType {
        self.types
            .get(usize::from(kind.0))
            .or_else(|| self.types.first())
            .unwrap_or(&FALLBACK)
    }

    /// Whether a kind has a tag bit.
    #[inline]
    pub fn is(&self, kind: DamageKind, tag: u32) -> bool {
        self.get(kind).tags & tag != 0
    }
}

static FALLBACK: DamageType = DamageType {
    id: String::new(),
    msg_id: String::new(),
    scaling: Scaling::Never,
    exhaustion: 0.0,
    effects: Effects::Hurt,
    death_message_type: DeathMessageType::Default,
    tags: 0,
};

/// `DamageSource`: a type with the entities and position that cause it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DamageSource {
    pub kind: DamageKind,
    /// The responsible entity (`causingEntity`), e.g. the shooter.
    pub cause: Option<EntityId>,
    /// The entity that hit (`directEntity`), e.g. the arrow.
    pub direct: Option<EntityId>,
    /// `damageSourcePosition`.
    pub position: Option<Vec3>,
    /// The causing entity is a living entity other than a player (difficulty scaling).
    pub cause_is_mob: bool,
    /// Translation key of the causing entity's type, for death messages (`death.attack.mob`).
    pub cause_name: Option<&'static str>,
    /// `getSecondsToDisableBlocking` of the attacker's weapon (axes disable shields).
    pub disable_shield_seconds: f32,
}

impl DamageSource {
    /// A source without entities (environment damage).
    pub fn of(kind: DamageKind) -> DamageSource {
        DamageSource {
            kind,
            cause: None,
            direct: None,
            position: None,
            cause_is_mob: false,
            cause_name: None,
            disable_shield_seconds: 0.0,
        }
    }

    /// `scalesWithDifficulty` for sources without a causing entity.
    pub fn scales_with_difficulty(
        &self,
        types: &DamageTypes,
        cause_is_living_non_player: bool,
    ) -> bool {
        match types.get(self.kind).scaling {
            Scaling::Never => false,
            Scaling::WhenCausedByLivingNonPlayer => cause_is_living_non_player,
            Scaling::Always => true,
        }
    }
}
