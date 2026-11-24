//! Play packets of block interaction, inventories and items.

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
    use crate::Packet;
    use crate::codec::{Decode, Encode, Reader, Result, write_len, write_varint};
    use crate::item::ItemStack;
    use crate::types::BlockPos;

    /// `InteractionHand`; unknown ids decode as the main hand (`OutOfBoundsStrategy.ZERO`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub enum Hand {
        #[default]
        MainHand,
        OffHand,
    }

    impl Encode for Hand {
        fn encode(&self, out: &mut Vec<u8>) {
            write_varint(out, i32::from(*self == Hand::OffHand));
        }
    }

    impl Decode for Hand {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            Ok(if r.varint()? == 1 {
                Hand::OffHand
            } else {
                Hand::MainHand
            })
        }
    }

    crate::varint_enum! {
        /// Action of `player_action` (`ServerboundPlayerActionPacket.Action`).
        pub enum DigAction {
            StartDestroyBlock,
            ChangeDestroyDirection,
            AbortDestroyBlock,
            StopDestroyBlock,
            DropAllItems,
            DropItem,
            ReleaseUseItem,
            SwapItemWithOffhand,
            Stab,
        }
    }

    /// `player_action`: digging, dropping and hand actions. `direction` is the raw byte; the
    /// handler maps it like `Direction.from3DDataValue`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "player_action", bucket = "interact")]
    pub struct PlayerActionPacket {
        pub action: DigAction,
        pub pos: BlockPos,
        pub direction: u8,
        #[field(varint)]
        pub sequence: i32,
    }

    /// `use_item_on`: a click on a block. `face` is the raw `Direction` id (wrapped by the
    /// handler); the cursor is relative to the block and checked by the handler.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "use_item_on", bucket = "interact")]
    pub struct UseItemOn {
        pub hand: Hand,
        pub pos: BlockPos,
        #[field(varint)]
        pub face: i32,
        pub cursor_x: f32,
        pub cursor_y: f32,
        pub cursor_z: f32,
        pub inside: bool,
        pub world_border_hit: bool,
        #[field(varint)]
        pub sequence: i32,
    }

    /// `use_item`: a click into the air.
    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "use_item", bucket = "interact")]
    pub struct UseItem {
        pub hand: Hand,
        #[field(varint)]
        pub sequence: i32,
        pub y_rot: f32,
        pub x_rot: f32,
    }

    /// `set_carried_item`: the selected hotbar slot.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "set_carried_item", bucket = "inventory")]
    pub struct SetCarriedItem {
        pub slot: i16,
    }

    /// `punch`: the player swung its main hand at nothing.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "punch", bucket = "interact")]
    pub struct Punch;

    /// `pick_item_from_block`: middle click on a block.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "pick_item_from_block", bucket = "inventory")]
    pub struct PickItemFromBlock {
        pub pos: BlockPos,
        pub include_data: bool,
    }

    /// `container_close`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "container_close", bucket = "inventory")]
    pub struct ContainerClose {
        #[field(varint)]
        pub container_id: i32,
    }

    /// `container_button_click`: a button of a menu (enchanting options, stonecutter and loom
    /// entries, lectern pages).
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "container_button_click", bucket = "inventory")]
    pub struct ContainerButtonClick {
        #[field(varint)]
        pub container_id: i32,
        #[field(varint)]
        pub button_id: i32,
    }

    /// `container_slot_state_changed`: a crafter slot was enabled or disabled.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(
        state = Play,
        dir = Serverbound,
        name = "container_slot_state_changed",
        bucket = "inventory"
    )]
    pub struct ContainerSlotStateChanged {
        #[field(varint)]
        pub slot_id: i32,
        #[field(varint)]
        pub container_id: i32,
        pub new_state: bool,
    }

    /// `sign_update`: the four lines of one side of a sign. `front` is `SignTextSlot` (0 back,
    /// 1 front).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SignUpdate {
        pub pos: BlockPos,
        pub lines: [String; 4],
        pub front: bool,
    }

    impl Encode for SignUpdate {
        fn encode(&self, out: &mut Vec<u8>) {
            self.pos.encode(out);
            for line in &self.lines {
                line.encode(out);
            }
            write_varint(out, i32::from(self.front));
        }
    }

    impl Decode for SignUpdate {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            let pos = BlockPos::decode(r)?;
            let mut lines: [String; 4] = Default::default();
            for line in &mut lines {
                *line = r.string(384)?.to_owned();
            }
            // `ByteBufCodecs.idMapper`: unknown ids are the first value (back).
            let front = r.varint()? == 1;
            Ok(SignUpdate { pos, lines, front })
        }
    }

    manual_packet!(
        SignUpdate,
        Serverbound,
        serverbound,
        SIGN_UPDATE,
        "sign_update",
        Interact
    );

    /// `rename_item`: the anvil's name field.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "rename_item", bucket = "inventory")]
    pub struct RenameItem {
        #[field(max_len = 32767)]
        pub name: String,
    }

    /// `place_recipe`: a recipe clicked in the recipe book.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "place_recipe", bucket = "inventory")]
    pub struct PlaceRecipe {
        #[field(varint)]
        pub container_id: i32,
        /// `RecipeDisplayId`.
        #[field(varint)]
        pub recipe: i32,
        pub use_max_items: bool,
    }

    /// `recipe_book_seen_recipe`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Serverbound, name = "recipe_book_seen_recipe", bucket = "inventory")]
    pub struct RecipeBookSeenRecipe {
        #[field(varint)]
        pub recipe: i32,
    }

    /// `recipe_book_change_settings`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(
        state = Play,
        dir = Serverbound,
        name = "recipe_book_change_settings",
        bucket = "inventory"
    )]
    pub struct RecipeBookChangeSettings {
        /// `RecipeBookType` id: 0 crafting, 1 furnace, 2 blast furnace, 3 smoker.
        #[field(varint)]
        pub book_type: i32,
        pub open: bool,
        pub filtering: bool,
    }

    /// `set_creative_mode_slot`: a stack from the creative inventory. The stack is decoded with
    /// the untrusted codec and every component value is validated
    /// ([`ItemStack::decode_untrusted`]) against the installed registries.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SetCreativeModeSlot {
        pub slot: i16,
        pub item: ItemStack,
    }

    impl Encode for SetCreativeModeSlot {
        fn encode(&self, out: &mut Vec<u8>) {
            self.slot.encode(out);
            self.item.encode_untrusted(out);
        }
    }

    impl Decode for SetCreativeModeSlot {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            let slot = r.i16()?;
            let item = ItemStack::decode_untrusted(r, &crate::item::InstalledRegistries)?;
            Ok(SetCreativeModeSlot { slot, item })
        }
    }

    manual_packet!(
        SetCreativeModeSlot,
        Serverbound,
        serverbound,
        SET_CREATIVE_MODE_SLOT,
        "set_creative_mode_slot",
        Inventory
    );

    /// `ContainerInput`; unknown ids decode as `Pickup` (`OutOfBoundsStrategy.ZERO`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum ContainerInput {
        Pickup,
        QuickMove,
        Swap,
        Clone,
        Throw,
        QuickCraft,
        PickupAll,
    }

    impl ContainerInput {
        const ALL: [ContainerInput; 7] = [
            ContainerInput::Pickup,
            ContainerInput::QuickMove,
            ContainerInput::Swap,
            ContainerInput::Clone,
            ContainerInput::Throw,
            ContainerInput::QuickCraft,
            ContainerInput::PickupAll,
        ];

        fn id(self) -> i32 {
            Self::ALL
                .iter()
                .position(|c| *c == self)
                .and_then(|i| i32::try_from(i).ok())
                .unwrap_or(0)
        }
    }

    /// The client's view of a stack (`HashedStack`): item, count and component hashes.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct HashedStack {
        /// `None` for the empty stack.
        pub item: Option<HashedItem>,
    }

    /// A non-empty `HashedStack`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct HashedItem {
        pub item: i32,
        pub count: i32,
        /// Added components with their hash.
        pub added: Vec<(i32, i32)>,
        pub removed: Vec<i32>,
    }

    /// Most component entries of a hashed stack (`HashedPatchMap`).
    const MAX_HASHED_COMPONENTS: usize = 256;

    impl Encode for HashedStack {
        fn encode(&self, out: &mut Vec<u8>) {
            match &self.item {
                None => out.push(0),
                Some(h) => {
                    out.push(1);
                    write_varint(out, h.item);
                    write_varint(out, h.count);
                    write_len(out, h.added.len());
                    for (t, hash) in &h.added {
                        write_varint(out, *t);
                        hash.encode(out);
                    }
                    write_len(out, h.removed.len());
                    for t in &h.removed {
                        write_varint(out, *t);
                    }
                }
            }
        }
    }

    impl Decode for HashedStack {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            if !bool::decode(r)? {
                return Ok(HashedStack::default());
            }
            let item = r.varint()?;
            let count = r.varint()?;
            let n = r.len_prefix("hashed components", MAX_HASHED_COMPONENTS, 5)?;
            let mut added = Vec::with_capacity(n);
            for _ in 0..n {
                added.push((r.varint()?, r.i32()?));
            }
            let n = r.len_prefix("removed hashed components", MAX_HASHED_COMPONENTS, 1)?;
            let mut removed = Vec::with_capacity(n);
            for _ in 0..n {
                removed.push(r.varint()?);
            }
            Ok(HashedStack {
                item: Some(HashedItem {
                    item,
                    count,
                    added,
                    removed,
                }),
            })
        }
    }

    /// `container_click`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ContainerClick {
        pub container_id: i32,
        pub state_id: i32,
        pub slot: i16,
        pub button: i8,
        pub input: ContainerInput,
        /// The client's predicted changes (at most 128).
        pub changed: Vec<(i16, HashedStack)>,
        pub carried: HashedStack,
    }

    /// Most changed slots of one click (`ServerboundContainerClickPacket.MAX_SLOT_COUNT`).
    const MAX_CHANGED_SLOTS: usize = 128;

    impl Encode for ContainerClick {
        fn encode(&self, out: &mut Vec<u8>) {
            write_varint(out, self.container_id);
            write_varint(out, self.state_id);
            self.slot.encode(out);
            self.button.encode(out);
            write_varint(out, self.input.id());
            write_len(out, self.changed.len());
            for (slot, stack) in &self.changed {
                slot.encode(out);
                stack.encode(out);
            }
            self.carried.encode(out);
        }
    }

    impl Decode for ContainerClick {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            let container_id = r.varint()?;
            let state_id = r.varint()?;
            let slot = r.i16()?;
            let button = r.i8()?;
            let input = usize::try_from(r.varint()?)
                .ok()
                .and_then(|i| ContainerInput::ALL.get(i).copied())
                .unwrap_or(ContainerInput::Pickup);
            let n = r.len_prefix("changed slots", MAX_CHANGED_SLOTS, 3)?;
            let mut changed: Vec<(i16, HashedStack)> = Vec::with_capacity(n);
            for _ in 0..n {
                let slot = r.i16()?;
                let stack = HashedStack::decode(r)?;
                // A map: a later entry replaces an earlier one.
                changed.retain(|(s, _)| *s != slot);
                changed.push((slot, stack));
            }
            let carried = HashedStack::decode(r)?;
            Ok(ContainerClick {
                container_id,
                state_id,
                slot,
                button,
                input,
                changed,
                carried,
            })
        }
    }

    manual_packet!(
        ContainerClick,
        Serverbound,
        serverbound,
        CONTAINER_CLICK,
        "container_click",
        Inventory
    );
}

/// Clientbound packets.
pub mod clientbound {
    use crate::Packet;
    use crate::codec::{Decode, Encode, Reader, Result, write_len, write_varint};
    use crate::item::ItemStack;
    use crate::types::BlockPos;

    /// `block_changed_ack`: block changes of the client's predictions up to `sequence` are
    /// settled.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "block_changed_ack")]
    pub struct BlockChangedAck {
        #[field(varint)]
        pub sequence: i32,
    }

    /// `block_update`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "block_update")]
    pub struct BlockUpdate {
        pub pos: BlockPos,
        #[field(varint)]
        pub state: i32,
    }

    /// `block_destruction`: crack progress 0–9 of a block a player digs; other values remove
    /// it.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "block_destruction")]
    pub struct BlockDestruction {
        #[field(varint)]
        pub id: i32,
        pub pos: BlockPos,
        pub progress: u8,
    }

    /// `level_event` (`LevelEvent` ids, e.g. 2001 block break particles and sound).
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "level_event")]
    pub struct LevelEvent {
        pub event: i32,
        pub pos: BlockPos,
        pub data: i32,
        pub global: bool,
    }

    /// `open_screen`: opens a menu.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "open_screen")]
    pub struct OpenScreen {
        #[field(varint)]
        pub container_id: i32,
        /// `minecraft:menu` id.
        #[field(varint)]
        pub menu: i32,
        #[field(max_len = 2_097_152)]
        pub title: crate::text::Component,
    }

    /// `open_sign_editor`: the client opens the editor of one side of a sign.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct OpenSignEditor {
        pub pos: BlockPos,
        pub front: bool,
    }

    impl Encode for OpenSignEditor {
        fn encode(&self, out: &mut Vec<u8>) {
            self.pos.encode(out);
            write_varint(out, i32::from(self.front));
        }
    }

    impl Decode for OpenSignEditor {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            Ok(OpenSignEditor {
                pos: BlockPos::decode(r)?,
                front: r.varint()? == 1,
            })
        }
    }

    manual_packet!(
        OpenSignEditor,
        Clientbound,
        clientbound,
        OPEN_SIGN_EDITOR,
        "open_sign_editor",
        Default
    );

    /// `container_set_data`: a data slot of a menu (progress bars, costs, selections).
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "container_set_data")]
    pub struct ContainerSetData {
        #[field(varint)]
        pub container_id: i32,
        pub id: i16,
        pub value: i16,
    }

    /// `container_close`: the server closes the open menu.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "container_close")]
    pub struct ContainerCloseClientbound {
        #[field(varint)]
        pub container_id: i32,
    }

    /// `block_entity_data`: the client-visible data of a block entity as network NBT (one
    /// compound tag, already encoded).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct BlockEntityData {
        pub pos: BlockPos,
        /// `minecraft:block_entity_type` id.
        pub kind: i32,
        /// The compound in network NBT (`TRUSTED_COMPOUND_TAG`).
        pub nbt: Vec<u8>,
    }

    impl Encode for BlockEntityData {
        fn encode(&self, out: &mut Vec<u8>) {
            self.pos.encode(out);
            write_varint(out, self.kind);
            out.extend_from_slice(&self.nbt);
        }
    }

    impl Decode for BlockEntityData {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            let pos = BlockPos::decode(r)?;
            let kind = r.varint()?;
            Ok(BlockEntityData {
                pos,
                kind,
                nbt: r.take_rest().to_vec(),
            })
        }
    }

    manual_packet!(
        BlockEntityData,
        Clientbound,
        clientbound,
        BLOCK_ENTITY_DATA,
        "block_entity_data",
        Default
    );

    /// One stonecutter entry of `update_recipes`: the input items and the result.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StonecutterEntry {
        /// Item ids the recipe takes (an `Ingredient`, sent as a direct holder set).
        pub input: Vec<i32>,
        /// The result (`SlotDisplay.ItemStackSlotDisplay` of an `ItemStackTemplate`).
        pub result: ItemStack,
    }

    /// `update_recipes`: the recipe property sets (furnace inputs, smithing slots, brewing)
    /// and the stonecutter recipes the client shows.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct UpdateRecipes {
        /// Property set id (e.g. `minecraft:furnace_input`) → item ids.
        pub item_sets: Vec<(String, Vec<i32>)>,
        /// Stonecutter recipes in recipe id order.
        pub stonecutter: Vec<StonecutterEntry>,
    }

    /// `minecraft:slot_display` id of `item_stack`.
    const SLOT_DISPLAY_ITEM_STACK: i32 = 5;

    impl Encode for UpdateRecipes {
        fn encode(&self, out: &mut Vec<u8>) {
            write_len(out, self.item_sets.len());
            for (key, items) in &self.item_sets {
                key.encode(out);
                write_len(out, items.len());
                for item in items {
                    write_varint(out, *item);
                }
            }
            write_len(out, self.stonecutter.len());
            for entry in &self.stonecutter {
                // `ByteBufCodecs.holderSet`: size + 1, then the ids.
                write_len(out, entry.input.len().saturating_add(1));
                for item in &entry.input {
                    write_varint(out, *item);
                }
                write_varint(out, SLOT_DISPLAY_ITEM_STACK);
                // `ItemStackTemplate`: item, count, component patch.
                let mut stack = Vec::new();
                entry.result.encode(&mut stack);
                let mut r = Reader::new(&stack);
                let count = r.varint().unwrap_or(1);
                let item = r.varint().unwrap_or(0);
                write_varint(out, item);
                write_varint(out, count);
                out.extend_from_slice(r.take_rest());
            }
        }
    }

    impl Decode for UpdateRecipes {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            // Only the test client reads it; the contents are skipped.
            r.take_rest();
            Ok(UpdateRecipes::default())
        }
    }

    manual_packet!(
        UpdateRecipes,
        Clientbound,
        clientbound,
        UPDATE_RECIPES,
        "update_recipes",
        Default
    );

    /// `set_held_slot`.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "set_held_slot")]
    pub struct SetHeldSlot {
        #[field(varint)]
        pub slot: i32,
    }

    /// `take_item_entity`: the pickup animation.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "take_item_entity")]
    pub struct TakeItemEntity {
        #[field(varint)]
        pub item_id: i32,
        #[field(varint)]
        pub player_id: i32,
        #[field(varint)]
        pub amount: i32,
    }

    /// `swing_animation`: an entity swings a hand.
    #[derive(Debug, Clone, PartialEq, Eq, Packet)]
    #[packet(state = Play, dir = Clientbound, name = "swing_animation")]
    pub struct SwingAnimation {
        #[field(varint)]
        pub entity_id: i32,
        pub hand: super::serverbound::Hand,
        /// `SwingAnimationType` id: 0 none, 1 whack, 2 stab.
        #[field(varint)]
        pub kind: i32,
        #[field(varint)]
        pub duration: i32,
    }

    /// `section_blocks_update`: several block changes of one section.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SectionBlocksUpdate {
        /// `SectionPos.asLong`.
        pub section: i64,
        /// `state id << 12 | x << 8 | z << 4 | y` (section-relative).
        pub changes: Vec<i64>,
    }

    impl Encode for SectionBlocksUpdate {
        fn encode(&self, out: &mut Vec<u8>) {
            self.section.encode(out);
            write_len(out, self.changes.len());
            for c in &self.changes {
                crate::codec::write_varlong(out, *c);
            }
        }
    }

    impl Decode for SectionBlocksUpdate {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            let section = r.i64()?;
            let n = r.len_prefix("section block changes", 4096, 1)?;
            let mut changes = Vec::with_capacity(n);
            for _ in 0..n {
                changes.push(r.varlong()?);
            }
            Ok(SectionBlocksUpdate { section, changes })
        }
    }

    manual_packet!(
        SectionBlocksUpdate,
        Clientbound,
        clientbound,
        SECTION_BLOCKS_UPDATE,
        "section_blocks_update",
        Default
    );

    /// `container_set_content`: all slots of a menu and the carried stack.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ContainerSetContent {
        pub container_id: i32,
        pub state_id: i32,
        pub items: Vec<ItemStack>,
        pub carried: ItemStack,
    }

    impl Encode for ContainerSetContent {
        fn encode(&self, out: &mut Vec<u8>) {
            write_varint(out, self.container_id);
            write_varint(out, self.state_id);
            write_len(out, self.items.len());
            for item in &self.items {
                item.encode(out);
            }
            self.carried.encode(out);
        }
    }

    impl Decode for ContainerSetContent {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            let container_id = r.varint()?;
            let state_id = r.varint()?;
            let n = r.len_prefix("container items", 256, 1)?;
            let mut items = Vec::with_capacity(n);
            for _ in 0..n {
                items.push(ItemStack::decode_plain(r)?);
            }
            let carried = ItemStack::decode_plain(r)?;
            Ok(ContainerSetContent {
                container_id,
                state_id,
                items,
                carried,
            })
        }
    }

    manual_packet!(
        ContainerSetContent,
        Clientbound,
        clientbound,
        CONTAINER_SET_CONTENT,
        "container_set_content",
        Default
    );

    /// `container_set_slot`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ContainerSetSlot {
        pub container_id: i32,
        pub state_id: i32,
        pub slot: i16,
        pub item: ItemStack,
    }

    impl Encode for ContainerSetSlot {
        fn encode(&self, out: &mut Vec<u8>) {
            write_varint(out, self.container_id);
            write_varint(out, self.state_id);
            self.slot.encode(out);
            self.item.encode(out);
        }
    }

    impl Decode for ContainerSetSlot {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            Ok(ContainerSetSlot {
                container_id: r.varint()?,
                state_id: r.varint()?,
                slot: r.i16()?,
                item: ItemStack::decode_plain(r)?,
            })
        }
    }

    manual_packet!(
        ContainerSetSlot,
        Clientbound,
        clientbound,
        CONTAINER_SET_SLOT,
        "container_set_slot",
        Default
    );

    /// `set_cursor_item`: the stack carried by the mouse.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SetCursorItem {
        pub item: ItemStack,
    }

    impl Encode for SetCursorItem {
        fn encode(&self, out: &mut Vec<u8>) {
            self.item.encode(out);
        }
    }

    impl Decode for SetCursorItem {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            Ok(SetCursorItem {
                item: ItemStack::decode_plain(r)?,
            })
        }
    }

    manual_packet!(
        SetCursorItem,
        Clientbound,
        clientbound,
        SET_CURSOR_ITEM,
        "set_cursor_item",
        Default
    );

    /// `set_player_inventory`: one slot of the player's inventory (`Inventory` index).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SetPlayerInventory {
        pub slot: i32,
        pub item: ItemStack,
    }

    impl Encode for SetPlayerInventory {
        fn encode(&self, out: &mut Vec<u8>) {
            write_varint(out, self.slot);
            self.item.encode(out);
        }
    }

    impl Decode for SetPlayerInventory {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            Ok(SetPlayerInventory {
                slot: r.varint()?,
                item: ItemStack::decode_plain(r)?,
            })
        }
    }

    manual_packet!(
        SetPlayerInventory,
        Clientbound,
        clientbound,
        SET_PLAYER_INVENTORY,
        "set_player_inventory",
        Default
    );

    /// `set_equipment`: what an entity wears and holds (`EquipmentSlot` ordinal per entry).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SetEquipment {
        pub entity_id: i32,
        pub slots: Vec<(u8, ItemStack)>,
    }

    impl Encode for SetEquipment {
        fn encode(&self, out: &mut Vec<u8>) {
            write_varint(out, self.entity_id);
            let last = self.slots.len().saturating_sub(1);
            for (i, (slot, item)) in self.slots.iter().enumerate() {
                out.push(if i < last { slot | 0x80 } else { *slot });
                item.encode(out);
            }
        }
    }

    impl Decode for SetEquipment {
        fn decode(r: &mut Reader<'_>) -> Result<Self> {
            let entity_id = r.varint()?;
            let mut slots = Vec::new();
            loop {
                let b = r.u8()?;
                slots.push((b & 0x7F, ItemStack::decode_plain(r)?));
                if b & 0x80 == 0 || slots.len() >= 8 {
                    break;
                }
            }
            Ok(SetEquipment { entity_id, slots })
        }
    }

    manual_packet!(
        SetEquipment,
        Clientbound,
        clientbound,
        SET_EQUIPMENT,
        "set_equipment",
        Default
    );
}
