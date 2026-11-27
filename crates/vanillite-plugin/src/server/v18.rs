//! [`Server`] functions added with ABI 1.8: extra worlds (Bukkit's `createWorld`,
//! `unloadWorld`, `getWorlds`) and moving players between worlds from any thread.

use vanillite_abi::{self as abi, status};

use super::Server;
use crate::ffi::read_string;
use crate::types::{Error, Location, Uuid, World};

/// The environment of a world: its dimension type. Since ABI 1.8.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Environment {
    /// `minecraft:overworld`.
    #[default]
    Normal,
    /// `minecraft:the_nether`.
    Nether,
    /// `minecraft:the_end`.
    TheEnd,
    /// A data pack's dimension type.
    Custom,
}

impl Environment {
    fn to_abi(self) -> u32 {
        match self {
            Environment::Normal => abi::environment::NORMAL,
            Environment::Nether => abi::environment::NETHER,
            Environment::TheEnd => abi::environment::THE_END,
            Environment::Custom => abi::environment::CUSTOM,
        }
    }

    fn from_abi(v: u32) -> Environment {
        match v {
            abi::environment::NORMAL => Environment::Normal,
            abi::environment::NETHER => Environment::Nether,
            abi::environment::THE_END => Environment::TheEnd,
            _ => Environment::Custom,
        }
    }
}

/// A new extra world (Bukkit's `WorldCreator`): its own folder, `level.dat`, seed and
/// generator. Since ABI 1.8.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldCreator {
    /// Name and folder (letters, digits, `_`, `-`, `.`); the dimension key is
    /// `minecraft:<name in lower case>`.
    pub name: String,
    /// Its dimension type (not [`Environment::Custom`]).
    pub environment: Environment,
    /// `normal`, `flat`, `void`, `amplified`, `large_biomes` or the key of a dimension whose
    /// generator it takes.
    pub generator: String,
    /// `None`: a random seed.
    pub seed: Option<i64>,
    /// Whether structures are generated.
    pub structures: bool,
}

impl WorldCreator {
    /// A normal world with Vanilla terrain, a random seed and structures.
    pub fn new(name: &str) -> WorldCreator {
        WorldCreator {
            name: name.to_owned(),
            environment: Environment::Normal,
            generator: "normal".to_owned(),
            seed: None,
            structures: true,
        }
    }

    /// With this environment.
    #[must_use]
    pub fn environment(mut self, environment: Environment) -> Self {
        self.environment = environment;
        self
    }

    /// With this generator.
    #[must_use]
    pub fn generator(mut self, generator: &str) -> Self {
        generator.clone_into(&mut self.generator);
        self
    }

    /// With this seed.
    #[must_use]
    pub fn seed(mut self, seed: i64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// With or without structures.
    #[must_use]
    pub fn structures(mut self, structures: bool) -> Self {
        self.structures = structures;
        self
    }
}

/// What a world is ([`Server::world_details`]). Since ABI 1.8.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldDetails {
    /// Its dimension type.
    pub environment: Environment,
    /// Whether it is loaded.
    pub loaded: bool,
    /// An extra world (own folder); otherwise a dimension of the default world.
    pub extra: bool,
    /// Its seed.
    pub seed: i64,
    /// Its folder (`level-name` for the default world's dimensions).
    pub folder: String,
}

impl Server {
    /// Creates an extra world, or loads it when its folder holds one (it keeps its seed and
    /// generator). It loads in the next global phase ([`abi::event::WORLD_LOAD`]); the world
    /// is usable from then on. A loaded world of that name is returned as it is. Since ABI 1.8.
    pub fn create_world(&self, creator: &WorldCreator) -> Result<World, Error> {
        let mut flags = 0;
        if creator.seed.is_some() {
            flags |= abi::world_create_flag::SEED;
        }
        if !creator.structures {
            flags |= abi::world_create_flag::NO_STRUCTURES;
        }
        let spec = abi::WorldCreate {
            size: abi::size_of::<abi::WorldCreate>(),
            name: abi::Str::new(&creator.name),
            environment: creator.environment.to_abi(),
            generator: abi::Str::new(&creator.generator),
            seed: creator.seed.unwrap_or(0),
            flags,
        };
        let mut world = 0;
        // SAFETY: valid host function, record and output.
        Error::check(unsafe { (self.api().world_create)(self.host.id, &spec, &mut world) })?;
        Ok(World(world))
    }

    /// Loads the extra world in the folder `name` (see [`Server::create_world`]). Since ABI 1.8.
    pub fn load_world(&self, name: &str) -> Result<World, Error> {
        let mut world = 0;
        // SAFETY: valid host function, string and output.
        Error::check(unsafe {
            (self.api().world_load)(self.host.id, abi::Str::new(name), &mut world)
        })?;
        Ok(World(world))
    }

    /// Unloads an extra world in the next global phase, its chunks saved with `save`; fails
    /// while players are in it ([`abi::event::WORLD_UNLOAD`] may still keep it). Since ABI 1.8.
    pub fn unload_world(&self, world: World, save: bool) -> Result<(), Error> {
        // SAFETY: valid host function.
        Error::check(unsafe { (self.api().world_unload)(self.host.id, world.0, u32::from(save)) })
    }

    /// The number of world indices, loaded or not; every [`World`] is below it. Since ABI 1.8.
    pub fn world_count(&self) -> u32 {
        let mut count = 0;
        // SAFETY: valid host function and output.
        let s = unsafe { (self.api().world_count)(self.host.id, &mut count) };
        if s == status::OK { count } else { 0 }
    }

    /// What a world is; `None` past the last world. Since ABI 1.8.
    pub fn world_details(&self, world: World) -> Option<WorldDetails> {
        let mut details = abi::WorldDetails {
            size: abi::size_of::<abi::WorldDetails>(),
            environment: 0,
            flags: 0,
            seed: 0,
        };
        // SAFETY: valid host function, outputs and buffer.
        let folder = read_string(|out| unsafe {
            (self.api().world_info)(self.host.id, world.0, &mut details, out)
        })
        .ok()?;
        Some(WorldDetails {
            environment: Environment::from_abi(details.environment),
            loaded: details.flags & abi::world_flag::LOADED != 0,
            extra: details.flags & abi::world_flag::EXTRA != 0,
            seed: details.seed,
            folder,
        })
    }

    /// Moves an online player to a place in any loaded world, from any thread: between the
    /// ticks of its world, to another world like `/tp` (with the teleport event). Since ABI 1.8.
    pub fn move_player(&self, player: Uuid, to: Location) -> Result<(), Error> {
        let to = to.to_abi();
        // SAFETY: valid host function and record.
        Error::check(unsafe { (self.api().player_move)(self.host.id, player.to_abi(), &to) })
    }
}
