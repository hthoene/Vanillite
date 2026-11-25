//! `TestBlock`: the start, log, fail and accept blocks of block based game tests.

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::packets::play::serverbound::SetTestBlock;

use super::Editor;
use crate::block::BlockBehaviour;
use crate::level::{BlockEntity, Level};

/// `TestBlockMode`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Mode {
    /// `start`: powered by the test when it starts.
    #[default]
    Start,
    /// `log`: logs its message when powered.
    Log,
    /// `fail`: fails the test when powered.
    Fail,
    /// `accept`: passes the test when powered.
    Accept,
}

impl Mode {
    /// The modes in id order.
    pub const ALL: [Mode; 4] = [Mode::Start, Mode::Log, Mode::Fail, Mode::Accept];

    /// `getSerializedName`.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Start => "start",
            Mode::Log => "log",
            Mode::Fail => "fail",
            Mode::Accept => "accept",
        }
    }

    /// The mode of a serialized name.
    pub fn by_name(name: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.name() == name)
    }

    /// `BY_ID` with `OutOfBoundsStrategy.ZERO`.
    pub fn by_id(id: u8) -> Mode {
        Mode::ALL
            .get(usize::from(id))
            .copied()
            .unwrap_or(Mode::Start)
    }

    /// `getDisplayName`: `test_block.mode.<name>`.
    pub fn display_name(self) -> vanillite_protocol::text::Component {
        vanillite_protocol::text::Component::translatable(
            format!("test_block.mode.{}", self.name()),
            vec![],
        )
    }

    /// The `mode` of a test block state.
    pub fn of_state(state: BlockState) -> Mode {
        state
            .value("mode")
            .and_then(Mode::by_name)
            .unwrap_or_default()
    }
}

/// `TestBlockEntity`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TestBlock {
    /// `mode` (the constructor takes the block state's).
    pub mode: Mode,
    /// `message`.
    pub message: String,
    /// `powered`.
    pub powered: bool,
    /// `triggered`: not saved.
    pub triggered: bool,
}

impl TestBlock {
    /// `new TestBlockEntity(pos, state)`.
    pub fn new(state: BlockState) -> Self {
        Self {
            mode: Mode::of_state(state),
            ..Self::default()
        }
    }

    /// `loadAdditional`: a missing mode is `fail`.
    pub fn load(&mut self, nbt: &Compound) {
        self.mode = match nbt.get("mode") {
            Some(Tag::String(m)) => Mode::by_name(m).unwrap_or(Mode::Fail),
            _ => Mode::Fail,
        };
        self.message = match nbt.get("message") {
            Some(Tag::String(m)) => m.clone(),
            _ => String::new(),
        };
        self.powered = matches!(nbt.get("powered"), Some(Tag::Byte(b)) if *b != 0);
    }

    /// `saveAdditional`.
    pub fn save(&self, nbt: &mut Compound) {
        nbt.insert("mode", Tag::String(self.mode.name().to_owned()));
        nbt.insert("message", Tag::String(self.message.clone()));
        nbt.insert("powered", Tag::Byte(i8::from(self.powered)));
    }

    /// `getUpdateTag`: `saveCustomOnly`.
    pub fn update_tag(&self) -> Option<Compound> {
        let mut nbt = Compound::new();
        self.save(&mut nbt);
        Some(nbt)
    }
}

fn entity(level: &mut Level, pos: BlockPos) -> Option<&mut TestBlock> {
    match level.block_entity_mut(pos) {
        Some(BlockEntity::TestBlock(t)) => Some(t),
        _ => None,
    }
}

/// `TestBlockEntity.log`.
fn log(pos: BlockPos, block: &TestBlock) {
    if !block.message.trim().is_empty() {
        log::info!(
            "Test {} (at BlockPos{{x={}, y={}, z={}}}): {}",
            block.mode.name(),
            pos.x,
            pos.y,
            pos.z,
            block.message
        );
    }
}

/// `TestBlockEntity.trigger`: a start block powers its neighbours, the others remember the
/// trigger (log blocks log first).
pub fn trigger(level: &mut Level, pos: BlockPos) {
    let Some(block) = entity(level, pos) else {
        return;
    };
    if block.mode == Mode::Start {
        block.powered = true;
        let copy = block.clone();
        level.update_neighbors_at(pos, ids::TEST_BLOCK);
        // `willTickThisTick` only asks the tick queue; it changes nothing.
        log(pos, &copy);
    } else {
        if block.mode == Mode::Log {
            log(pos, block);
        }
        block.triggered = true;
    }
}

/// `TestBlockEntity.reset`: a start block turns off.
pub fn reset(level: &mut Level, pos: BlockPos) {
    let Some(block) = entity(level, pos) else {
        return;
    };
    block.triggered = false;
    if block.mode == Mode::Start {
        block.powered = false;
        level.update_neighbors_at(pos, ids::TEST_BLOCK);
    }
}

/// `TestBlockEntity.setMode`: the block state follows (`UPDATE_CLIENTS`).
pub fn set_mode(level: &mut Level, pos: BlockPos, mode: Mode) {
    let Some(block) = entity(level, pos) else {
        return;
    };
    block.mode = mode;
    let state = level.block(pos);
    if state.block() == ids::TEST_BLOCK
        && let Some(new) = state.with("mode", mode.name())
    {
        level.set_block(pos, new, crate::level::update::CLIENTS);
    }
}

/// The behaviour of the block.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::TEST_BLOCK {
        b.tick = Some(tick);
        b.neighbor_changed = Some(neighbor_changed);
        b.signal = Some(signal);
    }
}

/// `TestBlock.tick`: resets the block entity.
fn tick(level: &mut Level, _state: BlockState, pos: BlockPos) {
    reset(level, pos);
}

/// `TestBlock.neighborChanged`: every mode but start triggers on a rising signal.
fn neighbor_changed(
    level: &mut Level,
    _state: BlockState,
    pos: BlockPos,
    _from: BlockId,
    _m: bool,
) {
    let powered_now = level.has_neighbor_signal(pos);
    let Some(block) = entity(level, pos) else {
        return;
    };
    if block.mode == Mode::Start {
        return;
    }
    if powered_now && !block.powered {
        block.powered = true;
        trigger(level, pos);
    } else if !powered_now && block.powered {
        block.powered = false;
    }
}

/// `TestBlock.ownSignal` (`getSignal`): a powered start block emits 15.
fn signal(level: &Level, state: BlockState, pos: BlockPos, _direction: Direction) -> i32 {
    if Mode::of_state(state) != Mode::Start {
        return 0;
    }
    match level.block_entity(pos) {
        Some(BlockEntity::TestBlock(t)) if t.powered => 15,
        _ => 0,
    }
}

/// An assertion a block based test failed (`GameTestAssertException`, with the absolute and
/// relative position for `GameTestAssertPosException`).
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    /// The message.
    pub message: Box<vanillite_protocol::text::Component>,
    /// The absolute and the relative position it is about.
    pub at: Option<(BlockPos, BlockPos)>,
}

/// A block based test in the world (`BlockBasedTestInstance` with its `GameTestHelper`): the
/// structure's rotated origin, rotation and unrotated size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockBasedTest {
    /// `getTestOrigin`: the start corner.
    pub origin: BlockPos,
    /// The test's rotation.
    pub rotation: vanillite_worldgen::template::transform::Rotation,
    /// The size of the structure before its rotation (`getRelativeBounds`).
    pub size: [i32; 3],
}

impl BlockBasedTest {
    /// `absolutePos`.
    pub fn absolute(&self, rel: BlockPos) -> BlockPos {
        let p = BlockPos::new(
            self.origin.x + rel.x,
            self.origin.y + rel.y,
            self.origin.z + rel.z,
        );
        vanillite_worldgen::template::transform::transform(
            p,
            vanillite_worldgen::template::transform::Mirror::None,
            self.rotation,
            self.origin,
        )
    }

    fn fail(message: vanillite_protocol::text::Component) -> Failure {
        Failure {
            message: Box::new(message),
            at: None,
        }
    }

    fn missing(mode: Mode) -> Failure {
        Self::fail(vanillite_protocol::text::Component::translatable(
            "test_block.error.missing",
            vec![mode.display_name()],
        ))
    }

    /// The test blocks of `mode` in the structure (`forEveryBlockInStructure`: x fastest,
    /// then y, then z), as relative positions.
    pub fn test_blocks(&self, level: &Level, mode: Mode) -> Vec<BlockPos> {
        let [sx, sy, sz] = self.size;
        let mut out = Vec::new();
        for z in 0..sz {
            for y in 0..sy {
                for x in 0..sx {
                    let rel = BlockPos::new(x, y, z);
                    let state = level.block(self.absolute(rel));
                    if state.block() == ids::TEST_BLOCK && Mode::of_state(state) == mode {
                        out.push(rel);
                    }
                }
            }
        }
        out
    }

    /// `getBlockEntity(pos, TestBlockEntity.class)`.
    fn block(&self, level: &Level, rel: BlockPos) -> Result<TestBlock, Failure> {
        let abs = self.absolute(rel);
        let at = Some((abs, rel));
        match level.block_entity(abs) {
            Some(BlockEntity::TestBlock(t)) => Ok((**t).clone()),
            None => Err(Failure {
                message: Box::new(vanillite_protocol::text::Component::translatable(
                    "test.error.missing_block_entity",
                    vec![],
                )),
                at,
            }),
            Some(_) => {
                let kind = vanillite_data::registry::BLOCK_ENTITY_TYPE
                    .name(level.block(abs).block().info().block_entity)
                    .unwrap_or("minecraft:empty");
                Err(Failure {
                    message: Box::new(vanillite_protocol::text::Component::translatable_args(
                        "test.error.wrong_block_entity",
                        vec![vanillite_protocol::text::Arg::String(kind.to_owned())],
                    )),
                    at,
                })
            }
        }
    }

    /// `BlockBasedTestInstance.run`: the one start block triggers.
    pub fn run(&self, level: &mut Level) -> Result<(), Failure> {
        let starts = self.test_blocks(level, Mode::Start);
        let Some(&start) = starts.first() else {
            return Err(Self::missing(Mode::Start));
        };
        if starts.len() != 1 {
            return Err(Self::fail(
                vanillite_protocol::text::Component::translatable(
                    "test_block.error.too_many",
                    vec![Mode::Start.display_name()],
                ),
            ));
        }
        self.block(level, start)?;
        trigger(level, self.absolute(start));
        Ok(())
    }

    /// The check of every tick (`onEachTick`): `Ok(true)` when an accept block triggered,
    /// an error for a triggered fail block; triggered log blocks log and reset.
    pub fn check(&self, level: &mut Level) -> Result<bool, Failure> {
        let accepts = self.test_blocks(level, Mode::Accept);
        if accepts.is_empty() {
            return Err(Self::missing(Mode::Accept));
        }
        for rel in &accepts {
            if self.block(level, *rel)?.triggered {
                return Ok(true);
            }
        }
        for rel in self.test_blocks(level, Mode::Fail) {
            let block = self.block(level, rel)?;
            if block.triggered {
                return Err(Self::fail(vanillite_protocol::text::Component::text(
                    block.message,
                )));
            }
        }
        for rel in self.test_blocks(level, Mode::Log) {
            if self.block(level, rel)?.triggered {
                let abs = self.absolute(rel);
                trigger(level, abs);
                reset(level, abs);
            }
        }
        Ok(false)
    }
}

/// `handleSetTestBlock`.
pub fn handle(level: &mut Level, packet: SetTestBlock, editor: &Editor<'_>) {
    if !editor.allowed {
        return;
    }
    let pos = BlockPos::new(packet.pos.x, packet.pos.y, packet.pos.z);
    if !matches!(level.block_entity(pos), Some(BlockEntity::TestBlock(_))) {
        return;
    }
    set_mode(level, pos, Mode::by_id(packet.mode));
    if let Some(block) = entity(level, pos) {
        block.message = packet.message;
    }
    level.block_entities.touch(pos.chunk());
    level.send_block_entity_updated(pos);
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn test_block_nbt_round_trip() {
        let block = TestBlock {
            mode: Mode::Accept,
            message: "done".into(),
            powered: true,
            triggered: true,
        };
        let mut nbt = Compound::new();
        block.save(&mut nbt);
        assert_eq!(nbt.get("mode"), Some(&Tag::String("accept".into())));
        let mut loaded = TestBlock::default();
        loaded.load(&nbt);
        assert_eq!(
            loaded,
            TestBlock {
                triggered: false,
                ..block
            }
        );
        let mut empty = TestBlock::default();
        empty.load(&Compound::new());
        assert_eq!(empty.mode, Mode::Fail);
    }

    fn place(level: &mut Level, pos: BlockPos, mode: Mode) {
        let state = ids::TEST_BLOCK
            .default_state()
            .with("mode", mode.name())
            .unwrap_or_else(|| ids::TEST_BLOCK.default_state());
        level.set_block(pos, state, crate::level::update::ALL);
    }

    fn block(level: &Level, pos: BlockPos) -> TestBlock {
        match level.block_entity(pos) {
            Some(BlockEntity::TestBlock(t)) => (**t).clone(),
            other => panic!("no test block: {other:?}"),
        }
    }

    #[test]
    fn test_blocks_trigger_on_rising_signal_and_start_blocks_power() {
        let mut level = crate::level::tests::flat_level();
        let fail = BlockPos::new(0, 0, 0);
        place(&mut level, fail, Mode::Fail);
        assert_eq!(block(&level, fail).mode, Mode::Fail);
        level.set_block(
            fail.relative(Direction::East),
            ids::REDSTONE_BLOCK.default_state(),
            crate::level::update::ALL,
        );
        let b = block(&level, fail);
        assert!(b.powered && b.triggered);
        reset(&mut level, fail);
        assert!(!block(&level, fail).triggered);

        let start = BlockPos::new(4, 0, 0);
        place(&mut level, start, Mode::Start);
        let lamp = start.relative(Direction::East);
        level.set_block(
            lamp,
            ids::REDSTONE_LAMP.default_state(),
            crate::level::update::ALL,
        );
        trigger(&mut level, start);
        assert!(block(&level, start).powered);
        assert!(!block(&level, start).triggered);
        assert_eq!(level.block(lamp).value("lit"), Some("true"));
        set_mode(&mut level, start, Mode::Log);
        assert_eq!(Mode::of_state(level.block(start)), Mode::Log);
    }

    #[test]
    fn block_based_test_passes_when_the_start_block_powers_the_accept_block() {
        use vanillite_worldgen::template::transform::Rotation;
        let mut level = crate::level::tests::flat_level();
        let test = BlockBasedTest {
            origin: BlockPos::new(2, 0, 2),
            rotation: Rotation::Clockwise90,
            size: [3, 1, 1],
        };
        // Missing blocks fail like Vanilla.
        assert_eq!(
            test.run(&mut level).map_err(|f| *f.message),
            Err(vanillite_protocol::text::Component::translatable(
                "test_block.error.missing",
                vec![Mode::Start.display_name()],
            ))
        );
        place(
            &mut level,
            test.absolute(BlockPos::new(0, 0, 0)),
            Mode::Start,
        );
        place(
            &mut level,
            test.absolute(BlockPos::new(2, 0, 0)),
            Mode::Fail,
        );
        assert_eq!(test.check(&mut level).map_err(|f| *f.message).ok(), None);
        let accept = test.absolute(BlockPos::new(1, 0, 0));
        // Clockwise: relative x runs along world z.
        assert_eq!(accept, BlockPos::new(2, 0, 3));
        place(&mut level, accept, Mode::Accept);
        assert_eq!(test.check(&mut level), Ok(false));
        assert_eq!(test.run(&mut level), Ok(()));
        assert_eq!(test.check(&mut level), Ok(true));
    }
}
