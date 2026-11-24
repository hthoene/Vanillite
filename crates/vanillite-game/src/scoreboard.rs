//! The scoreboard: objectives, scores, display slots and teams (Vanilla's `Scoreboard`,
//! `Objective`, `Score`, `PlayerTeam`). Pure data; every change is recorded as a
//! [`ScoreEvent`] the server turns into packets (`ServerScoreboard`) and a dirty flag for saving.

use std::collections::BTreeMap;

use vanillite_base::hash::FxHashMap;
use vanillite_protocol::text::{Component, Style};

/// How an objective is shown (`ObjectiveCriteria.RenderType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderType {
    /// `integer`.
    #[default]
    Integer,
    /// `hearts`.
    Hearts,
}

impl RenderType {
    /// The serialized name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Integer => "integer",
            Self::Hearts => "hearts",
        }
    }

    /// By serialized name.
    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "integer" => Some(Self::Integer),
            "hearts" => Some(Self::Hearts),
            _ => None,
        }
    }
}

/// How a score is shown (`NumberFormat`).
#[derive(Debug, Clone, PartialEq)]
pub enum NumberFormat {
    /// `blank`.
    Blank,
    /// `styled`.
    Styled(Box<Style>),
    /// `fixed`.
    Fixed(Box<Component>),
}

/// What feeds an objective (`ObjectiveCriteria`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CriteriaKind {
    /// `dummy`.
    Dummy,
    /// `trigger`.
    Trigger,
    /// `deathCount`.
    DeathCount,
    /// `playerKillCount`.
    PlayerKillCount,
    /// `totalKillCount`.
    TotalKillCount,
    /// `health`.
    Health,
    /// `food`.
    Food,
    /// `air`.
    Air,
    /// `armor`.
    Armor,
    /// `xp`.
    Experience,
    /// `level`.
    Level,
    /// `teamkill.<color>`: the `ChatFormatting` colour id.
    TeamKill(u8),
    /// `killedByTeam.<color>`.
    KilledByTeam(u8),
    /// `<stat type>:<value>` with both ids written `namespace.path`.
    Stat(String),
}

/// The colour names of `ChatFormatting` 0..=15.
pub const COLOR_NAMES: [&str; 16] = [
    "black",
    "dark_blue",
    "dark_green",
    "dark_aqua",
    "dark_red",
    "dark_purple",
    "gold",
    "gray",
    "dark_gray",
    "blue",
    "green",
    "aqua",
    "red",
    "light_purple",
    "yellow",
    "white",
];

/// The colour id of a `ChatFormatting` colour name.
pub fn color_id(name: &str) -> Option<u8> {
    COLOR_NAMES
        .iter()
        .position(|n| *n == name)
        .and_then(|i| u8::try_from(i).ok())
}

impl CriteriaKind {
    /// By name (`ObjectiveCriteria.byName`); stat criteria are checked by the caller.
    pub fn by_name(name: &str) -> Option<Self> {
        Some(match name {
            "dummy" => Self::Dummy,
            "trigger" => Self::Trigger,
            "deathCount" => Self::DeathCount,
            "playerKillCount" => Self::PlayerKillCount,
            "totalKillCount" => Self::TotalKillCount,
            "health" => Self::Health,
            "food" => Self::Food,
            "air" => Self::Air,
            "armor" => Self::Armor,
            "xp" => Self::Experience,
            "level" => Self::Level,
            _ => {
                if let Some(c) = name.strip_prefix("teamkill.") {
                    return color_id(c).map(Self::TeamKill);
                }
                if let Some(c) = name.strip_prefix("killedByTeam.") {
                    return color_id(c).map(Self::KilledByTeam);
                }
                if name.contains(':') {
                    return Some(Self::Stat(name.to_owned()));
                }
                return None;
            }
        })
    }

    /// The name.
    pub fn name(&self) -> String {
        match self {
            Self::Dummy => "dummy".into(),
            Self::Trigger => "trigger".into(),
            Self::DeathCount => "deathCount".into(),
            Self::PlayerKillCount => "playerKillCount".into(),
            Self::TotalKillCount => "totalKillCount".into(),
            Self::Health => "health".into(),
            Self::Food => "food".into(),
            Self::Air => "air".into(),
            Self::Armor => "armor".into(),
            Self::Experience => "xp".into(),
            Self::Level => "level".into(),
            Self::TeamKill(c) => format!("teamkill.{}", color_name(*c)),
            Self::KilledByTeam(c) => format!("killedByTeam.{}", color_name(*c)),
            Self::Stat(s) => s.clone(),
        }
    }

    /// Whether commands may not change its scores.
    pub fn is_read_only(&self) -> bool {
        matches!(
            self,
            Self::Health | Self::Food | Self::Air | Self::Armor | Self::Experience | Self::Level
        )
    }

    /// The default render type.
    pub fn default_render_type(&self) -> RenderType {
        if *self == Self::Health {
            RenderType::Hearts
        } else {
            RenderType::Integer
        }
    }
}

fn color_name(id: u8) -> &'static str {
    COLOR_NAMES.get(usize::from(id)).copied().unwrap_or("white")
}

/// An objective.
#[derive(Debug, Clone, PartialEq)]
pub struct Objective {
    /// Name.
    pub name: String,
    /// Criteria.
    pub criteria: CriteriaKind,
    /// Display name.
    pub display_name: Component,
    /// Render type.
    pub render_type: RenderType,
    /// Score display names follow the holders' display names.
    pub display_auto_update: bool,
    /// Number format.
    pub number_format: Option<NumberFormat>,
}

impl Objective {
    /// `getFormattedDisplayName`: the display name with a hover of the name, in brackets.
    pub fn formatted_display_name(&self) -> Component {
        use vanillite_protocol::text::HoverEvent;
        Component::text("[")
            .append(
                self.display_name
                    .clone()
                    .hover(HoverEvent::ShowText(Component::text(self.name.as_str()))),
            )
            .append(Component::text("]"))
    }
}

/// A score.
#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    /// Value.
    pub value: i32,
    /// For `trigger` objectives: locked until `scoreboard players enable`.
    pub locked: bool,
    /// Display name override.
    pub display: Option<Component>,
    /// Number format override.
    pub number_format: Option<NumberFormat>,
}

impl Default for Score {
    fn default() -> Self {
        Score {
            value: 0,
            locked: true,
            display: None,
            number_format: None,
        }
    }
}

/// `Team.Visibility`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    /// `always`.
    #[default]
    Always,
    /// `never`.
    Never,
    /// `hideForOtherTeams`.
    HideForOtherTeams,
    /// `hideForOwnTeam`.
    HideForOwnTeam,
}

impl Visibility {
    /// All, in id order.
    pub const ALL: [Visibility; 4] = [
        Self::Always,
        Self::Never,
        Self::HideForOtherTeams,
        Self::HideForOwnTeam,
    ];

    /// The name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Never => "never",
            Self::HideForOtherTeams => "hideForOtherTeams",
            Self::HideForOwnTeam => "hideForOwnTeam",
        }
    }

    /// The network id.
    pub fn id(self) -> i32 {
        match self {
            Self::Always => 0,
            Self::Never => 1,
            Self::HideForOtherTeams => 2,
            Self::HideForOwnTeam => 3,
        }
    }

    /// By name.
    pub fn by_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.name() == name)
    }
}

/// `Team.CollisionRule`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CollisionRule {
    /// `always`.
    #[default]
    Always,
    /// `never`.
    Never,
    /// `pushOtherTeams`.
    PushOtherTeams,
    /// `pushOwnTeam`.
    PushOwnTeam,
}

impl CollisionRule {
    /// All, in id order.
    pub const ALL: [CollisionRule; 4] = [
        Self::Always,
        Self::Never,
        Self::PushOtherTeams,
        Self::PushOwnTeam,
    ];

    /// The name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Never => "never",
            Self::PushOtherTeams => "pushOtherTeams",
            Self::PushOwnTeam => "pushOwnTeam",
        }
    }

    /// The network id.
    pub fn id(self) -> i32 {
        match self {
            Self::Always => 0,
            Self::Never => 1,
            Self::PushOtherTeams => 2,
            Self::PushOwnTeam => 3,
        }
    }

    /// By name.
    pub fn by_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.name() == name)
    }
}

/// A team (`PlayerTeam`).
#[derive(Debug, Clone, PartialEq)]
pub struct Team {
    /// Name.
    pub name: String,
    /// Display name.
    pub display_name: Component,
    /// Prefix of member names.
    pub prefix: Component,
    /// Suffix of member names.
    pub suffix: Component,
    /// `ChatFormatting` colour id, `None` = reset.
    pub color: Option<u8>,
    /// `friendlyFire`.
    pub allow_friendly_fire: bool,
    /// `seeFriendlyInvisibles`.
    pub see_friendly_invisibles: bool,
    /// `nametagVisibility`.
    pub nametag_visibility: Visibility,
    /// `deathMessageVisibility`.
    pub death_message_visibility: Visibility,
    /// `collisionRule`.
    pub collision_rule: CollisionRule,
    /// Members in join order.
    pub players: Vec<String>,
}

impl Team {
    /// A team with Vanilla's defaults.
    pub fn new(name: &str) -> Team {
        Team {
            name: name.to_owned(),
            display_name: Component::text(name),
            prefix: Component::empty(),
            suffix: Component::empty(),
            color: None,
            allow_friendly_fire: true,
            see_friendly_invisibles: true,
            nametag_visibility: Visibility::Always,
            death_message_visibility: Visibility::Always,
            collision_rule: CollisionRule::Always,
            players: Vec::new(),
        }
    }

    /// The options byte of the team packet.
    pub fn pack_options(&self) -> u8 {
        u8::from(self.allow_friendly_fire) | (u8::from(self.see_friendly_invisibles) << 1)
    }

    /// `getFormattedDisplayName`: the display name in the team style with a hover of the name,
    /// in brackets.
    pub fn formatted_display_name(&self) -> Component {
        use vanillite_protocol::text::HoverEvent;
        let mut inner = self
            .display_name
            .clone()
            .hover(HoverEvent::ShowText(Component::text(self.name.as_str())))
            .insertion(self.name.clone());
        if let Some(color) = self.color.and_then(named_color) {
            inner = inner.color(vanillite_protocol::text::Color::Named(color));
        }
        Component::text("[").append(inner).append(Component::text("]"))
    }

    /// `PlayerTeam.formatNameForTeam`: prefix, the name in the team colour, suffix.
    pub fn format_name(&self, name: Component) -> Component {
        let mut name_part = Component::empty().append(name);
        if let Some(color) = self.color.and_then(named_color) {
            name_part = name_part.color(vanillite_protocol::text::Color::Named(color));
        }
        Component::empty()
            .append(self.prefix.clone())
            .append(name_part)
            .append(self.suffix.clone())
    }
}

/// The named text colour of a `ChatFormatting` colour id.
pub fn named_color(id: u8) -> Option<vanillite_protocol::text::NamedColor> {
    vanillite_protocol::text::NamedColor::from_name(color_name(id))
}

/// A change of the scoreboard the server sends to players.
#[derive(Debug, Clone, PartialEq)]
pub enum ScoreEvent {
    /// `onObjectiveAdded`.
    ObjectiveAdded(String),
    /// `onObjectiveChanged`.
    ObjectiveChanged(String),
    /// `onObjectiveRemoved` (with its objective as it was).
    ObjectiveRemoved(Box<Objective>),
    /// `onScoreChanged`.
    ScoreChanged { holder: String, objective: String },
    /// `onPlayerScoreRemoved`.
    ScoreRemoved { holder: String, objective: String },
    /// `onPlayerRemoved`: all scores of the holder.
    HolderRemoved(String),
    /// `setDisplayObjective`: the slot id and the objective before.
    DisplaySlot { slot: u8, before: Option<String> },
    /// `onTeamAdded`.
    TeamAdded(String),
    /// `onTeamChanged`.
    TeamChanged(String),
    /// `onTeamRemoved`.
    TeamRemoved(String),
    /// `addPlayerToTeam`.
    TeamJoined { team: String, holder: String },
    /// `removePlayerFromTeam`.
    TeamLeft { team: String, holder: String },
}

/// Display slots: list, sidebar, below name and the 16 team sidebars.
pub const DISPLAY_SLOTS: usize = 19;

/// The scoreboard of the server.
#[derive(Debug, Default)]
pub struct Scoreboard {
    objectives: BTreeMap<String, Objective>,
    /// Objective names in creation order (`objectivesByName` iteration and the save order).
    objective_order: Vec<String>,
    scores: FxHashMap<String, BTreeMap<String, Score>>,
    /// Holders in first-score order.
    holder_order: Vec<String>,
    display: [Option<String>; DISPLAY_SLOTS],
    teams: BTreeMap<String, Team>,
    team_order: Vec<String>,
    team_of: FxHashMap<String, String>,
    events: Vec<ScoreEvent>,
    dirty: bool,
}

impl Scoreboard {
    /// An empty scoreboard.
    pub fn new() -> Scoreboard {
        Scoreboard::default()
    }

    fn changed(&mut self, event: ScoreEvent) {
        self.events.push(event);
        self.dirty = true;
    }

    /// Takes the changes since the last call.
    pub fn take_events(&mut self) -> Vec<ScoreEvent> {
        std::mem::take(&mut self.events)
    }

    /// Whether it changed since the last save; clears the flag.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// An objective.
    pub fn objective(&self, name: &str) -> Option<&Objective> {
        self.objectives.get(name)
    }

    /// The objectives in creation order.
    pub fn objectives(&self) -> impl Iterator<Item = &Objective> {
        self.objective_order
            .iter()
            .filter_map(|n| self.objectives.get(n))
    }

    /// `addObjective`; `false` if the name is taken.
    pub fn add_objective(&mut self, objective: Objective) -> bool {
        if self.objectives.contains_key(&objective.name) {
            return false;
        }
        let name = objective.name.clone();
        self.objective_order.push(name.clone());
        self.objectives.insert(name.clone(), objective);
        self.changed(ScoreEvent::ObjectiveAdded(name));
        true
    }

    /// Changes an objective (`onObjectiveChanged`).
    pub fn modify_objective(&mut self, name: &str, f: impl FnOnce(&mut Objective)) -> bool {
        let Some(o) = self.objectives.get_mut(name) else {
            return false;
        };
        f(o);
        self.changed(ScoreEvent::ObjectiveChanged(name.to_owned()));
        true
    }

    /// `removeObjective`: clears its display slots and scores.
    pub fn remove_objective(&mut self, name: &str) -> bool {
        let Some(objective) = self.objectives.remove(name) else {
            return false;
        };
        self.objective_order.retain(|n| n != name);
        for slot in 0..DISPLAY_SLOTS {
            if self.display.get(slot).and_then(Option::as_deref) == Some(name)
                && let Ok(slot) = u8::try_from(slot)
            {
                self.set_display(slot, None);
            }
        }
        for scores in self.scores.values_mut() {
            scores.remove(name);
        }
        self.changed(ScoreEvent::ObjectiveRemoved(Box::new(objective)));
        true
    }

    /// The objective in a display slot.
    pub fn display(&self, slot: u8) -> Option<&str> {
        self.display.get(usize::from(slot)).and_then(Option::as_deref)
    }

    /// `setDisplayObjective`.
    pub fn set_display(&mut self, slot: u8, objective: Option<&str>) {
        let Some(entry) = self.display.get_mut(usize::from(slot)) else {
            return;
        };
        let before = std::mem::replace(entry, objective.map(str::to_owned));
        self.changed(ScoreEvent::DisplaySlot { slot, before });
    }

    /// The display slots an objective is in.
    pub fn slots_of(&self, objective: &str) -> Vec<u8> {
        (0..DISPLAY_SLOTS)
            .filter_map(|i| {
                (self.display.get(i).and_then(Option::as_deref) == Some(objective))
                    .then(|| u8::try_from(i).ok())
                    .flatten()
            })
            .collect()
    }

    /// A score, if it exists (`getPlayerScoreInfo`).
    pub fn score(&self, holder: &str, objective: &str) -> Option<&Score> {
        self.scores.get(holder)?.get(objective)
    }

    /// The scores of a holder by objective name.
    pub fn scores_of(&self, holder: &str) -> Option<&BTreeMap<String, Score>> {
        self.scores.get(holder)
    }

    /// Holders with any score, in first-score order (`getTrackedPlayers`).
    pub fn holders(&self) -> impl Iterator<Item = &str> {
        self.holder_order
            .iter()
            .filter(|h| self.scores.get(*h).is_some_and(|s| !s.is_empty()))
            .map(String::as_str)
    }

    /// The scores of an objective as (holder, score), in holder order (`listPlayerScores`).
    pub fn scores_in(&self, objective: &str) -> Vec<(&str, &Score)> {
        self.holder_order
            .iter()
            .filter_map(|h| Some((h.as_str(), self.scores.get(h)?.get(objective)?)))
            .collect()
    }

    /// `getOrCreatePlayerScore(...).set(value)`. `display` is the holder's display name for
    /// objectives with `displayAutoUpdate`. Returns `false` when the objective is unknown.
    pub fn set_score(
        &mut self,
        holder: &str,
        objective: &str,
        value: i32,
        display: Option<&Component>,
    ) -> bool {
        let Some(auto) = self.objectives.get(objective).map(|o| o.display_auto_update) else {
            return false;
        };
        let (score, new) = self.score_entry(holder, objective);
        let mut changed = new;
        if auto
            && let Some(d) = display
            && score.display.as_ref() != Some(d)
        {
            score.display = Some(d.clone());
            changed = true;
        }
        if score.value != value {
            score.value = value;
            changed = true;
        }
        if changed {
            self.changed(ScoreEvent::ScoreChanged {
                holder: holder.to_owned(),
                objective: objective.to_owned(),
            });
        }
        true
    }

    /// Creates the score if missing (unsynced like Vanilla's `getOrCreate`) and returns it.
    fn score_entry(&mut self, holder: &str, objective: &str) -> (&mut Score, bool) {
        if !self.scores.contains_key(holder) {
            self.holder_order.push(holder.to_owned());
        }
        let scores = self.scores.entry(holder.to_owned()).or_default();
        let new = !scores.contains_key(objective);
        (scores.entry(objective.to_owned()).or_default(), new)
    }

    /// Ensures the score exists (`getOrCreatePlayerScore` without a change).
    pub fn touch_score(&mut self, holder: &str, objective: &str) {
        if self.objectives.contains_key(objective) {
            self.score_entry(holder, objective);
        }
    }

    /// Sets or clears the lock of a score (`lock`/`unlock`), creating it.
    pub fn set_locked(&mut self, holder: &str, objective: &str, locked: bool) {
        if !self.objectives.contains_key(objective) {
            return;
        }
        let (score, new) = self.score_entry(holder, objective);
        score.locked = locked;
        if new {
            self.changed(ScoreEvent::ScoreChanged {
                holder: holder.to_owned(),
                objective: objective.to_owned(),
            });
        } else {
            self.dirty = true;
        }
    }

    /// Changes the display name override of a score (`display`).
    pub fn set_score_display(&mut self, holder: &str, objective: &str, display: Option<Component>) {
        if !self.objectives.contains_key(objective) {
            return;
        }
        let (score, new) = self.score_entry(holder, objective);
        if new || score.display != display {
            score.display = display;
            self.changed(ScoreEvent::ScoreChanged {
                holder: holder.to_owned(),
                objective: objective.to_owned(),
            });
        }
    }

    /// Changes the number format override of a score.
    pub fn set_score_format(
        &mut self,
        holder: &str,
        objective: &str,
        format: Option<NumberFormat>,
    ) {
        if !self.objectives.contains_key(objective) {
            return;
        }
        let (score, _) = self.score_entry(holder, objective);
        score.number_format = format;
        self.changed(ScoreEvent::ScoreChanged {
            holder: holder.to_owned(),
            objective: objective.to_owned(),
        });
    }

    /// `resetSinglePlayerScore`.
    pub fn reset_score(&mut self, holder: &str, objective: &str) -> bool {
        let Some(scores) = self.scores.get_mut(holder) else {
            return false;
        };
        if scores.remove(objective).is_none() {
            return false;
        }
        if scores.is_empty() {
            self.scores.remove(holder);
            self.holder_order.retain(|h| h != holder);
            self.changed(ScoreEvent::HolderRemoved(holder.to_owned()));
        } else {
            self.changed(ScoreEvent::ScoreRemoved {
                holder: holder.to_owned(),
                objective: objective.to_owned(),
            });
        }
        true
    }

    /// `resetAllPlayerScores`.
    pub fn reset_all(&mut self, holder: &str) -> bool {
        if self.scores.remove(holder).is_none() {
            return false;
        }
        self.holder_order.retain(|h| h != holder);
        self.changed(ScoreEvent::HolderRemoved(holder.to_owned()));
        true
    }

    /// The objectives of a criteria.
    pub fn objectives_of(&self, criteria: &CriteriaKind) -> Vec<String> {
        self.objectives()
            .filter(|o| o.criteria == *criteria)
            .map(|o| o.name.clone())
            .collect()
    }

    /// `forAllObjectives(criteria, holder, op)`: applies `f` to the score of every objective of
    /// the criteria (writable regardless of read-only).
    pub fn for_criteria(
        &mut self,
        criteria: &CriteriaKind,
        holder: &str,
        display: Option<&Component>,
        mut f: impl FnMut(i32) -> i32,
    ) {
        for objective in self.objectives_of(criteria) {
            let current = self.score(holder, &objective).map_or(0, |s| s.value);
            self.set_score(holder, &objective, f(current), display);
        }
    }

    /// A team.
    pub fn team(&self, name: &str) -> Option<&Team> {
        self.teams.get(name)
    }

    /// The teams in creation order.
    pub fn teams(&self) -> impl Iterator<Item = &Team> {
        self.team_order.iter().filter_map(|n| self.teams.get(n))
    }

    /// `addPlayerTeam`; `false` if the name is taken.
    pub fn add_team(&mut self, name: &str) -> bool {
        if self.teams.contains_key(name) {
            return false;
        }
        self.teams.insert(name.to_owned(), Team::new(name));
        self.team_order.push(name.to_owned());
        self.changed(ScoreEvent::TeamAdded(name.to_owned()));
        true
    }

    /// Changes a team (`onTeamChanged`).
    pub fn modify_team(&mut self, name: &str, f: impl FnOnce(&mut Team)) -> bool {
        let Some(t) = self.teams.get_mut(name) else {
            return false;
        };
        f(t);
        self.changed(ScoreEvent::TeamChanged(name.to_owned()));
        true
    }

    /// `removePlayerTeam`.
    pub fn remove_team(&mut self, name: &str) -> bool {
        let Some(team) = self.teams.remove(name) else {
            return false;
        };
        self.team_order.retain(|n| n != name);
        for p in &team.players {
            self.team_of.remove(p);
        }
        self.changed(ScoreEvent::TeamRemoved(name.to_owned()));
        true
    }

    /// The team of a holder (`getPlayersTeam`).
    pub fn team_of(&self, holder: &str) -> Option<&Team> {
        self.teams.get(self.team_of.get(holder)?)
    }

    /// `addPlayerToTeam`: leaves the old team first. `false` if already in this team or the
    /// team is unknown.
    pub fn join_team(&mut self, holder: &str, team: &str) -> bool {
        if !self.teams.contains_key(team) {
            return false;
        }
        if let Some(old) = self.team_of.get(holder).cloned() {
            if old == team {
                return false;
            }
            self.leave_team(holder);
        }
        self.team_of.insert(holder.to_owned(), team.to_owned());
        if let Some(t) = self.teams.get_mut(team) {
            t.players.push(holder.to_owned());
        }
        self.changed(ScoreEvent::TeamJoined {
            team: team.to_owned(),
            holder: holder.to_owned(),
        });
        true
    }

    /// `removePlayerFromTeam`; `false` if the holder is in no team.
    pub fn leave_team(&mut self, holder: &str) -> bool {
        let Some(team) = self.team_of.remove(holder) else {
            return false;
        };
        if let Some(t) = self.teams.get_mut(&team) {
            t.players.retain(|p| p != holder);
        }
        self.changed(ScoreEvent::TeamLeft {
            team,
            holder: holder.to_owned(),
        });
        true
    }
}
