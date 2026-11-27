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

/// `TrailParticleOption(target, color, duration)`.
pub fn trail(target: Vec3, color: i32, duration: i32) -> Particle {
    let mut options = Vec::with_capacity(32);
    options.extend_from_slice(&target.x.to_be_bytes());
    options.extend_from_slice(&target.y.to_be_bytes());
    options.extend_from_slice(&target.z.to_be_bytes());
    options.extend_from_slice(&color.to_be_bytes());
    write_varint(&mut options, duration);
    Particle {
        kind: kind("minecraft:trail"),
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

    /// `broadcastEntityEvent(entity, event)` for the players near the entity.
    pub fn broadcast_entity_event(&mut self, entity: vanillite_world::entity::EntityId, event: i8) {
        let Some((pos, network_id)) = self
            .entities
            .store
            .get(entity)
            .map(|e| (e.pos(), e.network_id().0))
        else {
            return;
        };
        self.output.nearby_packets.push((
            pos,
            160.0,
            vanillite_protocol::SharedPacket::of(
                &vanillite_protocol::packets::play::clientbound::EntityEvent {
                    entity_id: network_id,
                    event,
                },
            ),
        ));
    }

    /// `sendParticles` with all of the packet's fields.
    pub fn send_particles_packet(&mut self, packet: LevelParticles) {
        self.output.particles.push(packet);
    }
}

/// `BubbleColumnBlock.entityInside` of an entity without a behaviour of its own (items,
/// experience orbs, falling blocks), at a column it is precisely in: the new velocity and
/// whether the fall distance resets (`onInsideBubbleColumn`). At the top of the column
/// (`onAboveBubbleColumn`) the level random draws the splash and bubble particles.
pub fn bubble_column_drag(
    level: &mut Level,
    pos: vanillite_base::BlockPos,
    v: Vec3,
) -> (Vec3, bool) {
    let state = level.block(pos);
    let drag_down = state.value("drag") == Some("true");
    let above = level.block(vanillite_base::BlockPos::new(pos.x, pos.y + 1, pos.z));
    let nothing_above = above.collision_shape().is_empty() && above.fluid_state().is_empty();
    let yd = match (nothing_above, drag_down) {
        (true, true) => (v.y - 0.03).max(-0.9),
        (true, false) => (v.y + 0.1).min(1.8),
        (false, true) => (v.y - 0.03).max(-0.3),
        (false, false) => (v.y + 0.06).min(0.7),
    };
    if nothing_above {
        bubble_column_particles(level, pos);
    }
    (Vec3::new(v.x, yd, v.z), !nothing_above)
}

/// `Entity.sendBubbleColumnParticles`: splashes and bubbles at random spots on the top of the
/// column at `pos`, drawn from the level random.
pub fn bubble_column_particles(level: &mut Level, pos: vanillite_base::BlockPos) {
    use vanillite_base::random::RandomSource;
    let (x, y, z) = (f64::from(pos.x), f64::from(pos.y + 1), f64::from(pos.z));
    for _ in 0..2 {
        let at = Vec3::new(
            x + level.random().next_double(),
            y,
            z + level.random().next_double(),
        );
        level.send_particles(simple("minecraft:splash"), at, 1, Vec3::ZERO, 1.0);
        let at = Vec3::new(
            x + level.random().next_double(),
            y,
            z + level.random().next_double(),
        );
        level.send_particles(
            simple("minecraft:bubble"),
            at,
            1,
            Vec3::new(0.0, 0.01, 0.0),
            0.2,
        );
    }
}
