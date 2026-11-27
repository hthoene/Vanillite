//! Brain memories (`MemoryModuleType`, `MemorySlot`, `ExpirableValue`, `WalkTarget`,
//! `PositionTracker`, `NearestVisibleLivingEntities`).
//!
//! A brain only has slots for the memories it registered (from its sensors, behaviours and
//! explicit list); setting an unregistered memory does nothing, like in Vanilla.

use vanillite_base::{BlockPos, Vec3};
use vanillite_world::entity::EntityId;

use crate::survival::damage::DamageKind;

/// The kind of value a memory type holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    Unit,
    Bool,
    Int,
    Long,
    Entity,
    Entities,
    Visible,
    Uuid,
    Uuids,
    Pos,
    GlobalPos,
    GlobalPositions,
    Vec3,
    Walk,
    Look,
    Damage,
}

/// `MemoryStatus`: what a behaviour or activity requires of a memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryStatus {
    ValuePresent,
    ValueAbsent,
    Registered,
}

/// A position in a dimension (`GlobalPos`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalPos {
    pub dimension: Box<str>,
    pub pos: BlockPos,
}

/// `PositionTracker`: an entity (`EntityTracker`) or a fixed position (`BlockPosTracker`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tracker {
    /// `EntityTracker(entity, trackEyeHeight, targetEyeHeight)`.
    Entity {
        id: EntityId,
        track_eye: bool,
        target_eye: bool,
    },
    /// `BlockPosTracker`: the block and the position looked at.
    Block { pos: BlockPos, center: Vec3 },
}

impl Tracker {
    /// `new EntityTracker(entity, trackEyeHeight)`.
    pub fn entity(id: EntityId, track_eye: bool) -> Tracker {
        Tracker::Entity {
            id,
            track_eye,
            target_eye: false,
        }
    }

    /// `new BlockPosTracker(pos)`: the centre of the block.
    pub fn block(pos: BlockPos) -> Tracker {
        Tracker::Block {
            pos,
            center: Vec3::new(
                f64::from(pos.x) + 0.5,
                f64::from(pos.y) + 0.5,
                f64::from(pos.z) + 0.5,
            ),
        }
    }

    /// `new BlockPosTracker(vec)`.
    pub fn at(v: Vec3) -> Tracker {
        Tracker::Block {
            pos: BlockPos::containing(v.x, v.y, v.z),
            center: v,
        }
    }

    /// The tracked entity.
    pub fn entity_id(&self) -> Option<EntityId> {
        match *self {
            Tracker::Entity { id, .. } => Some(id),
            Tracker::Block { .. } => None,
        }
    }
}

/// `WalkTarget`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WalkTarget {
    pub target: Tracker,
    pub speed: f32,
    pub close_enough: i32,
}

impl WalkTarget {
    /// `new WalkTarget(vec, speed, closeEnough)`: a `BlockPosTracker` of the block holding
    /// `vec` (its centre, not `vec` itself).
    pub fn at(v: Vec3, speed: f32, close_enough: i32) -> WalkTarget {
        WalkTarget {
            target: Tracker::block(BlockPos::containing(v.x, v.y, v.z)),
            speed,
            close_enough,
        }
    }

    /// `new WalkTarget(blockPos, speed, closeEnough)`.
    pub fn block(pos: BlockPos, speed: f32, close_enough: i32) -> WalkTarget {
        WalkTarget {
            target: Tracker::block(pos),
            speed,
            close_enough,
        }
    }

    /// `new WalkTarget(entity, speed, closeEnough)` (an `EntityTracker` without eye height).
    pub fn entity(id: EntityId, speed: f32, close_enough: i32) -> WalkTarget {
        WalkTarget {
            target: Tracker::entity(id, false),
            speed,
            close_enough,
        }
    }
}

/// `NearestVisibleLivingEntities`: the nearby living entities by distance and the lazily
/// computed, cached line of sight test of each.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Visible {
    pub entities: Vec<EntityId>,
    /// Per entity: 0 not tested, 1 visible, 2 not visible.
    pub sight: Vec<u8>,
}

/// A memory value.
#[derive(Clone, Debug, PartialEq)]
pub enum MemValue {
    Unit,
    Bool(bool),
    Int(i32),
    Long(i64),
    Entity(EntityId),
    Entities(Vec<EntityId>),
    Visible(Visible),
    Uuid(u128),
    Uuids(Vec<u128>),
    Pos(BlockPos),
    GlobalPos(GlobalPos),
    GlobalPositions(Vec<GlobalPos>),
    Vec3(Vec3),
    Walk(WalkTarget),
    Look(Tracker),
    /// `DamageSource`: its type and the entity that caused it.
    Damage(DamageKind, Option<EntityId>),
}

impl MemValue {
    /// `isEmptyCollection`: empty lists clear the memory.
    pub fn is_empty_collection(&self) -> bool {
        match self {
            MemValue::Entities(v) => v.is_empty(),
            MemValue::Uuids(v) => v.is_empty(),
            MemValue::GlobalPositions(v) => v.is_empty(),
            _ => false,
        }
    }
}

/// `MemorySlot`: a value and its time to live (`i64::MAX` never expires).
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    pub memory: MemoryType,
    pub value: Option<MemValue>,
    pub ttl: i64,
}

impl Slot {
    pub const NEVER: i64 = i64::MAX;

    /// `MemorySlot.tick`.
    pub fn tick(&mut self) {
        if self.value.is_some() && self.ttl != Self::NEVER {
            if self.ttl <= 0 {
                self.clear();
            } else {
                self.ttl -= 1;
            }
        }
    }

    pub fn clear(&mut self) {
        self.value = None;
        self.ttl = Self::NEVER;
    }
}

/// Every memory type of Vanilla 26.3 (`MemoryModuleType`), in registration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MemoryType {
    Dummy,
    Home,
    JobSite,
    PotentialJobSite,
    MeetingPoint,
    SecondaryJobSite,
    NearestLivingEntities,
    NearestVisibleLivingEntities,
    VisibleVillagerBabies,
    NearestPlayers,
    NearestVisiblePlayer,
    NearestVisibleAttackablePlayer,
    NearestVisibleAttackablePlayers,
    WalkTarget,
    LookTarget,
    AttackTarget,
    AttackCoolingDown,
    InteractionTarget,
    BreedTarget,
    RideTarget,
    Path,
    DoorsToClose,
    NearestBed,
    HurtBy,
    HurtByEntity,
    AvoidTarget,
    NearestHostile,
    NearestAttackable,
    HidingPlace,
    HeardBellTime,
    CantReachWalkTargetSince,
    GolemDetectedRecently,
    DangerDetectedRecently,
    LastSlept,
    LastWoken,
    LastWorkedAtPoi,
    NearestVisibleAdult,
    NearestVisibleWantedItem,
    NearestVisibleNemesis,
    PlayDeadTicks,
    TemptingPlayer,
    TemptationCooldownTicks,
    GazeCooldownTicks,
    LongJumpCooldownTicks,
    LongJumpMidJump,
    HasHuntingCooldown,
    RamCooldownTicks,
    RamTarget,
    IsInWater,
    IsPregnant,
    IsPanicking,
    UnreachableTongueTargets,
    VisitedBlockPositions,
    UnreachableTransportBlockPositions,
    TransportItemsCooldownTicks,
    ChargeCooldownTicks,
    AttackTargetCooldown,
    SpearFleeingTime,
    SpearFleeingPosition,
    SpearChargePosition,
    SpearEngageTime,
    SpearStatus,
    AngryAt,
    UniversalAnger,
    AdmiringItem,
    TimeTryingToReachAdmireItem,
    DisableWalkToAdmireItem,
    AdmiringDisabled,
    HuntedRecently,
    CelebrateLocation,
    Dancing,
    NearestVisibleHuntableHoglin,
    NearestVisibleBabyHoglin,
    NearestTargetablePlayerNotWearingGold,
    NearbyAdultPiglins,
    NearestVisibleAdultPiglins,
    NearestVisibleAdultHoglins,
    NearestVisibleAdultPiglin,
    NearestVisibleZombified,
    VisibleAdultPiglinCount,
    VisibleAdultHoglinCount,
    NearestPlayerHoldingWantedItem,
    AteRecently,
    NearestRepellent,
    Pacified,
    RoarTarget,
    DisturbanceLocation,
    RecentProjectile,
    IsSniffing,
    IsEmerging,
    RoarSoundDelay,
    DigCooldown,
    RoarSoundCooldown,
    SniffCooldown,
    TouchCooldown,
    VibrationCooldown,
    SonicBoomCooldown,
    SonicBoomSoundCooldown,
    SonicBoomSoundDelay,
    LikedPlayer,
    LikedNoteblockPosition,
    LikedNoteblockCooldownTicks,
    ItemPickupCooldownTicks,
    SnifferExploredPositions,
    SnifferSniffingTarget,
    SnifferDigging,
    SnifferHappy,
    BreezeJumpCooldown,
    BreezeShoot,
    BreezeShootCharging,
    BreezeShootRecovering,
    BreezeShootCooldown,
    BreezeJumpInhaling,
    BreezeJumpTarget,
    BreezeLeavingWater,
}

/// The number of memory types.
pub const MEMORY_COUNT: usize = 115;

impl MemoryType {
    /// All memory types by index.
    pub const ALL: [MemoryType; MEMORY_COUNT] = [
        MemoryType::Dummy,
        MemoryType::Home,
        MemoryType::JobSite,
        MemoryType::PotentialJobSite,
        MemoryType::MeetingPoint,
        MemoryType::SecondaryJobSite,
        MemoryType::NearestLivingEntities,
        MemoryType::NearestVisibleLivingEntities,
        MemoryType::VisibleVillagerBabies,
        MemoryType::NearestPlayers,
        MemoryType::NearestVisiblePlayer,
        MemoryType::NearestVisibleAttackablePlayer,
        MemoryType::NearestVisibleAttackablePlayers,
        MemoryType::WalkTarget,
        MemoryType::LookTarget,
        MemoryType::AttackTarget,
        MemoryType::AttackCoolingDown,
        MemoryType::InteractionTarget,
        MemoryType::BreedTarget,
        MemoryType::RideTarget,
        MemoryType::Path,
        MemoryType::DoorsToClose,
        MemoryType::NearestBed,
        MemoryType::HurtBy,
        MemoryType::HurtByEntity,
        MemoryType::AvoidTarget,
        MemoryType::NearestHostile,
        MemoryType::NearestAttackable,
        MemoryType::HidingPlace,
        MemoryType::HeardBellTime,
        MemoryType::CantReachWalkTargetSince,
        MemoryType::GolemDetectedRecently,
        MemoryType::DangerDetectedRecently,
        MemoryType::LastSlept,
        MemoryType::LastWoken,
        MemoryType::LastWorkedAtPoi,
        MemoryType::NearestVisibleAdult,
        MemoryType::NearestVisibleWantedItem,
        MemoryType::NearestVisibleNemesis,
        MemoryType::PlayDeadTicks,
        MemoryType::TemptingPlayer,
        MemoryType::TemptationCooldownTicks,
        MemoryType::GazeCooldownTicks,
        MemoryType::LongJumpCooldownTicks,
        MemoryType::LongJumpMidJump,
        MemoryType::HasHuntingCooldown,
        MemoryType::RamCooldownTicks,
        MemoryType::RamTarget,
        MemoryType::IsInWater,
        MemoryType::IsPregnant,
        MemoryType::IsPanicking,
        MemoryType::UnreachableTongueTargets,
        MemoryType::VisitedBlockPositions,
        MemoryType::UnreachableTransportBlockPositions,
        MemoryType::TransportItemsCooldownTicks,
        MemoryType::ChargeCooldownTicks,
        MemoryType::AttackTargetCooldown,
        MemoryType::SpearFleeingTime,
        MemoryType::SpearFleeingPosition,
        MemoryType::SpearChargePosition,
        MemoryType::SpearEngageTime,
        MemoryType::SpearStatus,
        MemoryType::AngryAt,
        MemoryType::UniversalAnger,
        MemoryType::AdmiringItem,
        MemoryType::TimeTryingToReachAdmireItem,
        MemoryType::DisableWalkToAdmireItem,
        MemoryType::AdmiringDisabled,
        MemoryType::HuntedRecently,
        MemoryType::CelebrateLocation,
        MemoryType::Dancing,
        MemoryType::NearestVisibleHuntableHoglin,
        MemoryType::NearestVisibleBabyHoglin,
        MemoryType::NearestTargetablePlayerNotWearingGold,
        MemoryType::NearbyAdultPiglins,
        MemoryType::NearestVisibleAdultPiglins,
        MemoryType::NearestVisibleAdultHoglins,
        MemoryType::NearestVisibleAdultPiglin,
        MemoryType::NearestVisibleZombified,
        MemoryType::VisibleAdultPiglinCount,
        MemoryType::VisibleAdultHoglinCount,
        MemoryType::NearestPlayerHoldingWantedItem,
        MemoryType::AteRecently,
        MemoryType::NearestRepellent,
        MemoryType::Pacified,
        MemoryType::RoarTarget,
        MemoryType::DisturbanceLocation,
        MemoryType::RecentProjectile,
        MemoryType::IsSniffing,
        MemoryType::IsEmerging,
        MemoryType::RoarSoundDelay,
        MemoryType::DigCooldown,
        MemoryType::RoarSoundCooldown,
        MemoryType::SniffCooldown,
        MemoryType::TouchCooldown,
        MemoryType::VibrationCooldown,
        MemoryType::SonicBoomCooldown,
        MemoryType::SonicBoomSoundCooldown,
        MemoryType::SonicBoomSoundDelay,
        MemoryType::LikedPlayer,
        MemoryType::LikedNoteblockPosition,
        MemoryType::LikedNoteblockCooldownTicks,
        MemoryType::ItemPickupCooldownTicks,
        MemoryType::SnifferExploredPositions,
        MemoryType::SnifferSniffingTarget,
        MemoryType::SnifferDigging,
        MemoryType::SnifferHappy,
        MemoryType::BreezeJumpCooldown,
        MemoryType::BreezeShoot,
        MemoryType::BreezeShootCharging,
        MemoryType::BreezeShootRecovering,
        MemoryType::BreezeShootCooldown,
        MemoryType::BreezeJumpInhaling,
        MemoryType::BreezeJumpTarget,
        MemoryType::BreezeLeavingWater,
    ];

    /// The registry id without namespace.
    pub fn name(self) -> &'static str {
        match self {
            MemoryType::Dummy => "dummy",
            MemoryType::Home => "home",
            MemoryType::JobSite => "job_site",
            MemoryType::PotentialJobSite => "potential_job_site",
            MemoryType::MeetingPoint => "meeting_point",
            MemoryType::SecondaryJobSite => "secondary_job_site",
            MemoryType::NearestLivingEntities => "mobs",
            MemoryType::NearestVisibleLivingEntities => "visible_mobs",
            MemoryType::VisibleVillagerBabies => "visible_villager_babies",
            MemoryType::NearestPlayers => "nearest_players",
            MemoryType::NearestVisiblePlayer => "nearest_visible_player",
            MemoryType::NearestVisibleAttackablePlayer => "nearest_visible_targetable_player",
            MemoryType::NearestVisibleAttackablePlayers => "nearest_visible_targetable_players",
            MemoryType::WalkTarget => "walk_target",
            MemoryType::LookTarget => "look_target",
            MemoryType::AttackTarget => "attack_target",
            MemoryType::AttackCoolingDown => "attack_cooling_down",
            MemoryType::InteractionTarget => "interaction_target",
            MemoryType::BreedTarget => "breed_target",
            MemoryType::RideTarget => "ride_target",
            MemoryType::Path => "path",
            MemoryType::DoorsToClose => "doors_to_close",
            MemoryType::NearestBed => "nearest_bed",
            MemoryType::HurtBy => "hurt_by",
            MemoryType::HurtByEntity => "hurt_by_entity",
            MemoryType::AvoidTarget => "avoid_target",
            MemoryType::NearestHostile => "nearest_hostile",
            MemoryType::NearestAttackable => "nearest_attackable",
            MemoryType::HidingPlace => "hiding_place",
            MemoryType::HeardBellTime => "heard_bell_time",
            MemoryType::CantReachWalkTargetSince => "cant_reach_walk_target_since",
            MemoryType::GolemDetectedRecently => "golem_detected_recently",
            MemoryType::DangerDetectedRecently => "danger_detected_recently",
            MemoryType::LastSlept => "last_slept",
            MemoryType::LastWoken => "last_woken",
            MemoryType::LastWorkedAtPoi => "last_worked_at_poi",
            MemoryType::NearestVisibleAdult => "nearest_visible_adult",
            MemoryType::NearestVisibleWantedItem => "nearest_visible_wanted_item",
            MemoryType::NearestVisibleNemesis => "nearest_visible_nemesis",
            MemoryType::PlayDeadTicks => "play_dead_ticks",
            MemoryType::TemptingPlayer => "tempting_player",
            MemoryType::TemptationCooldownTicks => "temptation_cooldown_ticks",
            MemoryType::GazeCooldownTicks => "gaze_cooldown_ticks",
            MemoryType::LongJumpCooldownTicks => "long_jump_cooling_down",
            MemoryType::LongJumpMidJump => "long_jump_mid_jump",
            MemoryType::HasHuntingCooldown => "has_hunting_cooldown",
            MemoryType::RamCooldownTicks => "ram_cooldown_ticks",
            MemoryType::RamTarget => "ram_target",
            MemoryType::IsInWater => "is_in_water",
            MemoryType::IsPregnant => "is_pregnant",
            MemoryType::IsPanicking => "is_panicking",
            MemoryType::UnreachableTongueTargets => "unreachable_tongue_targets",
            MemoryType::VisitedBlockPositions => "visited_block_positions",
            MemoryType::UnreachableTransportBlockPositions => {
                "unreachable_transport_block_positions"
            }
            MemoryType::TransportItemsCooldownTicks => "transport_items_cooldown_ticks",
            MemoryType::ChargeCooldownTicks => "charge_cooldown_ticks",
            MemoryType::AttackTargetCooldown => "attack_target_cooldown",
            MemoryType::SpearFleeingTime => "spear_fleeing_time",
            MemoryType::SpearFleeingPosition => "spear_fleeing_position",
            MemoryType::SpearChargePosition => "spear_charge_position",
            MemoryType::SpearEngageTime => "spear_engage_time",
            MemoryType::SpearStatus => "spear_status",
            MemoryType::AngryAt => "angry_at",
            MemoryType::UniversalAnger => "universal_anger",
            MemoryType::AdmiringItem => "admiring_item",
            MemoryType::TimeTryingToReachAdmireItem => "time_trying_to_reach_admire_item",
            MemoryType::DisableWalkToAdmireItem => "disable_walk_to_admire_item",
            MemoryType::AdmiringDisabled => "admiring_disabled",
            MemoryType::HuntedRecently => "hunted_recently",
            MemoryType::CelebrateLocation => "celebrate_location",
            MemoryType::Dancing => "dancing",
            MemoryType::NearestVisibleHuntableHoglin => "nearest_visible_huntable_hoglin",
            MemoryType::NearestVisibleBabyHoglin => "nearest_visible_baby_hoglin",
            MemoryType::NearestTargetablePlayerNotWearingGold => {
                "nearest_targetable_player_not_wearing_gold"
            }
            MemoryType::NearbyAdultPiglins => "nearby_adult_piglins",
            MemoryType::NearestVisibleAdultPiglins => "nearest_visible_adult_piglins",
            MemoryType::NearestVisibleAdultHoglins => "nearest_visible_adult_hoglins",
            MemoryType::NearestVisibleAdultPiglin => "nearest_visible_adult_piglin",
            MemoryType::NearestVisibleZombified => "nearest_visible_zombified",
            MemoryType::VisibleAdultPiglinCount => "visible_adult_piglin_count",
            MemoryType::VisibleAdultHoglinCount => "visible_adult_hoglin_count",
            MemoryType::NearestPlayerHoldingWantedItem => "nearest_player_holding_wanted_item",
            MemoryType::AteRecently => "ate_recently",
            MemoryType::NearestRepellent => "nearest_repellent",
            MemoryType::Pacified => "pacified",
            MemoryType::RoarTarget => "roar_target",
            MemoryType::DisturbanceLocation => "disturbance_location",
            MemoryType::RecentProjectile => "recent_projectile",
            MemoryType::IsSniffing => "is_sniffing",
            MemoryType::IsEmerging => "is_emerging",
            MemoryType::RoarSoundDelay => "roar_sound_delay",
            MemoryType::DigCooldown => "dig_cooldown",
            MemoryType::RoarSoundCooldown => "roar_sound_cooldown",
            MemoryType::SniffCooldown => "sniff_cooldown",
            MemoryType::TouchCooldown => "touch_cooldown",
            MemoryType::VibrationCooldown => "vibration_cooldown",
            MemoryType::SonicBoomCooldown => "sonic_boom_cooldown",
            MemoryType::SonicBoomSoundCooldown => "sonic_boom_sound_cooldown",
            MemoryType::SonicBoomSoundDelay => "sonic_boom_sound_delay",
            MemoryType::LikedPlayer => "liked_player",
            MemoryType::LikedNoteblockPosition => "liked_noteblock",
            MemoryType::LikedNoteblockCooldownTicks => "liked_noteblock_cooldown_ticks",
            MemoryType::ItemPickupCooldownTicks => "item_pickup_cooldown_ticks",
            MemoryType::SnifferExploredPositions => "sniffer_explored_positions",
            MemoryType::SnifferSniffingTarget => "sniffer_sniffing_target",
            MemoryType::SnifferDigging => "sniffer_digging",
            MemoryType::SnifferHappy => "sniffer_happy",
            MemoryType::BreezeJumpCooldown => "breeze_jump_cooldown",
            MemoryType::BreezeShoot => "breeze_shoot",
            MemoryType::BreezeShootCharging => "breeze_shoot_charging",
            MemoryType::BreezeShootRecovering => "breeze_shoot_recover",
            MemoryType::BreezeShootCooldown => "breeze_shoot_cooldown",
            MemoryType::BreezeJumpInhaling => "breeze_jump_inhaling",
            MemoryType::BreezeJumpTarget => "breeze_jump_target",
            MemoryType::BreezeLeavingWater => "breeze_leaving_water",
        }
    }

    /// The kind of value the memory holds.
    pub fn kind(self) -> ValueKind {
        match self {
            MemoryType::Dummy => ValueKind::Unit,
            MemoryType::Home => ValueKind::GlobalPos,
            MemoryType::JobSite => ValueKind::GlobalPos,
            MemoryType::PotentialJobSite => ValueKind::GlobalPos,
            MemoryType::MeetingPoint => ValueKind::GlobalPos,
            MemoryType::SecondaryJobSite => ValueKind::GlobalPositions,
            MemoryType::NearestLivingEntities => ValueKind::Entities,
            MemoryType::NearestVisibleLivingEntities => ValueKind::Visible,
            MemoryType::VisibleVillagerBabies => ValueKind::Entities,
            MemoryType::NearestPlayers => ValueKind::Entities,
            MemoryType::NearestVisiblePlayer => ValueKind::Entity,
            MemoryType::NearestVisibleAttackablePlayer => ValueKind::Entity,
            MemoryType::NearestVisibleAttackablePlayers => ValueKind::Entities,
            MemoryType::WalkTarget => ValueKind::Walk,
            MemoryType::LookTarget => ValueKind::Look,
            MemoryType::AttackTarget => ValueKind::Entity,
            MemoryType::AttackCoolingDown => ValueKind::Bool,
            MemoryType::InteractionTarget => ValueKind::Entity,
            MemoryType::BreedTarget => ValueKind::Entity,
            MemoryType::RideTarget => ValueKind::Entity,
            MemoryType::Path => ValueKind::Unit,
            MemoryType::DoorsToClose => ValueKind::GlobalPositions,
            MemoryType::NearestBed => ValueKind::Pos,
            MemoryType::HurtBy => ValueKind::Damage,
            MemoryType::HurtByEntity => ValueKind::Entity,
            MemoryType::AvoidTarget => ValueKind::Entity,
            MemoryType::NearestHostile => ValueKind::Entity,
            MemoryType::NearestAttackable => ValueKind::Entity,
            MemoryType::HidingPlace => ValueKind::GlobalPos,
            MemoryType::HeardBellTime => ValueKind::Long,
            MemoryType::CantReachWalkTargetSince => ValueKind::Long,
            MemoryType::GolemDetectedRecently => ValueKind::Bool,
            MemoryType::DangerDetectedRecently => ValueKind::Bool,
            MemoryType::LastSlept => ValueKind::Long,
            MemoryType::LastWoken => ValueKind::Long,
            MemoryType::LastWorkedAtPoi => ValueKind::Long,
            MemoryType::NearestVisibleAdult => ValueKind::Entity,
            MemoryType::NearestVisibleWantedItem => ValueKind::Entity,
            MemoryType::NearestVisibleNemesis => ValueKind::Entity,
            MemoryType::PlayDeadTicks => ValueKind::Int,
            MemoryType::TemptingPlayer => ValueKind::Entity,
            MemoryType::TemptationCooldownTicks => ValueKind::Int,
            MemoryType::GazeCooldownTicks => ValueKind::Int,
            MemoryType::LongJumpCooldownTicks => ValueKind::Int,
            MemoryType::LongJumpMidJump => ValueKind::Bool,
            MemoryType::HasHuntingCooldown => ValueKind::Bool,
            MemoryType::RamCooldownTicks => ValueKind::Int,
            MemoryType::RamTarget => ValueKind::Vec3,
            MemoryType::IsInWater => ValueKind::Unit,
            MemoryType::IsPregnant => ValueKind::Unit,
            MemoryType::IsPanicking => ValueKind::Bool,
            MemoryType::UnreachableTongueTargets => ValueKind::Uuids,
            MemoryType::VisitedBlockPositions => ValueKind::GlobalPositions,
            MemoryType::UnreachableTransportBlockPositions => ValueKind::GlobalPositions,
            MemoryType::TransportItemsCooldownTicks => ValueKind::Int,
            MemoryType::ChargeCooldownTicks => ValueKind::Int,
            MemoryType::AttackTargetCooldown => ValueKind::Int,
            MemoryType::SpearFleeingTime => ValueKind::Int,
            MemoryType::SpearFleeingPosition => ValueKind::Vec3,
            MemoryType::SpearChargePosition => ValueKind::Vec3,
            MemoryType::SpearEngageTime => ValueKind::Int,
            MemoryType::SpearStatus => ValueKind::Int,
            MemoryType::AngryAt => ValueKind::Uuid,
            MemoryType::UniversalAnger => ValueKind::Bool,
            MemoryType::AdmiringItem => ValueKind::Bool,
            MemoryType::TimeTryingToReachAdmireItem => ValueKind::Int,
            MemoryType::DisableWalkToAdmireItem => ValueKind::Bool,
            MemoryType::AdmiringDisabled => ValueKind::Bool,
            MemoryType::HuntedRecently => ValueKind::Bool,
            MemoryType::CelebrateLocation => ValueKind::Pos,
            MemoryType::Dancing => ValueKind::Bool,
            MemoryType::NearestVisibleHuntableHoglin => ValueKind::Entity,
            MemoryType::NearestVisibleBabyHoglin => ValueKind::Entity,
            MemoryType::NearestTargetablePlayerNotWearingGold => ValueKind::Entity,
            MemoryType::NearbyAdultPiglins => ValueKind::Entities,
            MemoryType::NearestVisibleAdultPiglins => ValueKind::Entities,
            MemoryType::NearestVisibleAdultHoglins => ValueKind::Entities,
            MemoryType::NearestVisibleAdultPiglin => ValueKind::Entity,
            MemoryType::NearestVisibleZombified => ValueKind::Entity,
            MemoryType::VisibleAdultPiglinCount => ValueKind::Int,
            MemoryType::VisibleAdultHoglinCount => ValueKind::Int,
            MemoryType::NearestPlayerHoldingWantedItem => ValueKind::Entity,
            MemoryType::AteRecently => ValueKind::Bool,
            MemoryType::NearestRepellent => ValueKind::Pos,
            MemoryType::Pacified => ValueKind::Bool,
            MemoryType::RoarTarget => ValueKind::Entity,
            MemoryType::DisturbanceLocation => ValueKind::Pos,
            MemoryType::RecentProjectile => ValueKind::Unit,
            MemoryType::IsSniffing => ValueKind::Unit,
            MemoryType::IsEmerging => ValueKind::Unit,
            MemoryType::RoarSoundDelay => ValueKind::Unit,
            MemoryType::DigCooldown => ValueKind::Unit,
            MemoryType::RoarSoundCooldown => ValueKind::Unit,
            MemoryType::SniffCooldown => ValueKind::Unit,
            MemoryType::TouchCooldown => ValueKind::Unit,
            MemoryType::VibrationCooldown => ValueKind::Unit,
            MemoryType::SonicBoomCooldown => ValueKind::Unit,
            MemoryType::SonicBoomSoundCooldown => ValueKind::Unit,
            MemoryType::SonicBoomSoundDelay => ValueKind::Unit,
            MemoryType::LikedPlayer => ValueKind::Uuid,
            MemoryType::LikedNoteblockPosition => ValueKind::GlobalPos,
            MemoryType::LikedNoteblockCooldownTicks => ValueKind::Int,
            MemoryType::ItemPickupCooldownTicks => ValueKind::Int,
            MemoryType::SnifferExploredPositions => ValueKind::GlobalPositions,
            MemoryType::SnifferSniffingTarget => ValueKind::Pos,
            MemoryType::SnifferDigging => ValueKind::Bool,
            MemoryType::SnifferHappy => ValueKind::Bool,
            MemoryType::BreezeJumpCooldown => ValueKind::Unit,
            MemoryType::BreezeShoot => ValueKind::Unit,
            MemoryType::BreezeShootCharging => ValueKind::Unit,
            MemoryType::BreezeShootRecovering => ValueKind::Unit,
            MemoryType::BreezeShootCooldown => ValueKind::Unit,
            MemoryType::BreezeJumpInhaling => ValueKind::Unit,
            MemoryType::BreezeJumpTarget => ValueKind::Pos,
            MemoryType::BreezeLeavingWater => ValueKind::Unit,
        }
    }

    /// Whether the memory is saved with the entity (`canSerialize`).
    pub fn serializable(self) -> bool {
        matches!(
            self,
            MemoryType::Home
                | MemoryType::JobSite
                | MemoryType::PotentialJobSite
                | MemoryType::MeetingPoint
                | MemoryType::GolemDetectedRecently
                | MemoryType::DangerDetectedRecently
                | MemoryType::LastSlept
                | MemoryType::LastWoken
                | MemoryType::LastWorkedAtPoi
                | MemoryType::PlayDeadTicks
                | MemoryType::TemptationCooldownTicks
                | MemoryType::GazeCooldownTicks
                | MemoryType::LongJumpCooldownTicks
                | MemoryType::HasHuntingCooldown
                | MemoryType::RamCooldownTicks
                | MemoryType::IsInWater
                | MemoryType::IsPregnant
                | MemoryType::IsPanicking
                | MemoryType::VisitedBlockPositions
                | MemoryType::UnreachableTransportBlockPositions
                | MemoryType::ChargeCooldownTicks
                | MemoryType::AttackTargetCooldown
                | MemoryType::AngryAt
                | MemoryType::UniversalAnger
                | MemoryType::AdmiringItem
                | MemoryType::AdmiringDisabled
                | MemoryType::HuntedRecently
                | MemoryType::RecentProjectile
                | MemoryType::IsSniffing
                | MemoryType::IsEmerging
                | MemoryType::RoarSoundDelay
                | MemoryType::DigCooldown
                | MemoryType::RoarSoundCooldown
                | MemoryType::SniffCooldown
                | MemoryType::TouchCooldown
                | MemoryType::VibrationCooldown
                | MemoryType::SonicBoomCooldown
                | MemoryType::SonicBoomSoundCooldown
                | MemoryType::SonicBoomSoundDelay
                | MemoryType::LikedPlayer
                | MemoryType::LikedNoteblockPosition
                | MemoryType::LikedNoteblockCooldownTicks
                | MemoryType::ItemPickupCooldownTicks
                | MemoryType::SnifferExploredPositions
                | MemoryType::BreezeJumpCooldown
                | MemoryType::BreezeShoot
                | MemoryType::BreezeShootCharging
                | MemoryType::BreezeShootRecovering
                | MemoryType::BreezeShootCooldown
                | MemoryType::BreezeJumpInhaling
                | MemoryType::BreezeJumpTarget
                | MemoryType::BreezeLeavingWater
        )
    }
}
