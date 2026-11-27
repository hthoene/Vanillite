//! The packets of the operator screens (Vanilla's `ServerGamePacketListenerImpl`
//! `handleChangeDifficulty`, `handleLockDifficulty`, `handleChangeGameMode`,
//! `handleSetGameRule`, `sendGameRuleValues`, `handleBlockEntityTagQuery`,
//! `handleEntityTagQuery` and `handleTeleportToEntityPacket`). Every request checks the
//! sender's permission like Vanilla does: client checks count for nothing.

use vanillite_command::reader::StringReader;
use vanillite_config::GameMode;
use vanillite_game::level::Level;
use vanillite_game::player::AdminRequest;
use vanillite_game::survival::rules::{RULES, Rule, RuleType};
use vanillite_nbt::Tag;
use vanillite_protocol::packets::play::clientbound::{GameRuleValues, SystemChat, TagQuery};
use vanillite_protocol::packets::play::serverbound::GameRuleEntry;
use vanillite_protocol::text::{Arg, Component};
use vanillite_protocol::types::{Identifier, Nbt};
use vanillite_world::entity::EntityId;

use super::{permission_level, player_source, select};
use crate::global::Global;
use crate::world::WorldId;

/// `Permissions.COMMANDS_GAMEMASTER`.
const GAMEMASTER: u8 = 2;

/// Handles one operator screen request of the player `id` of this world.
pub(crate) fn handle(
    level: &mut Level,
    global: &Global,
    id: EntityId,
    world: WorldId,
    request: AdminRequest,
) {
    let Some(entity) = level.entities.store.get(id) else {
        return;
    };
    let uuid = entity.uuid();
    let Some(player) = level.entities.player(id) else {
        return;
    };
    let name = player.profile.name.clone();
    let game_mode = player.game_mode;
    let sink = std::sync::Arc::clone(&player.connection.sink);
    let gamemaster = permission_level(global, uuid) >= GAMEMASTER;
    match request {
        AdminRequest::ChangeDifficulty(raw) => {
            // `Difficulty.STREAM_CODEC`: out-of-range ids wrap.
            let difficulty =
                vanillite_game::survival::rules::Difficulty::from_id(raw.rem_euclid(4));
            if !gamemaster {
                log::warn!(
                    "Player {name} tried to change difficulty to {} without required permissions",
                    difficulty_display(difficulty)
                );
            } else if !global.level.difficulty_locked() {
                // `MinecraftServer.setDifficulty(difficulty, false)`.
                global.level.set_difficulty(difficulty);
                global
                    .players
                    .broadcast(&crate::survival::change_difficulty(&global.level), None);
            }
        }
        AdminRequest::LockDifficulty(locked) => {
            if gamemaster {
                global.level.set_difficulty_locked(locked);
                global
                    .players
                    .broadcast(&crate::survival::change_difficulty(&global.level), None);
            }
        }
        AdminRequest::ChangeGameMode(raw) => {
            // `GameType.STREAM_CODEC`: out-of-range ids are survival.
            let mode = crate::players::game_mode_by_id(raw).unwrap_or(GameMode::Survival);
            if !gamemaster {
                log::warn!(
                    "Player {name} tried to change game mode to {} without required permissions",
                    short_mode_name(mode)
                );
                return;
            }
            // `GameModeCommand.setGameMode(player, mode)` with the player's own source.
            let (Some(source), Some(target)) = (
                player_source(&level.entities, global, id, world),
                select::local(&level.entities, world, id),
            ) else {
                return;
            };
            super::exec::game_mode(&source, &mut level.entities, global, vec![target], mode);
        }
        AdminRequest::SetGameRules(entries) => {
            if !gamemaster {
                log::warn!(
                    "Player {name} tried to set game rule values without required permissions"
                );
                return;
            }
            for (key, value) in entries {
                // Only `minecraft:` keys name a registered rule.
                let rule = key.strip_prefix("minecraft:").and_then(Rule::by_name);
                let Some(rule) = rule else {
                    log::warn!(
                        "Received request to set unknown game rule: ResourceKey[minecraft:game_rule / {key}]"
                    );
                    continue;
                };
                // `GameRule.deserialize`: values that do not parse are ignored.
                let Some(value) = deserialize(rule, &value) else {
                    continue;
                };
                global.level.rules_mut().set(rule, value);
                super::survival::on_game_rule_changed(global, rule, value);
                broadcast_to_operators(global, rule, value);
            }
        }
        AdminRequest::RequestGameRuleValues => {
            if !gamemaster {
                log::warn!(
                    "Player {name} tried to request game rule values without required permissions"
                );
                return;
            }
            let rules = global.level.rules().clone();
            let values = RULES
                .iter()
                .enumerate()
                .map(|(i, info)| GameRuleEntry {
                    rule: Identifier::vanilla(info.name),
                    value: Rule(i).serialize(rules.get(Rule(i))),
                })
                .collect();
            sink.send(&GameRuleValues { values });
        }
        AdminRequest::BlockEntityTagQuery {
            transaction_id,
            pos,
        } => {
            if !gamemaster {
                return;
            }
            // `saveWithoutMetadata`: without `id`, `x`, `y` and `z`. Unloaded chunks are not
            // loaded for a client's query; they answer like a block without block entity.
            let tag = level.block_entity_nbt(pos).map(|mut c| {
                for key in ["id", "x", "y", "z"] {
                    c.remove(key);
                }
                Tag::Compound(c)
            });
            sink.send(&TagQuery {
                transaction_id,
                tag: Nbt(tag),
            });
        }
        AdminRequest::EntityTagQuery {
            transaction_id,
            entity_id,
        } => {
            if !gamemaster {
                return;
            }
            let Some(nbt) = level
                .entities
                .by_network_id(entity_id)
                .and_then(|e| super::data::entity_nbt(&level.entities, e))
            else {
                return;
            };
            sink.send(&TagQuery {
                transaction_id,
                tag: Nbt(Some(Tag::Compound(nbt))),
            });
        }
        AdminRequest::TeleportToEntity(to) => {
            if game_mode != GameMode::Spectator {
                return;
            }
            // `teleportTo(..., resetCamera = true)`.
            crate::spectator::set_camera(&mut level.entities, id, None);
            teleport_to_entity(level, global, id, world, to);
        }
    }
}

/// `handleTeleportToEntityPacket`: a spectator moves to the entity with the UUID `to`, in
/// whatever world it is (`teleportTo(level, x, y, z, {}, yRot, xRot, true)`).
fn teleport_to_entity(level: &mut Level, global: &Global, id: EntityId, world: WorldId, to: u128) {
    let Some(target) = select::local(&level.entities, world, id) else {
        return;
    };
    if let Some(place) = select::find_uuid(&level.entities, to)
        .and_then(|e| level.entities.store.get(e))
        .map(|e| (e.pos(), e.rot()))
    {
        let (pos, rot) = place;
        let _ = super::exec::teleport_local(level, &target, pos, 0, rot.y_rot, rot.x_rot);
        return;
    }
    // UUIDs are unique: at most one other world has the entity.
    for index in 0..crate::server::DIMENSIONS.len() {
        let Ok(index) = u16::try_from(index) else {
            continue;
        };
        let other = WorldId(index);
        if other == world {
            continue;
        }
        let (Some(global_arc), target) = (global.arc(), target.clone()) else {
            return;
        };
        global.run_in_world(other, move |w| {
            let Some((pos, rot)) = select::find_uuid(w.entities(), to)
                .and_then(|e| w.entities().store.get(e))
                .map(|e| (e.pos(), e.rot()))
            else {
                return;
            };
            super::exec::move_to_world(
                &global_arc,
                &target,
                other,
                pos,
                Some((rot.y_rot, rot.x_rot)),
            );
        });
    }
}

/// `GameRule.deserialize` with the rule's Brigadier argument: `true`/`false`, or an integer
/// in the rule's bounds, without trailing characters.
fn deserialize(rule: Rule, value: &str) -> Option<i32> {
    let mut reader = StringReader::new(value);
    let parsed = match rule.info().kind {
        RuleType::Bool => i32::from(reader.read_boolean().ok()?),
        RuleType::Int { min, max } => {
            let v = reader.read_int().ok()?;
            if v < min || v > max {
                return None;
            }
            v
        }
    };
    (!reader.can_read()).then_some(parsed)
}

/// `broadcastGameRuleChangeToOperators`: `commands.gamerule.set` to every operator online.
fn broadcast_to_operators(global: &Global, rule: Rule, value: i32) {
    let message = Component::translatable_args(
        "commands.gamerule.set",
        vec![
            Arg::String(rule.info().name.to_owned()),
            Arg::String(rule.serialize(value)),
        ],
    );
    for entry in global.players.snapshot() {
        if global.access.op_level(entry.uuid.0).is_none() {
            continue;
        }
        let accepts = entry
            .inbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .accepts_system(false);
        if accepts {
            entry.sink.send(&SystemChat {
                content: message.clone(),
                overlay: false,
            });
        }
    }
}

/// `Difficulty.getDisplayName` in English, for the log.
fn difficulty_display(d: vanillite_game::survival::rules::Difficulty) -> &'static str {
    use vanillite_game::survival::rules::Difficulty;
    match d {
        Difficulty::Peaceful => "Peaceful",
        Difficulty::Easy => "Easy",
        Difficulty::Normal => "Normal",
        Difficulty::Hard => "Hard",
    }
}

/// `GameType.getShortDisplayName` in English, for the log.
fn short_mode_name(mode: GameMode) -> &'static str {
    match mode {
        GameMode::Survival => "Survival",
        GameMode::Creative => "Creative",
        GameMode::Adventure => "Adventure",
        GameMode::Spectator => "Spectator",
    }
}
