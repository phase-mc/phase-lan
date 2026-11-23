//! Phase palette: twelve brand colors for nicknames and prefixes in the game.
//!
//! There is deliberately no arbitrary color (RGB wheel): all twelve read well in `Tab` and in
//! chat on a dark background and differ from each other.
//!
//! **What goes where.** A **nickname** color in Minecraft is a team color (`/team … color`), and
//! it can only be one of the sixteen named game colors. So every
//! Phase color has a nearest game color ([`PhaseColor::mc`]): it paints the nickname and the outline of the
//! highlight. A **prefix** is plain text, so an exact color is available
//! ([`PhaseColor::hex`], since 1.16).

use serde::Serialize;

/// Phase palette color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PhaseColor {
    /// Identifier: in settings, in signed permissions and in the protocol.
    pub id: &'static str,
    /// Name for the interface.
    pub title: &'static str,
    /// Exact brand color: prefix in the game and the swatch in the window.
    pub hex: &'static str,
    /// Named game color: nickname in `Tab`, above the head and the highlight outline.
    pub mc: &'static str,
    /// How the game draws [`PhaseColor::mc`], for the preview in the window.
    pub mc_hex: &'static str,
}

const fn color(
    id: &'static str,
    title: &'static str,
    hex: &'static str,
    mc: &'static str,
    mc_hex: &'static str,
) -> PhaseColor {
    PhaseColor {
        id,
        title,
        hex,
        mc,
        mc_hex,
    }
}

/// The twelve Phase colors in display order.
pub const PALETTE: [PhaseColor; 12] = [
    color("ice_sapphire", "Ice Sapphire", "#38BDF8", "blue", "#5555FF"),
    color("cyber_cyan", "Cyber Cyan", "#22D3EE", "aqua", "#55FFFF"),
    color("crisp_white", "Crisp White", "#F8FAFC", "white", "#FFFFFF"),
    color(
        "emerald_mint",
        "Emerald Mint",
        "#34D399",
        "green",
        "#55FF55",
    ),
    color("amber_gold", "Amber Gold", "#FBBF24", "gold", "#FFAA00"),
    color("coral_flame", "Coral Flame", "#FB7185", "red", "#FF5555"),
    color(
        "royal_violet",
        "Royal Violet",
        "#8B5CF6",
        "dark_purple",
        "#AA00AA",
    ),
    color(
        "rose_quartz",
        "Rose Quartz",
        "#F9A8D4",
        "light_purple",
        "#FF55FF",
    ),
    color("frost_silver", "Frost Silver", "#CBD5E1", "gray", "#AAAAAA"),
    color(
        "deep_ocean",
        "Deep Ocean",
        "#0E7490",
        "dark_aqua",
        "#00AAAA",
    ),
    color(
        "crimson_pulse",
        "Crimson Pulse",
        "#DC2626",
        "dark_red",
        "#AA0000",
    ),
    color(
        "obsidian_slate",
        "Obsidian Slate",
        "#64748B",
        "dark_gray",
        "#555555",
    ),
];

/// Prefix and header color when no custom color is chosen: Phase sapphire.
pub const DEFAULT_ACCENT: &str = "#38BDF8";

/// Palette color by identifier. An unknown identifier (including one from someone else's
/// permissions or an old settings file) gives `None`: only these twelve get into the game.
pub fn find(id: &str) -> Option<&'static PhaseColor> {
    PALETTE.iter().find(|color| color.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn twelve_distinct_colors_with_valid_game_names() {
        const GAME_COLORS: [&str; 16] = [
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
        assert_eq!(PALETTE.len(), 12);
        let unique = |pick: fn(&PhaseColor) -> &'static str| {
            PALETTE.iter().map(pick).collect::<HashSet<_>>().len()
        };
        assert_eq!(unique(|c| c.id), 12);
        assert_eq!(unique(|c| c.title), 12);
        assert_eq!(unique(|c| c.hex), 12);
        // In the game the colors do not merge either: each has its own game color.
        assert_eq!(unique(|c| c.mc), 12);
        for color in &PALETTE {
            assert!(GAME_COLORS.contains(&color.mc), "{}", color.mc);
            for hex in [color.hex, color.mc_hex] {
                assert!(
                    hex.len() == 7
                        && hex.starts_with('#')
                        && hex[1..].bytes().all(|b| b.is_ascii_hexdigit()),
                    "{hex}"
                );
            }
            assert!(
                color
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_')
            );
        }
    }

    #[test]
    fn lookup_accepts_only_the_palette() {
        assert_eq!(find("ice_sapphire").map(|c| c.mc), Some("blue"));
        assert_eq!(
            find("obsidian_slate").map(|c| c.title),
            Some("Obsidian Slate")
        );
        for bad in ["", "Ice Sapphire", "#38BDF8", "aqua", "ice_sapphire\"}"] {
            assert_eq!(find(bad), None, "{bad:?}");
        }
    }
}
