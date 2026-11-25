//! Play packets of the technical blocks: command blocks, command block minecarts, structure,
//! jigsaw, test and test instance blocks (the edit screens of operators in creative).

/// Implements [`Packet`](crate::Packet) for a struct with hand-written codecs.
macro_rules! manual_packet {
    ($ty:ident, $dir:ident, $module:ident, $id:ident, $name:literal, $bucket:ident) => {
        impl crate::Packet for $ty {
            const ID: i32 = crate::ids::play::$module::$id;
            const NAME: &'static str = $name;
            const STATE: crate::State = crate::State::Play;
            const DIRECTION: crate::Direction = crate::Direction::$dir;
            const COST: u16 = 1;
            const BUCKET: crate::Bucket = crate::Bucket::$bucket;
        }
    };
}

/// Serverbound packets.
pub mod serverbound {
    use crate::codec::{
        Decode, DecodeLimited, Encode, Reader, Result, write_varint, write_varlong,
    };
    use crate::text::Component;
    use crate::types::{BlockPos, Identifier};

    /// `FriendlyByteBuf.readUtf()` without a limit of its own.
    const MAX_STRING: usize = 32767;
    /// The component quota of an optional error message.
    const COMPONENT_MAX: usize = 2_097_152;

    fn flag(flags: i8, bit: i8) -> bool {
        flags & bit != 0
    }

    fn write_bool(out: &mut Vec<u8>, value: bool) {
        out.push(u8::from(value));
    }

    fn read_bool(r: &mut Reader<'_>) -> Result<bool> {
        Ok(r.u8()? != 0)
    }

    /// `ByteBufCodecs.idMapper` with `OutOfBoundsStrategy.ZERO`: unknown ids are the first.
    fn id_or_zero(r: &mut Reader<'_>, count: i32) -> Result<i32> {
        let id = r.varint()?;
        Ok(if (0..count).contains(&id) { id } else { 0 })
    }

    /// `set_command_block`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SetCommandBlock {
        pub pos: BlockPos,
        pub command: String,
        /// `CommandBlockEntity.Mode`: 0 sequence (chain), 1 auto (repeating), 2 redstone.
        pub mode: u8,
        pub track_output: bool,
        pub conditional: bool,
        pub automatic: bool,
    }

    impl Encode for SetCommandBlock {
        fn encode(&self, out: &mut Vec<u8>) {
            self.pos.encode(out);
            self.command.encode(out);
            write_varint(out, i32::from(self.mode));
            let flags = u8::from(self.track_output)
                | u8::from(self.conditional) << 1
                | u8::from(self.automatic) << 2;
            out.push(flags);
        }
    }

    impl Decode for SetCommandBlock {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            let pos = BlockPos::decode(r)?;
            let command = r.string(MAX_STRING)?.to_owned();
            let mode = u8::try_from(r.enum_id(3, "command block mode")?).unwrap_or(0);
            let flags = r.i8()?;
            Ok(Self {
                pos,
                command,
                mode,
                track_output: flag(flags, 1),
                conditional: flag(flags, 2),
                automatic: flag(flags, 4),
            })
        }
    }

    manual_packet!(
        SetCommandBlock,
        Serverbound,
        serverbound,
        SET_COMMAND_BLOCK,
        "set_command_block",
        Interact
    );

    /// `set_command_minecart`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SetCommandMinecart {
        pub entity: i32,
        pub command: String,
        pub track_output: bool,
    }

    impl Encode for SetCommandMinecart {
        fn encode(&self, out: &mut Vec<u8>) {
            write_varint(out, self.entity);
            self.command.encode(out);
            write_bool(out, self.track_output);
        }
    }

    impl Decode for SetCommandMinecart {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            Ok(Self {
                entity: r.varint()?,
                command: r.string(MAX_STRING)?.to_owned(),
                track_output: read_bool(r)?,
            })
        }
    }

    manual_packet!(
        SetCommandMinecart,
        Serverbound,
        serverbound,
        SET_COMMAND_MINECART,
        "set_command_minecart",
        Interact
    );

    /// `set_structure_block`: offsets clamp to `-48..=48`, sizes to `0..=48`, the integrity
    /// to `0..=1`, as Vanilla's reader does.
    #[derive(Debug, Clone, PartialEq)]
    pub struct SetStructureBlock {
        pub pos: BlockPos,
        /// `StructureBlockEntity.UpdateType`: update data, save area, load area, scan area.
        pub update_type: u8,
        /// `StructureMode`: save, load, corner, data.
        pub mode: u8,
        pub name: String,
        pub offset: [i32; 3],
        pub size: [i32; 3],
        /// `Mirror`: none, left right, front back.
        pub mirror: u8,
        /// `Rotation`: none, clockwise 90, 180, counterclockwise 90.
        pub rotation: u8,
        pub data: String,
        pub ignore_entities: bool,
        pub strict: bool,
        pub show_air: bool,
        pub show_bounding_box: bool,
        pub integrity: f32,
        pub seed: i64,
    }

    impl Encode for SetStructureBlock {
        fn encode(&self, out: &mut Vec<u8>) {
            self.pos.encode(out);
            write_varint(out, i32::from(self.update_type));
            write_varint(out, i32::from(self.mode));
            self.name.encode(out);
            for v in self.offset.iter().chain(&self.size) {
                out.push(i8::try_from(*v).unwrap_or(0).to_be_bytes()[0]);
            }
            write_varint(out, i32::from(self.mirror));
            write_varint(out, i32::from(self.rotation));
            self.data.encode(out);
            out.extend_from_slice(&self.integrity.to_be_bytes());
            write_varlong(out, self.seed);
            let flags = u8::from(self.ignore_entities)
                | u8::from(self.show_air) << 1
                | u8::from(self.show_bounding_box) << 2
                | u8::from(self.strict) << 3;
            out.push(flags);
        }
    }

    impl Decode for SetStructureBlock {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            let pos = BlockPos::decode(r)?;
            let update_type = u8::try_from(r.enum_id(4, "structure update type")?).unwrap_or(0);
            let mode = u8::try_from(r.enum_id(4, "structure mode")?).unwrap_or(0);
            let name = r.string(MAX_STRING)?.to_owned();
            let mut offset = [0; 3];
            for v in &mut offset {
                *v = i32::from(r.i8()?).clamp(-48, 48);
            }
            let mut size = [0; 3];
            for v in &mut size {
                *v = i32::from(r.i8()?).clamp(0, 48);
            }
            let mirror = u8::try_from(r.enum_id(3, "mirror")?).unwrap_or(0);
            let rotation = u8::try_from(id_or_zero(r, 4)?).unwrap_or(0);
            let data = r.string(128)?.to_owned();
            let integrity = r.f32()?.clamp(0.0, 1.0);
            let seed = r.varlong()?;
            let flags = r.i8()?;
            Ok(Self {
                pos,
                update_type,
                mode,
                name,
                offset,
                size,
                mirror,
                rotation,
                data,
                ignore_entities: flag(flags, 1),
                strict: flag(flags, 8),
                show_air: flag(flags, 2),
                show_bounding_box: flag(flags, 4),
                // `Mth.clamp` of NaN is NaN in Java too; it stays as sent.
                integrity,
                seed,
            })
        }
    }

    manual_packet!(
        SetStructureBlock,
        Serverbound,
        serverbound,
        SET_STRUCTURE_BLOCK,
        "set_structure_block",
        Interact
    );

    /// `set_jigsaw_block`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SetJigsawBlock {
        pub pos: BlockPos,
        pub name: Identifier,
        pub target: Identifier,
        pub pool: Identifier,
        pub final_state: String,
        /// `JigsawBlockEntity.JointType` by name; unknown names are `aligned`.
        pub joint: String,
        pub selection_priority: i32,
        pub placement_priority: i32,
    }

    impl Encode for SetJigsawBlock {
        fn encode(&self, out: &mut Vec<u8>) {
            self.pos.encode(out);
            self.name.encode(out);
            self.target.encode(out);
            self.pool.encode(out);
            self.final_state.encode(out);
            self.joint.encode(out);
            write_varint(out, self.selection_priority);
            write_varint(out, self.placement_priority);
        }
    }

    impl Decode for SetJigsawBlock {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            Ok(Self {
                pos: BlockPos::decode(r)?,
                name: Identifier::decode(r)?,
                target: Identifier::decode(r)?,
                pool: Identifier::decode(r)?,
                final_state: r.string(MAX_STRING)?.to_owned(),
                joint: r.string(MAX_STRING)?.to_owned(),
                selection_priority: r.varint()?,
                placement_priority: r.varint()?,
            })
        }
    }

    manual_packet!(
        SetJigsawBlock,
        Serverbound,
        serverbound,
        SET_JIGSAW_BLOCK,
        "set_jigsaw_block",
        Interact
    );

    /// `jigsaw_generate`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct JigsawGenerate {
        pub pos: BlockPos,
        pub levels: i32,
        pub keep_jigsaws: bool,
    }

    impl Encode for JigsawGenerate {
        fn encode(&self, out: &mut Vec<u8>) {
            self.pos.encode(out);
            write_varint(out, self.levels);
            write_bool(out, self.keep_jigsaws);
        }
    }

    impl Decode for JigsawGenerate {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            Ok(Self {
                pos: BlockPos::decode(r)?,
                levels: r.varint()?,
                keep_jigsaws: read_bool(r)?,
            })
        }
    }

    manual_packet!(
        JigsawGenerate,
        Serverbound,
        serverbound,
        JIGSAW_GENERATE,
        "jigsaw_generate",
        Interact
    );

    /// `set_test_block`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SetTestBlock {
        pub pos: BlockPos,
        /// `TestBlockMode`: start, log, fail, accept (unknown ids are start).
        pub mode: u8,
        pub message: String,
    }

    impl Encode for SetTestBlock {
        fn encode(&self, out: &mut Vec<u8>) {
            self.pos.encode(out);
            write_varint(out, i32::from(self.mode));
            self.message.encode(out);
        }
    }

    impl Decode for SetTestBlock {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            Ok(Self {
                pos: BlockPos::decode(r)?,
                mode: u8::try_from(id_or_zero(r, 4)?).unwrap_or(0),
                message: r.string(MAX_STRING)?.to_owned(),
            })
        }
    }

    manual_packet!(
        SetTestBlock,
        Serverbound,
        serverbound,
        SET_TEST_BLOCK,
        "set_test_block",
        Interact
    );

    /// `TestInstanceBlockEntity.Data` as sent.
    #[derive(Debug, Clone, PartialEq)]
    pub struct TestInstanceData {
        pub test: Option<Identifier>,
        pub size: [i32; 3],
        pub rotation: u8,
        pub ignore_entities: bool,
        /// `TestInstanceBlockEntity.Status`: cleared, running, finished.
        pub status: u8,
        pub error_message: Option<Component>,
    }

    impl Encode for TestInstanceData {
        fn encode(&self, out: &mut Vec<u8>) {
            write_bool(out, self.test.is_some());
            if let Some(test) = &self.test {
                test.encode(out);
            }
            for v in self.size {
                write_varint(out, v);
            }
            write_varint(out, i32::from(self.rotation));
            write_bool(out, self.ignore_entities);
            write_varint(out, i32::from(self.status));
            write_bool(out, self.error_message.is_some());
            if let Some(message) = &self.error_message {
                message.encode(out);
            }
        }
    }

    impl Decode for TestInstanceData {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            let test = if read_bool(r)? {
                Some(Identifier::decode(r)?)
            } else {
                None
            };
            let mut size = [0; 3];
            for v in &mut size {
                *v = r.varint()?;
            }
            let rotation = u8::try_from(id_or_zero(r, 4)?).unwrap_or(0);
            let ignore_entities = read_bool(r)?;
            let status = u8::try_from(id_or_zero(r, 3)?).unwrap_or(0);
            let error_message = if read_bool(r)? {
                Some(Component::decode_limited(r, COMPONENT_MAX)?)
            } else {
                None
            };
            Ok(Self {
                test,
                size,
                rotation,
                ignore_entities,
                status,
                error_message,
            })
        }
    }

    /// `test_instance_block_action`.
    #[derive(Debug, Clone, PartialEq)]
    pub struct TestInstanceBlockAction {
        pub pos: BlockPos,
        /// Init, query, set, reset, save, export, run (unknown ids are init).
        pub action: u8,
        pub data: TestInstanceData,
    }

    impl Encode for TestInstanceBlockAction {
        fn encode(&self, out: &mut Vec<u8>) {
            self.pos.encode(out);
            write_varint(out, i32::from(self.action));
            self.data.encode(out);
        }
    }

    impl Decode for TestInstanceBlockAction {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            Ok(Self {
                pos: BlockPos::decode(r)?,
                action: u8::try_from(id_or_zero(r, 7)?).unwrap_or(0),
                data: TestInstanceData::decode(r)?,
            })
        }
    }

    manual_packet!(
        TestInstanceBlockAction,
        Serverbound,
        serverbound,
        TEST_INSTANCE_BLOCK_ACTION,
        "test_instance_block_action",
        Interact
    );
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod structure_tests {
    use super::serverbound::{JigsawGenerate, SetJigsawBlock, SetStructureBlock};
    use crate::codec::{Decode, Encode, Reader};
    use crate::types::{BlockPos, Identifier};

    fn round_trip<T: Encode + Decode>(packet: &T) -> (Vec<u8>, T) {
        let mut buf = Vec::new();
        packet.encode(&mut buf);
        let decoded = T::decode(&mut Reader::new(&buf)).expect("decodes");
        (buf, decoded)
    }

    #[test]
    fn set_structure_block_decodes_and_clamps() {
        let packet = SetStructureBlock {
            pos: BlockPos { x: 1, y: -2, z: 3 },
            update_type: 1,
            mode: 0,
            name: "a".to_owned(),
            offset: [-48, 5, 48],
            size: [1, 48, 0],
            mirror: 2,
            rotation: 3,
            data: "marker".to_owned(),
            ignore_entities: false,
            strict: true,
            show_air: true,
            show_bounding_box: false,
            integrity: 0.5,
            seed: -9,
        };
        let (mut buf, decoded) = round_trip(&packet);
        assert_eq!(decoded, packet);
        // Position (8), update type, mode, the name (2): then the offsets and sizes.
        buf[12] = 100;
        buf[15] = 0x80;
        let clamped = SetStructureBlock::decode(&mut Reader::new(&buf)).expect("decodes");
        assert_eq!(clamped.offset[0], 48);
        assert_eq!(clamped.size[0], 0);
    }

    #[test]
    fn jigsaw_packets_decode() {
        let id = |s: &str| Identifier::new(s).expect("identifier");
        let packet = SetJigsawBlock {
            pos: BlockPos { x: 0, y: 64, z: 0 },
            name: id("minecraft:bottom"),
            target: id("minecraft:top"),
            pool: id("minecraft:empty"),
            final_state: "minecraft:air".to_owned(),
            joint: "rollable".to_owned(),
            selection_priority: 2,
            placement_priority: -1,
        };
        assert_eq!(round_trip(&packet).1, packet);
        let generate = JigsawGenerate {
            pos: BlockPos { x: 5, y: 6, z: 7 },
            levels: 7,
            keep_jigsaws: true,
        };
        assert_eq!(round_trip(&generate).1, generate);
    }
}
