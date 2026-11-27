//! Server particles (`ServerLevel.sendParticles`): one `level_particles` packet per call,
//! sent by the server to the players of the world whose block centre is closer than 32
//! blocks (512 with `override_limiter`).

use vanillite_base::Vec3;
use vanillite_protocol::codec::write_varint;
pub use vanillite_protocol::packets::play::clientbound::{LevelParticles, Particle};

use super::Level;

/// `RandomizationType.ALTERNATIVE_WITH_SPEED`: the deltas spread the speed instead of the
/// position.
pub const ALTERNATIVE_WITH_SPEED: i32 = 2;

/// The registry id of a particle type, 0 for an unknown name.
fn kind(name: &str) -> i32 {
    i32::from(
        vanillite_data::registry::PARTICLE_TYPE
            .id(name)
            .unwrap_or(0),
    )
}

/// A particle without options (`SimpleParticleType`).
pub fn simple(name: &str) -> Particle {
    Particle {
        kind: kind(name),
        options: Vec::new(),
    }
}

/// `ItemParticleOption(ITEM, item)`: a stack of one without component changes.
pub fn item(item: u16) -> Particle {
    let mut options = Vec::with_capacity(6);
    write_varint(&mut options, i32::from(item));
    write_varint(&mut options, 1);
    // An empty `DataComponentPatch`.
    write_varint(&mut options, 0);
    write_varint(&mut options, 0);
    Particle {
        kind: kind("minecraft:item"),
        options,
    }
}

/// `SpellParticleOption.create(type, color, power)`.
pub fn spell(name: &str, color: i32, power: f32) -> Particle {
    let mut options = Vec::with_capacity(8);
    options.extend_from_slice(&color.to_be_bytes());
    options.extend_from_slice(&power.to_be_bytes());
    Particle {
        kind: kind(name),
        options,
    }
}

/// The packet of `sendParticles(particle, false, false, x, y, z, count, xDist, yDist, zDist,
/// speed)`.
#[allow(clippy::cast_possible_truncation)]
pub fn packet(particle: Particle, pos: Vec3, count: i32, dist: Vec3, speed: f64) -> LevelParticles {
    let speed = speed as f32;
    LevelParticles {
        particle,
        override_limiter: false,
        always_show: false,
        x: pos.x,
        y: pos.y,
        z: pos.z,
        x_dist: dist.x as f32,
        y_dist: dist.y as f32,
        z_dist: dist.z as f32,
        x_max_speed: speed,
        y_max_speed: speed,
        z_max_speed: speed,
        count,
        randomization: 0,
    }
}

impl Level {
    /// `sendParticles(particle, x, y, z, count, xDist, yDist, zDist, speed)`.
    pub fn send_particles(
        &mut self,
        particle: Particle,
        pos: Vec3,
        count: i32,
        dist: Vec3,
        speed: f64,
    ) {
        self.output
            .particles
            .push(packet(particle, pos, count, dist, speed));
    }

    /// `sendParticles` with all of the packet's fields.
    pub fn send_particles_packet(&mut self, packet: LevelParticles) {
        self.output.particles.push(packet);
    }
}
