//! The kind state of the classic kinds (the kinds without a family module) that only one kind
//! has: the creeper's fuse, the sheep's wool, the chicken's eggs, the enderman's block. It
//! lives in the mob's [`Ext`](super::Ext) like a family's state, so it does not grow the
//! record every mob carries.

use vanillite_data::block::BlockState;

use super::Ext;
use crate::mob::MobData;
use crate::mob::kinds::Kind;

/// `Creeper`: the fuse and the charge.
#[derive(Clone, Debug, PartialEq)]
pub struct Creeper {
    pub swell: i32,
    pub old_swell: i32,
    /// `DATA_SWELL_DIR`: `-1` idle, `1` swelling.
    pub swell_dir: i32,
    /// `maxSwell` (`Fuse`).
    pub max_swell: i32,
    pub explosion_radius: i32,
    pub powered: bool,
    pub ignited: bool,
}

impl Creeper {
    const NEW: Creeper = Creeper {
        swell: 0,
        old_swell: 0,
        swell_dir: -1,
        max_swell: 30,
        explosion_radius: 3,
        powered: false,
        ignited: false,
    };
}

/// `Sheep`: the wool colour and the eat animation.
#[derive(Clone, Debug, PartialEq)]
pub struct Sheep {
    /// `DyeColor` id.
    pub color: u8,
    /// `eatAnimationTick`.
    pub eat_animation: i32,
}

impl Sheep {
    const NEW: Sheep = Sheep {
        color: 0,
        eat_animation: 0,
    };
}

/// `Chicken`: the egg timer and the jockey flag.
#[derive(Clone, Debug, PartialEq)]
pub struct Chicken {
    /// `eggTime`.
    pub egg_time: i32,
    /// `isChickenJockey`.
    pub jockey: bool,
    /// `flapSpeed`.
    pub flap_speed: f32,
}

impl Chicken {
    const NEW: Chicken = Chicken {
        egg_time: 0,
        jockey: false,
        flap_speed: 0.0,
    };
}

/// `EnderMan`: the carried block and the stare.
#[derive(Clone, Debug, PartialEq)]
pub struct Enderman {
    /// `DATA_CARRY_STATE`.
    pub carried: Option<BlockState>,
    /// `DATA_CREEPY`.
    pub creepy: bool,
    /// `targetChangeTime`.
    pub target_change_time: i32,
    /// `SPEED_MODIFIER_ATTACKING` (+0.15) is on.
    pub attack_speed: bool,
}

impl Enderman {
    const NEW: Enderman = Enderman {
        carried: None,
        creepy: false,
        target_change_time: 0,
        attack_speed: false,
    };
}

/// The state of a classic kind that has one.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Creeper(Creeper),
    Sheep(Sheep),
    Chicken(Chicken),
    Enderman(Enderman),
}

/// The fresh state of `kind`, if it is a classic kind with state.
pub(crate) fn new_ext(kind: Kind) -> Option<Ext> {
    let state = match kind {
        Kind::Creeper => State::Creeper(Creeper::NEW),
        Kind::Sheep => State::Sheep(Sheep::NEW),
        Kind::Chicken => State::Chicken(Chicken::NEW),
        Kind::Enderman => State::Enderman(Enderman::NEW),
        _ => return None,
    };
    Some(Ext::Classic(state))
}

static NO_CREEPER: Creeper = Creeper::NEW;
static NO_SHEEP: Sheep = Sheep::NEW;
static NO_CHICKEN: Chicken = Chicken::NEW;
static NO_ENDERMAN: Enderman = Enderman::NEW;

/// Read and write access to one classic kind's state: reads give the defaults for other mobs;
/// `*_mut` creates the state (only for a mob of that kind: it replaces any other kind state),
/// `try_*_mut` only changes a state the mob has.
macro_rules! access {
    ($get:ident, $get_mut:ident, $try_mut:ident, $variant:ident, $empty:ident) => {
        #[doc = concat!("The `", stringify!($variant), "` state (defaults for other mobs).")]
        pub fn $get(m: &MobData) -> &$variant {
            match &m.ext {
                Ext::Classic(State::$variant(s)) => s,
                _ => &$empty,
            }
        }

        #[doc = concat!("The `", stringify!($variant), "` state, mutably (created when missing).")]
        pub fn $get_mut(m: &mut MobData) -> &mut $variant {
            if !matches!(m.ext, Ext::Classic(State::$variant(_))) {
                m.ext = Ext::Classic(State::$variant($variant::NEW));
            }
            match &mut m.ext {
                Ext::Classic(State::$variant(s)) => s,
                _ => unreachable!("set above"),
            }
        }

        #[doc = concat!("The `", stringify!($variant), "` state, mutably, if the mob has it.")]
        pub fn $try_mut(m: &mut MobData) -> Option<&mut $variant> {
            match &mut m.ext {
                Ext::Classic(State::$variant(s)) => Some(s),
                _ => None,
            }
        }
    };
}

access!(creeper, creeper_mut, try_creeper_mut, Creeper, NO_CREEPER);
access!(sheep, sheep_mut, try_sheep_mut, Sheep, NO_SHEEP);
access!(chicken, chicken_mut, try_chicken_mut, Chicken, NO_CHICKEN);
access!(
    enderman,
    enderman_mut,
    try_enderman_mut,
    Enderman,
    NO_ENDERMAN
);
