//! What the command system needs to know about the one who runs a command.

use crate::parse::Args;
use crate::suggestion::SuggestionsBuilder;
use crate::tree::Requirement;

/// A command source: a player, the console or later a command block or function.
pub trait Source {
    /// Vanilla permission level 0..=4 (`op-permission-level` for operators).
    fn permission_level(&self) -> u8;

    /// Plugin permission nodes (hook for the plugin API; without plugins every node is
    /// granted and only the level counts).
    fn has_permission_node(&self, _node: &str) -> bool {
        true
    }

    /// Whether the source meets a node's requirement. Parsing and suggestions never walk
    /// nodes it does not meet (SEC-11).
    fn meets(&self, requirement: &Requirement) -> bool {
        self.permission_level() >= requirement.level
            && requirement
                .permission
                .is_none_or(|p| self.has_permission_node(p))
    }

    /// `commands/entity_selectors`: selectors are allowed from level 2 on.
    fn allows_selectors(&self) -> bool {
        self.permission_level() >= 2
    }

    /// Names of online players, for suggestions.
    fn online_player_names(&self) -> Vec<String>;

    /// Suggestions of a custom provider named at registration (Vanilla's `.suggests(...)`,
    /// sent to the client as `minecraft:ask_server`). `args` holds what is parsed so far.
    fn suggest_custom(&self, _provider: &str, _args: &Args, _builder: &mut SuggestionsBuilder<'_>) {
    }

    /// Whether a registry has an entry (resource arguments resolve at parse time).
    fn resource_exists(&self, registry: &str, id: &str) -> bool {
        vanillite_data::registry::Registry::by_name(registry).is_none_or(|r| r.id(id).is_some())
    }

    /// The entries of a registry, for suggestions. Static registries come from the generated
    /// data; the game adds data-driven ones.
    fn registry_entries(&self, registry: &str) -> Vec<String> {
        vanillite_data::registry::Registry::by_name(registry)
            .map(|r| r.entries.iter().map(|e| (*e).to_owned()).collect())
            .unwrap_or_default()
    }

    /// The tags of a registry without `#`, for suggestions.
    fn registry_tags(&self, _registry: &str) -> Vec<String> {
        Vec::new()
    }
}
