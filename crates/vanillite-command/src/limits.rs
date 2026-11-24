//! Limits of command parsing and suggestions (SEC-04, SEC-11). The server exposes each as a
//! setting; the defaults keep Vanilla's behaviour where Vanilla has a limit.

/// Limits of the command system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Longest command line that is parsed (Vanilla: the 32767 characters of the packet).
    pub max_command_length: usize,
    /// Longest input of a suggestion request; longer requests get no suggestions. Vanilla
    /// accepts 32500 characters; the default is far below that (SEC-11).
    pub max_suggestion_input: usize,
    /// Suggestions per answer (Vanilla's `MAX_COMMAND_SUGGESTIONS`).
    pub max_suggestions: usize,
    /// Characters of a message argument (Vanilla: 256).
    pub max_message_length: usize,
    /// Nesting depth of SNBT in arguments (Vanilla: 512).
    pub max_nbt_depth: u32,
    /// Nodes of an NBT path.
    pub max_nbt_path_nodes: usize,
    /// Options inside one entity selector `[...]`.
    pub max_selector_options: usize,
    /// Entity selectors inside one message argument.
    pub max_message_selectors: usize,
    /// Nested redirects followed while parsing one command.
    pub max_redirect_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_command_length: 32767,
            max_suggestion_input: 2048,
            max_suggestions: 1000,
            max_message_length: 256,
            max_nbt_depth: 512,
            max_nbt_path_nodes: 256,
            max_selector_options: 64,
            max_message_selectors: 16,
            max_redirect_depth: 64,
        }
    }
}
