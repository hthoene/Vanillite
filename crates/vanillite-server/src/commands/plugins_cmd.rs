//! Paper's `plugins` (`pl`) and Bukkit's `version` (`ver`, `about`): the plugins of the server,
//! native ones and those the Paper/Spigot layer hosts, and the version of the server or of a
//! plugin. Also the `plugins` field of Query.

use vanillite_plugin_host::PluginEntry;
use vanillite_protocol::text::{ClickEvent, Color, Component, HoverEvent, NamedColor};

use super::source::CommandSource;
use crate::global::Global;

/// Colour of the info icon of `/plugins`.
const INFO: Color = Color::Rgb(0x34_9F_DA);
/// Header colour of native plugins (and of a `Paper` group).
const NATIVE_HEADER: Color = Color::Rgb(0x02_88_D1);
/// Header colour of the `Bukkit` group.
const BUKKIT_HEADER: Color = Color::Rgb(0xED_81_06);
/// Author, website and licence of the server, shown by `/version`.
pub(super) const AUTHOR: &str = env!("CARGO_PKG_AUTHORS");
pub(super) const WEBSITE: &str = env!("CARGO_PKG_HOMEPAGE");
pub(super) const LICENSE: &str = env!("CARGO_PKG_LICENSE");
/// Plugin names per line of `/plugins`.
const PER_LINE: usize = 10;

fn named(text: impl Into<String>, color: NamedColor) -> Component {
    Component::text(text).color(Color::Named(color))
}

/// `/plugins`: `ℹ Server Plugins (N):`, then a section per group with the names sorted, green
/// when enabled and red when not; a name shows its version and description on hover and runs
/// `/version <name>` on click.
pub(crate) fn plugins(source: &CommandSource, global: &Global) -> i32 {
    let list = global.plugins.list();
    for line in plugins_lines(&list) {
        source.send_system(global, line);
    }
    i32::try_from(list.len()).unwrap_or(i32::MAX)
}

fn plugins_lines(list: &[PluginEntry]) -> Vec<Component> {
    let mut groups: Vec<(Option<&str>, Vec<&PluginEntry>)> = vec![(None, Vec::new())];
    for entry in list {
        let group = entry.group.as_deref();
        match groups.iter_mut().find(|(g, _)| *g == group) {
            Some((_, members)) => members.push(entry),
            None => groups.push((group, vec![entry])),
        }
    }
    let info = Component::text("ℹ ")
        .color(INFO)
        .hover(HoverEvent::ShowText(
            Component::text("Server plugins add features and commands to the server.").color(INFO),
        ));
    let mut lines = vec![Component::empty().append(info).append(named(
        format!("Server Plugins ({}):", list.len()),
        NamedColor::White,
    ))];
    for (group, mut members) in groups {
        if members.is_empty() {
            continue;
        }
        members.sort_by_key(|e| e.name.to_lowercase());
        let (title, color) = match group {
            None => ("Vanillite", NATIVE_HEADER),
            Some(g @ "Bukkit") => (g, BUKKIT_HEADER),
            Some(g @ "Paper") => (g, NATIVE_HEADER),
            Some(g) => (g, Color::Named(NamedColor::Gold)),
        };
        lines.push(Component::text(format!("{title} Plugins ({}):", members.len())).color(color));
        for (i, row) in members.chunks(PER_LINE).enumerate() {
            let mut line = if i == 0 {
                Component::text(" ").append(named("-", NamedColor::DarkGray))
            } else {
                Component::text("  ")
            };
            line = line.append(Component::text(" "));
            for (j, entry) in row.iter().enumerate() {
                if j > 0 {
                    line = line.append(named(", ", NamedColor::White));
                }
                line = line.append(name(entry));
            }
            lines.push(line);
        }
    }
    lines
}

/// A plugin's name in `/plugins`.
fn name(entry: &PluginEntry) -> Component {
    let mut hover = Component::text(format!("Version: {}", entry.version));
    if !entry.description.is_empty() {
        hover = hover.append(Component::text(format!("\n{}", entry.description)));
    }
    named(
        entry.name.clone(),
        if entry.enabled {
            NamedColor::Green
        } else {
            NamedColor::Red
        },
    )
    .hover(HoverEvent::ShowText(hover))
    .click(ClickEvent::RunCommand(format!("/version {}", entry.name)))
}

/// `/version [<plugin>]`.
pub(crate) fn version(plugin: Option<&str>, source: &CommandSource, global: &Global) -> i32 {
    let list = global.plugins.list();
    let lines = version_lines(&global.version, &list, plugin);
    for line in lines {
        source.send_system(global, line);
    }
    1
}

fn version_lines(server: &str, list: &[PluginEntry], plugin: Option<&str>) -> Vec<Component> {
    let Some(query) = plugin.map(str::trim).filter(|q| !q.is_empty()) else {
        return vec![
            Component::text(format!("This server is running {server}"))
                .hover(HoverEvent::ShowText(Component::text(
                    "Click to copy to clipboard",
                )))
                .click(ClickEvent::CopyToClipboard(server.to_owned())),
            Component::text("Author: ").append(named(AUTHOR, NamedColor::Green)),
            Component::text("Website: ").append(
                named(WEBSITE, NamedColor::Green).click(ClickEvent::OpenUrl(WEBSITE.to_owned())),
            ),
            Component::text("License: ").append(named(LICENSE, NamedColor::Green)),
        ];
    };
    // `VersionCommand`: the plugin of that exact name, else every one whose name contains it.
    let matches: Vec<&PluginEntry> = match list.iter().find(|e| e.name == query) {
        Some(exact) => vec![exact],
        None => {
            let lower = query.to_lowercase();
            list.iter()
                .filter(|e| e.name.to_lowercase().contains(&lower))
                .collect()
        }
    };
    if matches.is_empty() {
        return vec![
            Component::text("This server is not running any plugin by that name."),
            Component::text("Use /plugins to get a list of plugins."),
        ];
    }
    matches.into_iter().flat_map(describe).collect()
}

/// `VersionCommand.describeToSender`.
fn describe(entry: &PluginEntry) -> Vec<Component> {
    let mut lines = vec![
        named(entry.name.clone(), NamedColor::Green)
            .append(named(" version ", NamedColor::White))
            .append(named(entry.version.clone(), NamedColor::Green)),
    ];
    if !entry.description.is_empty() {
        lines.push(Component::text(entry.description.clone()));
    }
    if !entry.website.is_empty() {
        lines.push(
            Component::text("Website: ").append(named(entry.website.clone(), NamedColor::Green)),
        );
    }
    if !entry.authors.is_empty() {
        let label = if entry.authors.len() == 1 {
            "Author: "
        } else {
            "Authors: "
        };
        let mut line = Component::text(label);
        let last = entry.authors.len().saturating_sub(1);
        for (i, author) in entry.authors.iter().enumerate() {
            if i > 0 {
                let separator = if i == last { " and " } else { ", " };
                line = line.append(named(separator, NamedColor::White));
            }
            line = line.append(named(author.clone(), NamedColor::Green));
        }
        lines.push(line);
    }
    lines
}

/// Names for the tab completion of `/version <plugin>`.
pub(crate) fn names(global: &Global) -> Vec<String> {
    global.plugins.list().into_iter().map(|e| e.name).collect()
}

/// The `plugins` field of Query (`DedicatedServer.getPluginNames` on Bukkit):
/// `<version>: <name> <version>; ...`, only the version without plugins.
pub(crate) fn query_plugins(server: &str, list: &[PluginEntry]) -> String {
    let mut out = server.to_owned();
    for (i, entry) in list.iter().enumerate() {
        out.push_str(if i == 0 { ": " } else { "; " });
        out.push_str(&entry.name);
        out.push(' ');
        out.push_str(&entry.version.replace(';', ","));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(group: Option<&str>, name: &str, enabled: bool) -> PluginEntry {
        PluginEntry {
            group: group.map(str::to_owned),
            name: name.into(),
            version: "1.0".into(),
            description: String::new(),
            authors: Vec::new(),
            website: String::new(),
            enabled,
        }
    }

    fn plain(lines: &[Component]) -> Vec<String> {
        lines.iter().map(Component::plain).collect()
    }

    #[test]
    fn plugins_lists_groups_sorted_and_coloured() {
        let list = [
            entry(None, "worldtools", true),
            entry(None, "Auth", false),
            entry(Some("Bukkit"), "Vault", true),
            entry(Some("Bukkit"), "Essentials", true),
        ];
        let lines = plugins_lines(&list);
        assert_eq!(
            plain(&lines),
            [
                "ℹ Server Plugins (4):",
                "Vanillite Plugins (2):",
                " - Auth, worldtools",
                "Bukkit Plugins (2):",
                " - Essentials, Vault",
            ]
        );
        let json = lines
            .get(2)
            .map(Component::to_json_string)
            .unwrap_or_default();
        assert!(json.contains(r#""text":"Auth","color":"red""#), "{json}");
        assert!(
            json.contains(r#""text":"worldtools","color":"green""#),
            "{json}"
        );
        assert!(json.contains(r#""command":"/version Auth""#), "{json}");
        let header = lines
            .get(3)
            .map(Component::to_json_string)
            .unwrap_or_default();
        assert!(header.contains("#ED8106"), "{header}");
    }

    #[test]
    fn plugins_wraps_after_ten_names_and_lists_none() {
        let list: Vec<PluginEntry> = (0..12)
            .map(|i| entry(None, &format!("p{i:02}"), true))
            .collect();
        let lines = plain(&plugins_lines(&list));
        assert_eq!(lines.len(), 4);
        assert_eq!(
            lines.get(3).map(String::as_str),
            Some("   p10, p11"),
            "{lines:?}"
        );
        assert_eq!(plain(&plugins_lines(&[])), ["ℹ Server Plugins (0):"]);
    }

    #[test]
    fn version_shows_server_and_plugins() {
        let server = "Vanillite 0.1.0 (Minecraft 26.3, abc1234)";
        let mut essentials = entry(Some("Bukkit"), "Essentials", true);
        essentials.description = "Essential commands".into();
        essentials.website = "https://example.org".into();
        essentials.authors = vec!["a".into(), "b".into(), "c".into()];
        let mut worldtools = entry(None, "worldtools", true);
        worldtools.authors = vec!["x".into()];
        let list = [essentials, worldtools, entry(None, "EssentialsChat", true)];
        assert_eq!(
            plain(&version_lines(server, &list, None)),
            [
                "This server is running Vanillite 0.1.0 (Minecraft 26.3, abc1234)",
                "Author: Hannes Thöne",
                "Website: https://vanillite.org",
                "License: Apache-2.0",
            ]
        );
        assert_eq!(
            plain(&version_lines(server, &list, Some("Essentials"))),
            [
                "Essentials version 1.0",
                "Essential commands",
                "Website: https://example.org",
                "Authors: a, b and c",
            ]
        );
        assert_eq!(
            plain(&version_lines(server, &list, Some("TOOLS"))),
            ["worldtools version 1.0", "Author: x"]
        );
        assert_eq!(
            plain(&version_lines(server, &list, Some("essentials"))),
            [
                "Essentials version 1.0",
                "Essential commands",
                "Website: https://example.org",
                "Authors: a, b and c",
                "EssentialsChat version 1.0",
            ]
        );
        assert_eq!(
            plain(&version_lines(server, &list, Some("nope"))),
            [
                "This server is not running any plugin by that name.",
                "Use /plugins to get a list of plugins.",
            ]
        );
    }

    #[test]
    fn query_names_the_plugins() {
        let mut odd = entry(None, "B", true);
        odd.version = "1;2".into();
        assert_eq!(query_plugins("V 1", &[]), "V 1");
        assert_eq!(
            query_plugins("V 1", &[entry(None, "A", true), odd]),
            "V 1: A 1.0; B 1,2"
        );
    }
}
