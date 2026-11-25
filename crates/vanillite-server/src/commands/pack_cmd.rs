//! `DataPackCommand` and `ReloadCommand`.

// SEC-07: command arguments and pack names are untrusted input.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions
)]

use vanillite_command::error::CommandError;
use vanillite_command::{Arg as ErrArg, GameValue, Parsed};
use vanillite_datapack::repository::{self, Pack, PackSource, Position};
use vanillite_game::commands::{CommandId, DataPackOp, PackPosition};
use vanillite_protocol::text::{Arg, Color, Component, HoverEvent, NamedColor};

use super::Env;
use super::exec::format_list;
use super::source::CommandSource;
use crate::global::Global;

fn tr(key: &str, args: Vec<Component>) -> Component {
    Component::translatable(key, args)
}

/// A `translatableEscape` error with string arguments.
fn err(key: &'static str, args: &[&str]) -> CommandError {
    CommandError::new(
        key,
        args.iter().map(|a| ErrArg::Str((*a).to_owned())).collect(),
    )
}

fn count(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// `StringArgumentType.escapeIfRequired`.
pub(super) fn escape_if_required(text: &str) -> String {
    if text
        .chars()
        .all(vanillite_command::reader::is_allowed_in_unquoted_string)
    {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len().saturating_add(2));
    out.push('"');
    for c in text.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Runs `datapack` or `reload`.
pub(crate) fn run(
    parsed: &Parsed<CommandId>,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let global = env.global;
    match parsed.command {
        CommandId::Reload => Ok(reload(source, global)),
        CommandId::DataPack(DataPackOp::Enable(position)) => {
            enable(parsed, position, source, global)
        }
        CommandId::DataPack(DataPackOp::Disable) => disable(parsed, source, global),
        CommandId::DataPack(DataPackOp::List) => {
            Ok(list_enabled(source, global).saturating_add(list_available(source, global)))
        }
        CommandId::DataPack(DataPackOp::ListAvailable) => Ok(list_available(source, global)),
        CommandId::DataPack(DataPackOp::ListEnabled) => Ok(list_enabled(source, global)),
        CommandId::DataPack(DataPackOp::Create) => create(parsed, source, env),
        _ => Err(err("command.unknown.command", &[])),
    }
}

/// `ReloadCommand`: the selected packs and every new pack that is not disabled.
fn reload(source: &CommandSource, global: &Global) -> i32 {
    let ids = {
        let mut repo = global.packs.repository();
        // `discoverNewPacks`.
        let mut selected = repo.selected().to_vec();
        repo.reload();
        let disabled = global.level.disabled_packs();
        for pack in repo.available() {
            if !disabled.contains(&pack.id) && !selected.contains(&pack.id) {
                selected.push(pack.id.clone());
            }
        }
        selected
    };
    source.send_success(global, tr("commands.reload.success", vec![]), true);
    crate::packs::reload(global, ids, source.clone());
    0
}

/// `Pack.getChatLink`.
fn chat_link(pack: &Pack, enabled: bool) -> Component {
    let name = tr(
        "pack.nameAndSource",
        vec![
            Component::text(pack.id.as_str()),
            tr(pack.source.key(), vec![]),
        ],
    )
    .color(Color::Named(NamedColor::Gray));
    tr("chat.square_brackets", vec![name])
        .color(Color::Named(if enabled {
            NamedColor::Green
        } else {
            NamedColor::Red
        }))
        .insertion(escape_if_required(&pack.id))
        .hover(HoverEvent::ShowText(
            Component::empty()
                .append(Component::from_json(&pack.title))
                .append(Component::text("\n"))
                .append(Component::from_json(&pack.description)),
        ))
}

/// `DataPackCommand.getPack`: the pack of the argument `name`, which must (not) be enabled.
fn get_pack(
    parsed: &Parsed<CommandId>,
    name: &str,
    enabling: bool,
    global: &Global,
    repo: &repository::PackRepository,
) -> Result<Pack, CommandError> {
    let id = parsed.args.string(name).map_or("", String::as_str);
    let pack = repo
        .get(id)
        .ok_or_else(|| err("commands.datapack.unknown", &[id]))?;
    let enabled = repo.is_selected(id);
    if enabling && enabled {
        return Err(err("commands.datapack.enable.failed", &[id]));
    }
    if !enabling && !enabled {
        return Err(err("commands.datapack.disable.failed", &[id]));
    }
    let available = &global.packs.features;
    if !enabling && !pack.features.is_empty() && pack.source == PackSource::Feature {
        return Err(err("commands.datapack.disable.failed.feature", &[id]));
    }
    if !repository::features_allowed(&pack.features, available) {
        let missing = repository::missing_features(available, &pack.features);
        return Err(err(
            "commands.datapack.enable.failed.no_flags",
            &[id, &missing],
        ));
    }
    Ok(pack.clone())
}

/// `DataPackCommand.enablePack`.
fn enable(
    parsed: &Parsed<CommandId>,
    position: PackPosition,
    source: &CommandSource,
    global: &Global,
) -> Result<i32, CommandError> {
    let (pack, selected) = {
        let repo = global.packs.repository();
        let pack = get_pack(parsed, "name", true, global, &repo)?;
        let mut selected = repo.selected().to_vec();
        let index_of = |existing: &Pack, selected: &[String]| {
            selected
                .iter()
                .position(|id| *id == existing.id)
                .unwrap_or(0)
        };
        let index = match position {
            PackPosition::Default => match pack.position {
                Position::Top => selected.len(),
                Position::Bottom => 0,
            },
            PackPosition::First => 0,
            PackPosition::Last => selected.len(),
            PackPosition::Before => index_of(
                &get_pack(parsed, "existing", false, global, &repo)?,
                &selected,
            ),
            PackPosition::After => index_of(
                &get_pack(parsed, "existing", false, global, &repo)?,
                &selected,
            )
            .saturating_add(1),
        };
        selected.insert(index.min(selected.len()), pack.id.clone());
        (pack, selected)
    };
    source.send_success(
        global,
        tr(
            "commands.datapack.modify.enable",
            vec![chat_link(&pack, true)],
        ),
        true,
    );
    let size = count(selected.len());
    crate::packs::reload(global, selected, source.clone());
    Ok(size)
}

/// `DataPackCommand.disablePack`.
fn disable(
    parsed: &Parsed<CommandId>,
    source: &CommandSource,
    global: &Global,
) -> Result<i32, CommandError> {
    let (pack, selected) = {
        let repo = global.packs.repository();
        let pack = get_pack(parsed, "name", false, global, &repo)?;
        let mut selected = repo.selected().to_vec();
        if let Some(i) = selected.iter().position(|id| *id == pack.id) {
            selected.remove(i);
        }
        (pack, selected)
    };
    source.send_success(
        global,
        tr(
            "commands.datapack.modify.disable",
            vec![chat_link(&pack, true)],
        ),
        true,
    );
    let size = count(selected.len());
    crate::packs::reload(global, selected, source.clone());
    Ok(size)
}

/// `DataPackCommand.listAvailablePacks`: packs that are not selected and whose features the
/// world has.
fn list_available(source: &CommandSource, global: &Global) -> i32 {
    let links: Vec<Component> = {
        let mut repo = global.packs.repository();
        repo.reload();
        repo.available()
            .filter(|p| {
                !repo.is_selected(&p.id)
                    && repository::features_allowed(&p.features, &global.packs.features)
            })
            .map(|p| chat_link(p, false))
            .collect()
    };
    send_list(source, global, "commands.datapack.list.available", links)
}

/// `DataPackCommand.listEnabledPacks`.
fn list_enabled(source: &CommandSource, global: &Global) -> i32 {
    let links: Vec<Component> = {
        let mut repo = global.packs.repository();
        repo.reload();
        repo.selected_packs()
            .into_iter()
            .map(|p| chat_link(p, true))
            .collect()
    };
    send_list(source, global, "commands.datapack.list.enabled", links)
}

/// `<key>.none`, or `<key>.success` with the count and the links; returns the count.
fn send_list(source: &CommandSource, global: &Global, key: &str, links: Vec<Component>) -> i32 {
    let n = count(links.len());
    let message = if links.is_empty() {
        tr(&format!("{key}.none"), vec![])
    } else {
        Component::translatable_args(
            format!("{key}.success"),
            vec![Arg::Int(n), Arg::from(format_list(links))],
        )
    };
    source.send_success(global, message, false);
    n
}

/// `FileUtil.isValidPathSegment`: `[-._a-z0-9]+`, not `.` or `..`.
fn valid_path_segment(name: &str) -> bool {
    name != "."
        && name != ".."
        && !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.' | '_'))
}

/// `FileUtil.isPathPartPortable`: not a reserved Windows file name and not ending in `.`.
fn portable_path_part(name: &str) -> bool {
    if name.ends_with('.') {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "COM"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix)
            .is_some_and(|n| n.len() == 1 && n.chars().all(|c| ('1'..='9').contains(&c)))
    });
    !reserved
}

/// `pack.mcmeta` of a new pack: `PackMetadataSection` of this version's format, as Gson writes
/// it with a two-space indent.
fn pack_mcmeta(description: &Component) -> String {
    let (major, minor) = vanillite_data::version::DATA_PACK_VERSION;
    // `PackFormat.BOTTOM_CODEC`: just the major version when the minor one is 0.
    let min = if minor == 0 {
        major.to_string()
    } else {
        format!("[\n      {major},\n      {minor}\n    ]")
    };
    let description = serde_json::to_string_pretty(&description.to_json())
        .unwrap_or_else(|_| "\"\"".to_owned())
        .replace('\n', "\n    ");
    format!(
        "{{\n  \"pack\": {{\n    \"description\": {description},\n    \"min_format\": {min},\n    \"max_format\": {major}\n  }}\n}}"
    )
}

/// `DataPackCommand.createPack`: an empty pack in the world's `datapacks` folder.
fn create(
    parsed: &Parsed<CommandId>,
    source: &CommandSource,
    env: &Env<'_>,
) -> Result<i32, CommandError> {
    let global = env.global;
    let id = parsed.args.string("id").map_or("", String::as_str);
    let description = match parsed.args.game("description") {
        Some(GameValue::Component(c)) => super::text::resolve(env, source, c, None)?,
        _ => Component::empty(),
    };
    let folder = global.systems.level_dir.join("datapacks");
    if !valid_path_segment(id) {
        return Err(err("commands.datapack.create.invalid_name", &[id]));
    }
    if !portable_path_part(id) {
        return Err(err("commands.datapack.create.invalid_full_name", &[id]));
    }
    let dir = folder.join(id);
    if dir.exists() {
        return Err(err("commands.datapack.create.already_exists", &[id]));
    }
    let written = std::fs::create_dir(&dir)
        .and_then(|()| std::fs::create_dir(dir.join("data")))
        .and_then(|()| std::fs::write(dir.join("pack.mcmeta"), pack_mcmeta(&description)));
    if let Err(e) = written {
        log::warn!("Failed to create pack at {}: {e}", folder.display());
        return Err(err("commands.datapack.create.io_failure", &[id]));
    }
    source.send_success(
        global,
        Component::translatable_args(
            "commands.datapack.create.success",
            vec![Arg::String(id.to_owned())],
        ),
        true,
    );
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_metadata_like_vanilla() {
        assert_eq!(escape_if_required("vanilla"), "vanilla");
        assert_eq!(escape_if_required("file/a \"b\""), "\"file/a \\\"b\\\"\"");
        assert!(valid_path_segment("my_pack-1.0"));
        assert!(!valid_path_segment("My"));
        assert!(!valid_path_segment(".."));
        assert!(!valid_path_segment("a/b"));
        assert!(!portable_path_part("com"));
        assert!(!portable_path_part("con"));
        assert!(!portable_path_part("com1.txt"));
        assert!(!portable_path_part("lpt9"));
        assert!(!portable_path_part("pack."));
        assert!(portable_path_part("com10"));
        assert_eq!(
            pack_mcmeta(&Component::text("Hi")),
            "{\n  \"pack\": {\n    \"description\": \"Hi\",\n    \"min_format\": 121,\n    \"max_format\": 121\n  }\n}"
        );
    }
}
