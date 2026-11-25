//! The Bedrock releases the frontend speaks and how their packets differ.
//!
//! Each supported network protocol is one [`Version`] in [`SUPPORTED`]: its generated data
//! (block palette, items, creative inventory: [`crate::data::Palette`]) and its packet layout,
//! expressed as the layout changes since the oldest supported protocol. Packet encoders and
//! decoders ask the version for these differences; everything else is shared.
//!
//! Adding a release: generate its palette (`tools/bedrock-data/generate.py`, see
//! `data/README.md`), add a [`crate::data::Palette`] if the palette version is new, add a
//! [`Version`] here (to [`SUPPORTED`], and as [`Version::latest`] if it is the newest) and, if
//! the release changed the layout of a packet the frontend uses, a layout method for the change.

use crate::data::{self, Palette};

/// Bedrock 1.26.50: the outer presence flags of optional fields are gone, item use carries the
/// hand.
const P26_50: i32 = 2193;
/// Bedrock 1.26.60: passenger-of-block data, swing hand, biome override of chunks, PlayFab id in
/// the player list instead of the skin, a new item stack request action (16).
const P26_60: i32 = 2225;

/// One supported Bedrock network protocol.
#[derive(Debug)]
pub struct Version {
    /// Network protocol number.
    pub protocol: i32,
    /// Bedrock releases speaking this protocol.
    pub releases: &'static str,
    /// Generated data of the release.
    pub palette: &'static Palette,
}

static V2168: Version = Version {
    protocol: 2168,
    releases: "1.26.40-1.26.44",
    palette: &data::V26_40,
};
static V2169: Version = Version {
    protocol: 2169,
    releases: "1.26.45",
    palette: &data::V26_40,
};
static V2193: Version = Version {
    protocol: P26_50,
    releases: "1.26.50-1.26.52",
    palette: &data::V26_50,
};
static V2225: Version = Version {
    protocol: P26_60,
    releases: "1.26.60-1.26.66",
    palette: &data::V26_60,
};

/// The supported protocols, oldest first.
pub static SUPPORTED: [&Version; 4] = [&V2168, &V2169, &V2193, &V2225];

impl Version {
    /// The version of a protocol number.
    pub fn of(protocol: i32) -> Option<&'static Version> {
        SUPPORTED.iter().copied().find(|v| v.protocol == protocol)
    }

    /// The newest supported version (advertised in the server list).
    pub fn latest() -> &'static Version {
        &V2225
    }

    /// The oldest supported version.
    pub fn oldest() -> &'static Version {
        &V2168
    }

    /// The release the version is advertised as (its first release).
    pub fn name(&self) -> &'static str {
        self.releases.split('-').next().unwrap_or(self.releases)
    }

    /// The newest release of the version.
    pub fn last_release(&self) -> &'static str {
        self.releases.rsplit('-').next().unwrap_or(self.releases)
    }

    /// Before 1.26.50, optional fields of `player_auth_input`, `inventory_transaction`,
    /// inventory actions and `item_stack_response` sit inside an always-present outer optional
    /// (an extra `true`), the input flags carry a presence flag, item use has no hand,
    /// `boss_event` carries a player id and `play_sound` neither the range bypass nor the
    /// playback position.
    #[inline]
    pub fn before_26_50(&self) -> bool {
        self.protocol < P26_50
    }

    /// Before 1.26.60: no passenger-of-block data, swing hand, chunk biome override or editor
    /// migration version; the PlayFab id is part of the skin, not of the player list entry; item
    /// stack request actions from 16 on are one lower (no reserved action).
    #[inline]
    pub fn before_26_60(&self) -> bool {
        self.protocol < P26_60
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_ordered_and_known() {
        assert!(SUPPORTED.is_sorted_by_key(|v| v.protocol));
        assert_eq!(
            SUPPORTED.first().map(|v| v.protocol),
            Some(Version::oldest().protocol)
        );
        assert_eq!(
            SUPPORTED.last().map(|v| v.protocol),
            Some(Version::latest().protocol)
        );
        assert_eq!(Version::oldest().name(), "1.26.40");
        assert!(Version::of(2169).is_some_and(Version::before_26_50));
        assert!(Version::of(2000).is_none());
    }
}
