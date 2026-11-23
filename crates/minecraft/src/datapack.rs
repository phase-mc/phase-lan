//! Phase's hidden vanilla datapack: room styling right in the host's world.
//!
//! The game stream between the guest's game and the host's server is encrypted, and packets
//! cannot be inserted into it. So everything players see in the game is done by ordinary game commands
//! (`/bossbar`, `/team`, `/effect`) from a datapack in the host's world folder. It works with any
//! client and launcher, with mods or without, and guests need to install nothing.
//!
//! This module is a **pure generator**: room settings → a set of files. It does not touch
//! the disk (writing is in `phase_core::look`).
//!
//! ```text
//! <world>/datapacks/phase/
//!   pack.mcmeta
//!   phase-state.json                      what Phase created in the world (the game does not read this file)
//!   data/minecraft/tags/function/…        load.json (since 1.21)
//!   data/minecraft/tags/functions/…       the same for versions before 1.21
//!   data/phase/function/…  and  functions/… identical sets of functions:
//!     load, watch, changed, manual, hide         the permanent skeleton
//!     apply, glow                                created from the room settings
//! ```
//!
//! **How it lives.** There is no `tick` tag: the game does not call the datapack 20 times a second. Instead
//! `load` starts a light `watch` loop once a second (`schedule … 20t`): it only
//! counts the players in the world and looks at the `phase.sync` trigger. At rest that is
//! three `execute` commands a second; `/team`, `/bossbar` and `/effect` are not run.
//!
//! **Event.** The player count changed: `changed` only calls `apply` (header,
//! `/team`, `/bossbar` and an indefinite `glowing` for whoever joined): this is instant. There is deliberately
//! no `reload` here: it reloads the recipes, loot and tags of all mods and on modded
//! builds freezes the game for seconds every time a friend joins.
//!
//! **Live changes.** The game reads datapack files only on `load` and `reload`.
//! If the host changes a color, prefix or highlight, the files are rewritten at once, and the game picks
//! them up the next time the world is opened or on the `/trigger phase.sync` command (no cheats are
//! needed for it): `manual` is the only place where `reload` is called.
//!
//! **Highlight.** `apply` gives `glowing` indefinitely to those who have it enabled, and removes it
//! (`effect clear`) from the other players in the room.
//!
//! **Cleanup.** When the room is closed, `apply` becomes a cleanup: it removes the bossbar, the `ph.*`
//! teams and the highlight, and cancels the scheduled `hide`. What stays is a dormant skeleton and two
//! service counters (`phase.meta`, `phase.sync`): the datapack wakes up in the next room
//! without reopening the world. `tick` files from past versions are deleted on
//! write ([`STALE_FILES`]).
//!
//! **Text in commands.** Components are written with quoted keys
//! (`{"text":"[VIP] ","color":"#38BDF8"}`): such a form is both JSON (before 1.21.5) and
//! SNBT (1.21.5+). Nicknames, prefixes and the title are validated: quotes, backslash,
//! `§` and control characters never reach the files.

use std::collections::BTreeSet;

use phase_lang::Lang;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    handshake::is_valid_name,
    palette::{DEFAULT_ACCENT, PhaseColor},
};

/// The datapack folder in `<world>/datapacks`.
pub const PACK_FOLDER: &str = "phase";
/// State file in the datapack folder.
pub const STATE_FILE: &str = "phase-state.json";
/// Version of the datapack file layout.
pub const SCHEMA: u32 = 1;
/// Files of past datapack versions (the `tick` tag and function, the deferred `sync` function): they
/// must be deleted on write. A tag without a function would make the game look 20 times a second for
/// a nonexistent `phase:tick`.
pub const STALE_FILES: &[&str] = &[
    "data/minecraft/tags/function/tick.json",
    "data/minecraft/tags/functions/tick.json",
    "data/phase/function/tick.mcfunction",
    "data/phase/functions/tick.mcfunction",
    "data/phase/function/sync.mcfunction",
    "data/phase/functions/sync.mcfunction",
];

/// Default header title.
pub const DEFAULT_TITLE: &str = "PHASE";
/// Maximum length of a custom header title.
pub const TITLE_MAX_CHARS: usize = 32;
/// Maximum prefix length (without brackets).
pub const PREFIX_MAX_CHARS: usize = 8;
/// Maximum room code length in the header.
pub const CODE_MAX_CHARS: usize = 16;
/// How long the header stays on screen after a player joins or leaves: 10 seconds.
pub const WELCOME_TICKS: u32 = 200;
/// How many team names the state remembers (Phase never creates more).
const TEAMS_KEEP: usize = 96;

/// The oldest supported game version: 1.16 (HEX prefix color, command storage).
pub const MIN_PROTOCOL: i32 = 735;
/// Snapshot flag in the protocol number.
const SNAPSHOT_BIT: i32 = 0x4000_0000;

/// Game protocol number → datapack format. A row applies until the next one.
const PACK_FORMATS: &[(i32, u32)] = &[
    (735, 5),  // 1.16–1.16.1
    (751, 6),  // 1.16.2–1.16.5
    (755, 7),  // 1.17–1.17.1
    (757, 8),  // 1.18–1.18.1
    (758, 9),  // 1.18.2
    (759, 10), // 1.19–1.19.3
    (762, 12), // 1.19.4
    (763, 15), // 1.20–1.20.1
    (764, 18), // 1.20.2
    (765, 26), // 1.20.3–1.20.4
    (766, 41), // 1.20.5–1.20.6
    (767, 48), // 1.21–1.21.1
    (768, 57), // 1.21.2–1.21.3
    (769, 61), // 1.21.4
    (770, 71), // 1.21.5
    (771, 80), // 1.21.6
    (772, 81), // 1.21.7–1.21.8
    (773, 88), // 1.21.9–1.21.10
    (774, 94), // 1.21.11
];
/// Bounds of "suitable for versions" in `pack.mcmeta`: one datapack for all versions, because
/// it contains both folder layouts (`function` and `functions`).
const FORMAT_MIN: u32 = 5;
const FORMAT_MAX: u32 = 999;

/// A game version with this protocol number is supported (`None` means the version is unknown:
/// we try, the datapack suits newer versions).
pub fn supports(protocol: Option<i32>) -> bool {
    protocol.is_none_or(|p| p >= MIN_PROTOCOL)
}

/// Datapack format for the host's game version.
pub fn pack_format(protocol: Option<i32>) -> u32 {
    let latest = PACK_FORMATS[PACK_FORMATS.len() - 1].1;
    match protocol {
        Some(p) if p & SNAPSHOT_BIT == 0 => PACK_FORMATS
            .iter()
            .rev()
            .find(|(from, _)| p >= *from)
            .map_or(FORMAT_MIN, |(_, format)| *format),
        _ => latest,
    }
}

// ------------------------------------------------------------------ text validation

/// Why the text is not suitable for the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextError {
    Empty,
    TooLong { max: usize, len: usize },
    BadChar(char),
}

impl std::fmt::Display for TextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str(phase_lang::tr!("пусто", "empty")),
            Self::TooLong { max, len } => f.write_str(&phase_lang::tr!(
                format!("не длиннее {max} знаков (сейчас {len})"),
                format!("at most {max} characters (now {len})")
            )),
            Self::BadChar(c) if c.is_whitespace() => {
                f.write_str(phase_lang::tr!("без пробелов", "no spaces"))
            }
            Self::BadChar(c) => f.write_str(&phase_lang::tr!(
                format!("знак «{}» недопустим", c.escape_default()),
                format!("the character “{}” is not allowed", c.escape_default())
            )),
        }
    }
}

impl std::error::Error for TextError {}

/// A character must not be let into game commands: control characters (a line break would start a new
/// command), a quote and backslash (string boundaries), `§` (the game's formatting codes)
/// and invisible characters that change how text looks.
fn is_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '"' | '\\'
                | '§'
                | '\u{00AD}'
                | '\u{061C}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{FEFF}'
        )
}

/// Prefix before the nickname (without brackets): 1 to [`PREFIX_MAX_CHARS`] characters: letters and
/// digits of any alphabet and `- _ . ! + * # &`. Spaces at the edges are removed.
pub fn validate_prefix(raw: &str) -> Result<String, TextError> {
    let prefix = raw.trim();
    let len = prefix.chars().count();
    if len == 0 {
        return Err(TextError::Empty);
    }
    if len > PREFIX_MAX_CHARS {
        return Err(TextError::TooLong {
            max: PREFIX_MAX_CHARS,
            len,
        });
    }
    match prefix.chars().find(|&c| {
        is_unsafe(c)
            || !(c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '!' | '+' | '*' | '#' | '&'))
    }) {
        Some(bad) => Err(TextError::BadChar(bad)),
        None => Ok(prefix.to_string()),
    }
}

/// Header title: 1 to [`TITLE_MAX_CHARS`] characters, consecutive spaces become one.
pub fn validate_title(raw: &str) -> Result<String, TextError> {
    let title = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let len = title.chars().count();
    if len == 0 {
        return Err(TextError::Empty);
    }
    if len > TITLE_MAX_CHARS {
        return Err(TextError::TooLong {
            max: TITLE_MAX_CHARS,
            len,
        });
    }
    match title.chars().find(|&c| is_unsafe(c)) {
        Some(bad) => Err(TextError::BadChar(bad)),
        None => Ok(title),
    }
}

/// Room code for the header: Latin letters, digits and hyphen, up to [`CODE_MAX_CHARS`] characters.
pub fn is_valid_code(code: &str) -> bool {
    (1..=CODE_MAX_CHARS).contains(&code.len())
        && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// A quoted string for a text component. The checks above do not let quotes and
/// backslash through; escaping is the second line of defense in case the check is relaxed.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars().filter(|c| !c.is_control()) {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

// ------------------------------------------------------------------ room

/// Who the player is in the room: the place in `Tab` depends on it (the host comes first). `Pro` stays
/// in the `room.json` format (the mod understands it), but LAN is free and Phase does not write it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Rank {
    Host,
    Pro,
    Guest,
}

impl Rank {
    /// Letter in the team name: `Tab` sorts players by team name.
    fn letter(self) -> char {
        match self {
            Self::Host => 'a',
            Self::Pro => 'b',
            Self::Guest => 'c',
        }
    }
}

/// A player and their styling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackPlayer {
    /// The real in-game nickname. A nickname not matching `[A-Za-z0-9_]{1,16}` does not get into commands.
    pub name: String,
    pub rank: Rank,
    /// Text in brackets before the nickname (`HOST` or a custom prefix).
    pub label: Option<String>,
    /// Color of the nickname and prefix.
    pub color: Option<&'static PhaseColor>,
    /// Outline visible through walls.
    pub glow: bool,
}

/// The room as the game shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackRoom {
    /// Header title (`PHASE` or the host's own title).
    pub title: String,
    /// Room code.
    pub code: String,
    /// Seats in the room including the host: the header bar is filled as "online out of seats".
    pub max_players: u32,
    /// Show the "Powered by Phase" footer (mod only; the host can remove it).
    pub footer: bool,
    /// The host's in-game nickname: the `/trigger phase.sync` command is open to them.
    pub host_name: Option<String>,
    pub players: Vec<PackPlayer>,
}

/// What the datapack must do.
#[derive(Debug, Clone, Copy)]
pub enum PackMode<'a> {
    /// The room is open.
    Active(&'a PackRoom),
    /// There is no room: remove everything of its own from the world and wait.
    Sleeping,
}

/// A datapack file: path inside the datapack folder (with `/`) and contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackFile {
    pub path: String,
    pub content: String,
}

/// A finished datapack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pack {
    pub files: Vec<PackFile>,
    /// All teams Phase has ever created in this world (for cleanup).
    pub teams: Vec<String>,
}

impl Pack {
    /// File contents by path inside the datapack.
    pub fn file(&self, path: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|file| file.path == path)
            .map(|file| file.content.as_str())
    }
}

/// What Phase created in the world. Lives in the datapack folder; the game does not read this file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PackState {
    pub schema: u32,
    /// The room is open (`false` means the datapack sleeps).
    pub active: bool,
    /// Names of the `ph.*` teams created in this world.
    pub teams: Vec<String>,
    /// When the datapack first appeared in the world, Unix seconds. A world opened earlier has not
    /// loaded it yet.
    pub installed_unix: u64,
    pub updated_unix: u64,
}

/// A player ready to be written into commands.
struct Styled<'a> {
    player: &'a PackPlayer,
    /// The player's own team (`None` means no prefix and no color: no team needed).
    team: Option<String>,
    label: Option<String>,
}

/// Selects players with valid nicknames (without repeats) and hands out team names.
fn styled(room: &PackRoom) -> Vec<Styled<'_>> {
    let mut players: Vec<&PackPlayer> = Vec::new();
    for player in &room.players {
        if is_valid_name(&player.name)
            && !players
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(&player.name))
        {
            players.push(player);
        }
    }
    players.sort_by_key(|p| (p.rank, p.name.to_ascii_lowercase()));
    let mut next_team = 0usize;
    players
        .into_iter()
        .map(|player| {
            let label = player
                .label
                .as_deref()
                .and_then(|label| validate_prefix(label).ok());
            // The team number is two digits: the hundredth styled player would be left without a team,
            // but there are never that many players in a room.
            let styled = (label.is_some() || player.color.is_some()) && next_team < 100;
            let team = styled.then(|| {
                let name = format!("ph.{}{next_team:02}", player.rank.letter());
                next_team += 1;
                name
            });
            Styled {
                player,
                team,
                label,
            }
        })
        .collect()
}

/// The first line of every datapack file: the program writes it, edits will be lost.
fn header(lang: Lang) -> &'static str {
    match lang {
        Lang::Ru => "# Phase: файл создаёт программа, правки будут потеряны.\n",
        _ => "# Phase: this file is generated by the app, edits will be lost.\n",
    }
}

/// The word after the player count in the header: `PHASE • K7F-2QX • 3 online` (the word follows the room language).
fn online_word(lang: Lang) -> &'static str {
    match lang {
        Lang::Ru => " онлайн",
        _ => " online",
    }
}

/// Phase Companion pulse: the `#companion` score in the `phase.meta` objective. Once a second the mod
/// sets it to [`COMPANION_PULSE`], and the `watch` loop decrements it by 1 once a second. While
/// the score is above zero, the mod is alive and styles the world itself, and the datapack's `apply` is not called:
/// otherwise the datapack and the mod would fight over the same `ph.*` teams and bossbar. If the mod is gone (removed,
/// game without it), the score reaches zero in a few seconds and the datapack takes
/// the styling over itself. The score is live rather than a flag in storage: a forgotten flag would disable
/// the datapack forever.
pub const COMPANION_SCORE: &str = "#companion";
/// The pulse value the mod sets (seconds before the datapack takes over).
pub const COMPANION_PULSE: u32 = 5;

/// `apply` only if Phase Companion is silent.
fn apply_unless_companion() -> String {
    format!(
        "execute unless score {COMPANION_SCORE} phase.meta matches 1.. run function phase:apply\n"
    )
}

/// A comment in a datapack file in the room's language (`# …\n`).
fn note(lang: Lang, ru: &str, en: &str) -> String {
    let text = match lang {
        Lang::Ru => ru,
        _ => en,
    };
    format!("# {text}\n")
}

fn load_function(lang: Lang) -> String {
    format!(
        "{header}\
         scoreboard objectives add phase.meta dummy\n\
         scoreboard objectives add phase.sync trigger\n\
         {note}\
         execute store result score #last phase.meta if entity @a\n\
         {apply}\
         schedule function phase:watch 20t\n",
        header = header(lang),
        note = note(
            lang,
            "Свежие файлы уже прочитаны: число игроков на сейчас — не «изменение».",
            "Fresh files are already read: the current player count is not a \"change\"."
        ),
        apply = apply_unless_companion(),
    )
}

/// A loop once a second: counts players, looks at the trigger and the mod's pulse. Applies
/// nothing itself, except when the mod has just gone silent.
fn watch_function(lang: Lang) -> String {
    format!(
        "{header}\
         execute store result score #now phase.meta if entity @a\n\
         execute unless score #now phase.meta = #last phase.meta run function phase:changed\n\
         execute as @a[scores={{phase.sync=1..}}] run function phase:manual\n\
         {note}\
         execute if score {COMPANION_SCORE} phase.meta matches 1 run function phase:apply\n\
         execute if score {COMPANION_SCORE} phase.meta matches 1.. run scoreboard players remove {COMPANION_SCORE} phase.meta 1\n\
         schedule function phase:watch 20t\n",
        header = header(lang),
        note = note(
            lang,
            "Phase Companion замолчал: оформление снова делает датапак.",
            "Phase Companion went quiet: the datapack styles the room again."
        ),
    )
}

fn changed_function(lang: Lang) -> String {
    format!(
        "{header}\
         {note}\
         scoreboard players operation #last phase.meta = #now phase.meta\n\
         {apply}",
        header = header(lang),
        note = note(
            lang,
            "Кто-то вошёл или вышел: только применить оформление, мгновенно. Перечитывать\n# файлы здесь нельзя: на модовых сборках это фриз на секунды.",
            "Someone joined or left: just apply the style, instantly. Reloading files\n# here is not allowed: on modded setups it freezes the game for seconds."
        ),
        apply = apply_unless_companion(),
    )
}

fn manual_function(lang: Lang) -> String {
    format!(
        "{header}\
         {note}\
         scoreboard players set @s phase.sync 0\n\
         scoreboard players enable @s phase.sync\n\
         reload\n",
        header = header(lang),
        note = note(
            lang,
            "/trigger phase.sync — перечитать файлы с диска (`reload`) и применить сейчас.",
            "/trigger phase.sync — re-read the files from disk (`reload`) and apply now."
        ),
    )
}

/// Header: `PHASE • K7F-2QX • 3 online`. The game fills in the player count.
fn header_component(room: &PackRoom, lang: Lang) -> String {
    let title = validate_title(&room.title).unwrap_or_else(|_| DEFAULT_TITLE.to_string());
    let code = if is_valid_code(&room.code) {
        format!(" • {} • ", room.code)
    } else {
        " • ".to_string()
    };
    format!(
        "[{{\"text\":{title},\"color\":\"{DEFAULT_ACCENT}\",\"bold\":true}},\
         {{\"text\":{code},\"color\":\"white\",\"bold\":false}},\
         {{\"score\":{{\"name\":\"#now\",\"objective\":\"phase.meta\"}},\"color\":\"white\",\"bold\":false}},\
         {{\"text\":{online},\"color\":\"white\",\"bold\":false}}]",
        title = quote(&title),
        code = quote(&code),
        online = quote(online_word(lang)),
    )
}

fn remove_teams(out: &mut String, teams: &BTreeSet<String>) {
    for team in teams {
        out.push_str(&format!("team remove {team}\n"));
    }
}

fn apply_active(
    room: &PackRoom,
    players: &[Styled<'_>],
    teams: &BTreeSet<String>,
    lang: Lang,
) -> String {
    let mut out = String::from(header(lang));
    // Teams are created anew on every apply: so neither old
    // prefixes nor players whose styling was removed are left over.
    remove_teams(&mut out, teams);

    out.push_str("bossbar add phase:room {\"text\":\"PHASE\"}\n");
    out.push_str("bossbar set phase:room color blue\n");
    out.push_str(&format!(
        "bossbar set phase:room max {}\n",
        room.max_players.max(1)
    ));
    out.push_str("execute store result score #now phase.meta if entity @a\n");
    out.push_str("execute store result bossbar phase:room value if entity @a\n");
    out.push_str(&format!(
        "bossbar set phase:room name {}\n",
        header_component(room, lang)
    ));
    out.push_str("bossbar set phase:room players @a\n");
    out.push_str("bossbar set phase:room visible true\n");
    out.push_str(&format!("schedule function phase:hide {WELCOME_TICKS}t\n"));

    for styled in players {
        let player = styled.player;
        let Some(team) = &styled.team else {
            continue;
        };
        out.push_str(&format!("team add {team}\n"));
        if let Some(color) = player.color {
            out.push_str(&format!("team modify {team} color {}\n", color.mc));
        }
        if let Some(label) = &styled.label {
            let hex = player.color.map_or(DEFAULT_ACCENT, |color| color.hex);
            out.push_str(&format!(
                "team modify {team} prefix {{\"text\":{},\"color\":\"{hex}\"}}\n",
                quote(&format!("[{label}] "))
            ));
        }
        // Only free players: Phase does not touch a map's or another datapack's team.
        out.push_str(&format!(
            "execute as @a[name={},team=] run team join {team} @s\n",
            player.name
        ));
    }
    if let Some(host) = room.host_name.as_deref().filter(|name| is_valid_name(name)) {
        out.push_str(&format!(
            "scoreboard players enable @a[name={host}] phase.sync\n"
        ));
    }
    out.push_str("function phase:glow\n");
    out
}

fn glow_active(players: &[Styled<'_>], lang: Lang) -> String {
    let mut out = String::from(header(lang));
    out.push_str(&note(
        lang,
        "Подсветка применяется один раз, при load / reload: бессрочно, без частиц.",
        "The glow is applied once, on load / reload: indefinitely, without particles.",
    ));
    for styled in players {
        let name = &styled.player.name;
        if styled.player.glow {
            out.push_str(&format!(
                "effect give @a[name={name}] minecraft:glowing 999999 0 true\n"
            ));
        } else {
            out.push_str(&format!("effect clear @a[name={name}] minecraft:glowing\n"));
        }
    }
    out
}

fn apply_sleeping(teams: &BTreeSet<String>, lang: Lang) -> String {
    let mut out = String::from(header(lang));
    out.push_str(&note(
        lang,
        "Комната закрыта: убрать из мира всё, что создал Phase.",
        "The room is closed: remove everything Phase created in the world.",
    ));
    out.push_str("bossbar remove phase:room\n");
    remove_teams(&mut out, teams);
    out.push_str("schedule clear phase:hide\n");
    out.push_str("effect clear @a minecraft:glowing\n");
    out.push_str("data modify storage phase:state clean set value 1b\n");
    out
}

fn mcmeta(protocol: Option<i32>, lang: Lang) -> String {
    let meta = json!({
        "pack": {
            "description": match lang {
                Lang::Ru => "Phase: оформление комнаты",
                _ => "Phase: room style",
            },
            "pack_format": pack_format(protocol).clamp(FORMAT_MIN, FORMAT_MAX),
            "supported_formats": { "min_inclusive": FORMAT_MIN, "max_inclusive": FORMAT_MAX },
            "min_format": FORMAT_MIN,
            "max_format": FORMAT_MAX,
        }
    });
    format!("{meta:#}\n")
}

/// Builds the datapack. `protocol` is the host's game protocol number (from the status query),
/// `known_teams` are teams created in this world earlier ([`PackState::teams`]), `lang` is
/// the room language: the in-game header (the word "online" in that language) and the notes in the files use it.
pub fn generate(
    mode: PackMode<'_>,
    protocol: Option<i32>,
    known_teams: &[String],
    lang: Lang,
) -> Pack {
    let players = match mode {
        PackMode::Active(room) => styled(room),
        PackMode::Sleeping => Vec::new(),
    };
    let mut teams: BTreeSet<String> = known_teams
        .iter()
        .filter(|team| is_team_name(team))
        .cloned()
        .collect();
    teams.extend(players.iter().filter_map(|styled| styled.team.clone()));

    let (apply, glow) = match mode {
        PackMode::Active(room) => (
            apply_active(room, &players, &teams, lang),
            glow_active(&players, lang),
        ),
        PackMode::Sleeping => (
            apply_sleeping(&teams, lang),
            format!(
                "{}{}",
                header(lang),
                note(lang, "Комната закрыта.", "The room is closed.")
            ),
        ),
    };
    let functions = [
        ("load", load_function(lang)),
        ("watch", watch_function(lang)),
        ("changed", changed_function(lang)),
        ("manual", manual_function(lang)),
        (
            "hide",
            format!("{}bossbar set phase:room visible false\n", header(lang)),
        ),
        ("apply", apply),
        ("glow", glow),
    ];

    let mut files = vec![PackFile {
        path: "pack.mcmeta".into(),
        content: mcmeta(protocol, lang),
    }];
    // Two folder layouts: `function` is read by versions since 1.21, `functions` by older ones.
    for dir in ["function", "functions"] {
        files.push(PackFile {
            path: format!("data/minecraft/tags/{dir}/load.json"),
            content: "{\"values\":[\"phase:load\"]}\n".to_string(),
        });
        for (name, content) in &functions {
            files.push(PackFile {
                path: format!("data/phase/{dir}/{name}.mcfunction"),
                content: content.clone(),
            });
        }
    }
    Pack {
        files,
        teams: teams.into_iter().take(TEAMS_KEEP).collect(),
    }
}

/// Name of a team created by Phase: `ph.` + seat letter + two digits.
fn is_team_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 6
        && name.starts_with("ph.")
        && matches!(bytes[3], b'a'..=b'c')
        && bytes[4].is_ascii_digit()
        && bytes[5].is_ascii_digit()
}

// ------------------------------------------------------------------ Phase Companion

/// Phase folder in the world for the Phase Companion mod: `<world>/phase`.
pub const COMPANION_FOLDER: &str = "phase";
/// Room for the mod: Phase writes the file together with the datapack, and the mod rereads it when
/// the modification time changes.
pub const COMPANION_ROOM_FILE: &str = "room.json";
/// The mod confirms it works in this world by creating this file when the world is opened.
pub const COMPANION_ALIVE_FILE: &str = "companion.ok";

/// Contents of `room.json` for Phase Companion (a server mod for Fabric/Forge/NeoForge
/// on the host). The same data as in the datapack, but the mod applies it at once, without
/// `reload`, and draws a real `Tab` header. `None` means no styling (the room is closed
/// or styling is off).
///
/// `account_check`: whether to verify players' accounts on join (an optional field of
/// format 1: a mod that does not know it leaves the check as in the game). It is written even
/// without styling: joining without verification is possible with styling off too.
///
/// `lang` is the room language (`ru` / `en`, an optional field of format 1): the mod writes the `Tab`
/// footer and the localized word "online" in it. A file without the field is read by the mod as Russian.
///
/// ```json
/// { "schema": 1, "active": true, "account_check": true, "lang": "en", "title": "PHASE",
///   "code": "K7F-2QX", "max_players": 5,
///   "players": [ { "name": "Steve", "rank": "host", "label": "HOST",
///                  "color": "ice_sapphire", "name_color": "blue",
///                  "prefix_color": "#38BDF8", "glow": false } ] }
/// ```
pub fn companion_room(room: Option<&PackRoom>, account_check: bool, lang: Lang) -> String {
    let value = match room {
        None => json!({
            "schema": SCHEMA,
            "active": false,
            "account_check": account_check,
            "lang": lang.code(),
            "players": [],
        }),
        Some(room) => {
            let players: Vec<_> = styled(room)
                .iter()
                .map(|styled| {
                    let player = styled.player;
                    json!({
                        "name": player.name,
                        "rank": player.rank,
                        "label": styled.label,
                        "color": player.color.map(|c| c.id),
                        "name_color": player.color.map(|c| c.mc),
                        "prefix_color": player.color.map_or(DEFAULT_ACCENT, |c| c.hex),
                        "glow": player.glow,
                    })
                })
                .collect();
            json!({
                "schema": SCHEMA,
                "active": true,
                "account_check": account_check,
                "lang": lang.code(),
                "title": validate_title(&room.title).unwrap_or_else(|_| DEFAULT_TITLE.into()),
                "code": Some(&room.code).filter(|code| is_valid_code(code)),
                "max_players": room.max_players,
                "footer": room.footer,
                "players": players,
            })
        }
    };
    format!("{value:#}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette;

    fn player(name: &str, rank: Rank) -> PackPlayer {
        PackPlayer {
            name: name.into(),
            rank,
            label: None,
            color: None,
            glow: false,
        }
    }

    fn free_room() -> PackRoom {
        PackRoom {
            title: DEFAULT_TITLE.into(),
            code: "K7F-2QX".into(),
            max_players: 5,
            footer: true,
            host_name: Some("Steve".into()),
            players: vec![
                PackPlayer {
                    label: Some("HOST".into()),
                    ..player("Steve", Rank::Host)
                },
                player("Alex_01", Rank::Guest),
            ],
        }
    }

    fn apply_of(pack: &Pack) -> &str {
        pack.file("data/phase/function/apply.mcfunction").unwrap()
    }

    /// Every function line is a comment or a command from the known set: nothing
    /// that the game would read as a foreign command gets into the files.
    fn assert_only_known_commands(text: &str) {
        const COMMANDS: &[&str] = &[
            "#",
            "bossbar ",
            "team ",
            "execute ",
            "schedule ",
            "scoreboard ",
            "function ",
            "effect give @a[name=",
            "effect clear @a",
            "data modify storage phase:state ",
            "reload",
        ];
        for line in text.lines() {
            assert!(
                COMMANDS.iter().any(|start| line.starts_with(start)),
                "unexpected line: {line:?}"
            );
        }
    }

    #[test]
    fn pack_format_reference_versions() {
        // Reference values from the datapack format table.
        for (protocol, format) in [
            (754, 6),  // 1.16.5
            (758, 9),  // 1.18.2
            (762, 12), // 1.19.4
            (763, 15), // 1.20.1
            (765, 26), // 1.20.4
            (767, 48), // 1.21.1
            (769, 61), // 1.21.4
        ] {
            assert_eq!(pack_format(Some(protocol)), format, "protocol {protocol}");
        }
        // The table grows with the versions.
        assert!(
            PACK_FORMATS
                .windows(2)
                .all(|w| w[0].0 < w[1].0 && w[0].1 < w[1].1)
        );
        let latest = PACK_FORMATS[PACK_FORMATS.len() - 1].1;
        assert_eq!(
            pack_format(Some(9000)),
            latest,
            "version newer than the table"
        );
        assert_eq!(pack_format(Some(SNAPSHOT_BIT | 250)), latest, "snapshot");
        assert_eq!(pack_format(None), latest);

        assert!(supports(Some(735)) && supports(Some(767)) && supports(None));
        assert!(!supports(Some(578)), "1.15.2 is not supported");
        assert!(!supports(Some(-1)));
    }

    #[test]
    fn layout_serves_old_and_new_versions() {
        let pack = generate(PackMode::Active(&free_room()), Some(763), &[], Lang::Ru);
        let meta: serde_json::Value =
            serde_json::from_str(pack.file("pack.mcmeta").unwrap()).unwrap();
        assert_eq!(meta["pack"]["pack_format"], 15);
        assert_eq!(meta["pack"]["supported_formats"]["min_inclusive"], 5);
        assert_eq!(meta["pack"]["min_format"], 5);
        assert!(meta["pack"]["max_format"].as_u64().unwrap() >= 94);

        for dir in ["function", "functions"] {
            let json = pack
                .file(&format!("data/minecraft/tags/{dir}/load.json"))
                .unwrap();
            let value: serde_json::Value = serde_json::from_str(json).unwrap();
            assert_eq!(value["values"][0], "phase:load");
            assert!(
                pack.file(&format!("data/minecraft/tags/{dir}/tick.json"))
                    .is_none()
            );
            for name in [
                "load", "watch", "changed", "manual", "hide", "apply", "glow",
            ] {
                let path = format!("data/phase/{dir}/{name}.mcfunction");
                let text = pack.file(&path).unwrap_or_else(|| panic!("missing {path}"));
                assert_only_known_commands(text);
                assert!(text.ends_with('\n') && !text.contains('\r'));
            }
        }
        // Both layouts are identical.
        assert_eq!(
            pack.file("data/phase/function/apply.mcfunction"),
            pack.file("data/phase/functions/apply.mcfunction")
        );
        assert_eq!(pack.files.len(), 1 + 2 * (1 + 7));
        // There is no `tick` tag, and files of past versions are not in the set: they are deleted on write.
        for file in &pack.files {
            assert!(!file.path.contains("tick"), "{}", file.path);
            assert!(!STALE_FILES.contains(&file.path.as_str()), "{}", file.path);
        }
        assert_eq!(STALE_FILES.len(), 6);
        assert!(
            !pack
                .files
                .iter()
                .any(|f| f.path.contains("sync.mcfunction"))
        );

        // The once-a-second loop only counts and checks the trigger: no /team, /bossbar,
        // /effect, and `apply` is called from `changed`, `load` and once when the mod went silent.
        let watch = pack.file("data/phase/function/watch.mcfunction").unwrap();
        assert!(watch.contains("execute store result score #now phase.meta if entity @a\n"));
        assert!(watch.contains(
            "execute unless score #now phase.meta = #last phase.meta run function phase:changed\n"
        ));
        assert!(watch.contains("@a[scores={phase.sync=1..}] run function phase:manual\n"));
        assert!(watch.ends_with("schedule function phase:watch 20t\n"));
        for banned in ["team", "bossbar", "effect", "reload"] {
            assert!(!watch.contains(banned), "{banned} in watch");
        }
        assert_eq!(
            watch.matches("phase:apply").count(),
            1,
            "apply from watch only when the mod went silent"
        );
        // The rest of the skeleton runs no loops itself.
        for name in ["changed", "manual", "hide", "glow"] {
            let text = pack
                .file(&format!("data/phase/function/{name}.mcfunction"))
                .unwrap();
            assert!(!text.contains("phase:watch"), "{name}");
        }
    }

    #[test]
    fn free_room_gets_welcome_bar_and_host_tag() {
        let pack = generate(PackMode::Active(&free_room()), Some(767), &[], Lang::Ru);
        let apply = apply_of(&pack);

        // Header: title, room code and a live online counter, for 10 seconds.
        assert!(apply.contains("bossbar add phase:room "));
        assert!(apply.contains("bossbar set phase:room max 5\n"));
        assert!(apply.contains(
            "bossbar set phase:room name [{\"text\":\"PHASE\",\"color\":\"#38BDF8\",\"bold\":true},\
             {\"text\":\" • K7F-2QX • \",\"color\":\"white\",\"bold\":false},\
             {\"score\":{\"name\":\"#now\",\"objective\":\"phase.meta\"},\"color\":\"white\",\"bold\":false},\
             {\"text\":\" онлайн\",\"color\":\"white\",\"bold\":false}]\n"
        ));
        assert!(apply.contains("bossbar set phase:room players @a\n"));
        assert!(apply.contains("schedule function phase:hide 200t\n"));

        // The host comes first in Tab, with the [HOST] label; a Free guest has no styling.
        assert!(apply.contains("team add ph.a00\n"));
        assert!(
            apply.contains(
                "team modify ph.a00 prefix {\"text\":\"[HOST] \",\"color\":\"#38BDF8\"}\n"
            )
        );
        assert!(apply.contains("execute as @a[name=Steve,team=] run team join ph.a00 @s\n"));
        assert!(!apply.contains("Alex_01"));
        assert!(
            !apply.contains("team modify ph.a00 color"),
            "the host set no nickname color"
        );
        assert!(apply.contains("scoreboard players enable @a[name=Steve] phase.sync\n"));
        assert!(apply.ends_with("function phase:glow\n"));
        assert_eq!(pack.teams, ["ph.a00"]);

        // Highlight is not enabled for anyone: it is removed from the room's players.
        let glow = pack.file("data/phase/function/glow.mcfunction").unwrap();
        assert!(glow.contains("effect clear @a[name=Steve] minecraft:glowing\n"));
        assert!(!glow.contains("effect give"));

        // A player joining or leaving: only `apply`, no `reload` and no deferred calls.
        // `load` sets the counter to the current player count so it is not mistaken for a join.
        let changed = pack.file("data/phase/function/changed.mcfunction").unwrap();
        assert!(changed.ends_with("function phase:apply\n"));
        assert!(
            changed.contains("scoreboard players operation #last phase.meta = #now phase.meta\n")
        );
        for banned in ["reload", "schedule", "phase:sync"] {
            assert!(!changed.contains(banned), "{banned} in changed");
        }
        let load = pack.file("data/phase/function/load.mcfunction").unwrap();
        assert!(load.contains("execute store result score #last phase.meta if entity @a\n"));
        assert!(load.contains("function phase:apply\n"));
        assert!(load.ends_with("schedule function phase:watch 20t\n"));
        assert!(!load.contains("reload") && !load.contains("phase:sync"));
        // `reload` only in the manual trigger `/trigger phase.sync`.
        let manual = pack.file("data/phase/function/manual.mcfunction").unwrap();
        assert_eq!(manual.lines().last(), Some("reload"));
        for file in &pack.files {
            if file.path.ends_with(".mcfunction") && !file.path.ends_with("manual.mcfunction") {
                assert!(
                    !file.content.contains("reload\n"),
                    "reload in {}",
                    file.path
                );
            }
        }
    }

    #[test]
    fn pro_room_colors_prefixes_glow_and_custom_title() {
        let ice = palette::find("ice_sapphire");
        let room = PackRoom {
            title: "Vanilla Crew".into(),
            code: "vanilla-crew".into(),
            max_players: 8,
            footer: true,
            host_name: Some("Steve".into()),
            players: vec![
                PackPlayer {
                    label: Some("PRO".into()),
                    color: palette::find("amber_gold"),
                    ..player("Zed", Rank::Pro)
                },
                PackPlayer {
                    label: Some("Билдер".into()),
                    color: palette::find("emerald_mint"),
                    glow: true,
                    ..player("alex", Rank::Guest)
                },
                PackPlayer {
                    label: Some("KING".into()),
                    color: ice,
                    glow: true,
                    ..player("Steve", Rank::Host)
                },
                // Color only, no prefix.
                PackPlayer {
                    color: palette::find("crimson_pulse"),
                    ..player("Bob", Rank::Guest)
                },
            ],
        };
        let pack = generate(PackMode::Active(&room), Some(767), &[], Lang::Ru);
        let apply = apply_of(&pack);

        assert!(apply.contains("{\"text\":\"Vanilla Crew\",\"color\":\"#38BDF8\",\"bold\":true}"));
        assert!(apply.contains(" • vanilla-crew • "));
        assert!(apply.contains("bossbar set phase:room max 8\n"));

        // Tab seats: host, then Pro, then guests alphabetically.
        assert_eq!(pack.teams, ["ph.a00", "ph.b01", "ph.c02", "ph.c03"]);
        // Nickname in the game team color, prefix in the exact palette color.
        assert!(apply.contains("team modify ph.a00 color blue\n"));
        assert!(
            apply.contains(
                "team modify ph.a00 prefix {\"text\":\"[KING] \",\"color\":\"#38BDF8\"}\n"
            )
        );
        assert!(apply.contains("team modify ph.b01 color gold\n"));
        assert!(
            apply.contains(
                "team modify ph.b01 prefix {\"text\":\"[PRO] \",\"color\":\"#FBBF24\"}\n"
            )
        );
        assert!(apply.contains("execute as @a[name=alex,team=] run team join ph.c02 @s\n"));
        assert!(apply.contains(
            "team modify ph.c02 prefix {\"text\":\"[Билдер] \",\"color\":\"#34D399\"}\n"
        ));
        assert!(apply.contains("team modify ph.c03 color dark_red\n"));
        assert!(!apply.contains("team modify ph.c03 prefix"));

        // Highlight: indefinite for those who have it, removed from the others; no loops.
        assert!(apply.ends_with("function phase:glow\n"));
        let glow = pack.file("data/phase/function/glow.mcfunction").unwrap();
        assert!(glow.contains("effect give @a[name=Steve] minecraft:glowing 999999 0 true\n"));
        assert!(glow.contains("effect give @a[name=alex] minecraft:glowing 999999 0 true\n"));
        assert!(!glow.contains("effect give @a[name=Zed]"));
        assert!(!glow.contains("effect give @a[name=Bob]"));
        assert!(glow.contains("effect clear @a[name=Zed] minecraft:glowing\n"));
        assert!(glow.contains("effect clear @a[name=Bob] minecraft:glowing\n"));
        assert!(!glow.contains("schedule"));
    }

    #[test]
    fn closing_the_room_cleans_everything_phase_created() {
        let active = generate(
            PackMode::Active(&free_room()),
            Some(767),
            &["ph.c07".into()],
            Lang::Ru,
        );
        // Teams of past rooms are removed in an open room too.
        assert!(apply_of(&active).starts_with(&format!(
            "{}team remove ph.a00\nteam remove ph.c07\n",
            header(Lang::Ru)
        )));

        let sleeping = generate(PackMode::Sleeping, Some(767), &active.teams, Lang::Ru);
        let apply = apply_of(&sleeping);
        assert!(apply.contains("bossbar remove phase:room\n"));
        assert!(apply.contains("team remove ph.a00\n") && apply.contains("team remove ph.c07\n"));
        assert!(apply.contains("effect clear @a minecraft:glowing\n"));
        assert!(apply.contains("schedule clear phase:hide\n"));
        assert!(apply.contains("data modify storage phase:state clean set value 1b\n"));
        assert!(!apply.contains("team add") && !apply.contains("bossbar add"));
        assert!(
            !apply.contains("Steve"),
            "a dormant datapack has no nicknames"
        );
        let glow = sleeping
            .file("data/phase/function/glow.mcfunction")
            .unwrap();
        assert!(!glow.contains("effect") && !glow.contains("schedule"));
        // The team list is not lost: cleanup works after the next room too.
        assert_eq!(sleeping.teams, ["ph.a00", "ph.c07"]);
        // Same skeleton: a dormant datapack wakes up when a guest joins.
        for name in ["load", "watch", "changed", "manual", "hide"] {
            let path = format!("data/phase/function/{name}.mcfunction");
            assert_eq!(active.file(&path), sleeping.file(&path), "{name}");
        }
        // Foreign names from the state file do not get into commands.
        let odd = generate(
            PackMode::Sleeping,
            None,
            &[
                "ph.a00\nop Steve".into(),
                "red".into(),
                "ph.z00".into(),
                "ph.b12".into(),
            ],
            Lang::Ru,
        );
        assert_eq!(odd.teams, ["ph.b12"]);
        assert_only_known_commands(apply_of(&odd));
    }

    #[test]
    fn text_validation() {
        for ok in [
            "VIP",
            "Билдер",
            "a",
            "No.1",
            "x-y_z!",
            "12345678",
            "  PRO  ",
        ] {
            assert_eq!(validate_prefix(ok).as_deref(), Ok(ok.trim()), "{ok}");
        }
        assert_eq!(validate_prefix("  "), Err(TextError::Empty));
        assert_eq!(
            validate_prefix("123456789"),
            Err(TextError::TooLong { max: 8, len: 9 })
        );
        for (bad, c) in [
            ("a\"b", '"'),
            ("a\\b", '\\'),
            ("a b", ' '),
            ("§cRED", '§'),
            ("a\nb", '\n'),
            ("[VIP]", '['),
            ("a{b", '{'),
            ("a@b", '@'),
            ("a\u{202e}b", '\u{202e}'),
        ] {
            assert_eq!(validate_prefix(bad), Err(TextError::BadChar(c)), "{bad:?}");
        }

        assert_eq!(
            validate_title("  Vanilla   Crew ").as_deref(),
            Ok("Vanilla Crew")
        );
        assert_eq!(
            validate_title("Мир №1 [night] • go!").as_deref(),
            Ok("Мир №1 [night] • go!")
        );
        assert_eq!(validate_title(" \t "), Err(TextError::Empty));
        assert!(matches!(
            validate_title(&"x".repeat(33)),
            Err(TextError::TooLong { max: 32, len: 33 })
        ));
        for bad in ["say \"hi\"", "back\\slash", "§4red", "zero\u{200b}width"] {
            assert!(
                matches!(validate_title(bad), Err(TextError::BadChar(_))),
                "{bad:?}"
            );
        }
        // A line break in the title is just a space: it will not start a new command.
        assert_eq!(validate_title("a\nop Steve").as_deref(), Ok("a op Steve"));

        assert!(is_valid_code("K7F-2QX") && is_valid_code("vanilla-crew"));
        for bad in ["", "a b", "код", "x\"y", "12345678901234567"] {
            assert!(!is_valid_code(bad), "{bad:?}");
        }
        assert_eq!(quote("a\"b\\c\nd"), "\"a\\\"b\\\\cd\"");
    }

    #[test]
    fn hostile_names_prefixes_and_titles_never_reach_the_files() {
        let room = PackRoom {
            title: "x\"},{\"text\":\"pwned".into(),
            code: "K7F\n2QX".into(),
            max_players: 0,
            footer: true,
            host_name: Some("Steve] run op Steve".into()),
            players: vec![
                PackPlayer {
                    label: Some("\"}]".into()),
                    color: palette::find("cyber_cyan"),
                    glow: true,
                    ..player("Steve", Rank::Host)
                },
                PackPlayer {
                    label: Some("VIP".into()),
                    glow: true,
                    ..player("Alex] run op Alex", Rank::Guest)
                },
                PackPlayer {
                    label: Some("a\nop Bob".into()),
                    glow: true,
                    ..player("Bob\nop Bob", Rank::Guest)
                },
                PackPlayer {
                    label: Some("X".into()),
                    ..player("Серый", Rank::Guest)
                },
                // The same nickname a second time (different case) is one player in the game.
                PackPlayer {
                    label: Some("DUP".into()),
                    ..player("STEVE", Rank::Guest)
                },
            ],
        };
        let pack = generate(PackMode::Active(&room), Some(767), &[], Lang::Ru);
        for file in &pack.files {
            if !file.path.ends_with(".mcfunction") {
                continue;
            }
            assert_only_known_commands(&file.content);
            for bad in [
                "pwned",
                "op Steve",
                "op Alex",
                "op Bob",
                "Серый",
                "DUP",
                "\"}]",
                "\\",
            ] {
                assert!(!file.content.contains(bad), "{bad:?} in {}", file.path);
            }
        }
        let apply = apply_of(&pack);
        // A bad title is replaced with the default, a bad code and a bad prefix are removed.
        assert!(apply.contains(
            "{\"text\":\"PHASE\",\"color\":\"#38BDF8\",\"bold\":true},{\"text\":\" • \","
        ));
        assert!(apply.contains("bossbar set phase:room max 1\n"));
        assert!(apply.contains("team modify ph.a00 color aqua\n"));
        assert!(!apply.contains("ph.a00 prefix"));
        assert!(
            !apply.contains("phase.sync"),
            "the host nickname is invalid, so the command is not open"
        );
        assert_eq!(pack.teams, ["ph.a00"]);
        let glow = pack.file("data/phase/function/glow.mcfunction").unwrap();
        assert_eq!(glow.matches("effect give").count(), 1);
        assert_eq!(glow.matches("effect clear").count(), 0);

        // The file for Phase Companion gets the same selection.
        let json: serde_json::Value =
            serde_json::from_str(&companion_room(Some(&room), true, Lang::Ru)).unwrap();
        assert_eq!(json["title"], "PHASE");
        assert_eq!(json["code"], serde_json::Value::Null);
        assert_eq!(json["players"].as_array().unwrap().len(), 1);
        assert_eq!(json["players"][0]["name"], "Steve");
        assert_eq!(json["players"][0]["label"], serde_json::Value::Null);
        assert_eq!(json["players"][0]["name_color"], "aqua");
    }

    /// While Phase Companion is alive (`#companion` pulse > 0), the datapack does not apply
    /// styling; once it goes silent, the datapack takes it over exactly once, at the value 1.
    #[test]
    fn companion_pulse_gates_apply() {
        let pack = generate(PackMode::Active(&free_room()), Some(767), &[], Lang::Ru);
        let gate =
            "execute unless score #companion phase.meta matches 1.. run function phase:apply\n";
        let load = pack.file("data/phase/function/load.mcfunction").unwrap();
        let changed = pack.file("data/phase/function/changed.mcfunction").unwrap();
        for text in [load, changed] {
            assert!(text.contains(gate), "{text}");
            assert_eq!(text.matches("phase:apply").count(), 1, "{text}");
        }
        assert!(
            !load.contains("#companion phase.meta 0"),
            "load does not reset the pulse"
        );

        let watch = pack.file("data/phase/function/watch.mcfunction").unwrap();
        let takeover = watch
            .find("execute if score #companion phase.meta matches 1 run function phase:apply\n")
            .expect("takeover when the mod is silent");
        let tick_down = watch
            .find("execute if score #companion phase.meta matches 1.. run scoreboard players remove #companion phase.meta 1\n")
            .expect("the pulse decreases");
        assert!(
            takeover < tick_down,
            "apply at the value 1, before it drops to 0"
        );
        // The mod sets more than 1: the takeover does not fire while it is alive.
        const { assert!(COMPANION_PULSE > 1) };

        // Cleanup does not depend on the mod: it only removes its own things and sets the `clean` mark.
        let sleeping = generate(PackMode::Sleeping, Some(767), &[], Lang::Ru);
        assert!(!apply_of(&sleeping).contains("#companion"));
    }

    #[test]
    fn companion_room_file() {
        let json: serde_json::Value =
            serde_json::from_str(&companion_room(Some(&free_room()), true, Lang::Ru)).unwrap();
        assert_eq!(json["schema"], 1);
        assert_eq!(json["active"], true);
        assert_eq!(json["title"], "PHASE");
        assert_eq!(json["code"], "K7F-2QX");
        assert_eq!(json["max_players"], 5);
        assert_eq!(
            json["players"][0],
            serde_json::json!({
                "name": "Steve", "rank": "host", "label": "HOST", "color": null,
                "name_color": null, "prefix_color": "#38BDF8", "glow": false
            })
        );
        assert_eq!(json["players"][1]["rank"], "guest");
        assert_eq!(json["account_check"], true);
        // Without styling the account-check flag still reaches the mod.
        let closed: serde_json::Value =
            serde_json::from_str(&companion_room(None, false, Lang::Ru)).unwrap();
        assert_eq!(closed["active"], false);
        assert_eq!(closed["account_check"], false);
        assert_eq!(closed["lang"], "ru");
    }

    #[test]
    fn room_language_reaches_the_datapack_and_room_file() {
        let ru = generate(PackMode::Active(&free_room()), Some(767), &[], Lang::Ru);
        let en = generate(PackMode::Active(&free_room()), Some(767), &[], Lang::En);
        let apply_ru = apply_of(&ru);
        let apply_en = apply_of(&en);
        assert!(apply_ru.contains("\" онлайн\"") && apply_ru.starts_with(header(Lang::Ru)));
        assert!(apply_en.contains("\" online\"") && apply_en.starts_with(header(Lang::En)));
        assert!(!apply_en.contains("онлайн"));
        // No Russian letters in the English datapack (except nicknames from the settings).
        for file in &en.files {
            assert!(
                !file.content.chars().any(|c| ('а'..='я').contains(&c)),
                "Russian text in the English datapack: {}",
                file.path
            );
        }
        let json = |lang| -> serde_json::Value {
            serde_json::from_str(&companion_room(Some(&free_room()), true, lang)).unwrap()
        };
        assert_eq!(json(Lang::Ru)["lang"], "ru");
        assert_eq!(json(Lang::En)["lang"], "en");
        // The file is rewritten when the language changes: the contents differ.
        assert_ne!(apply_ru, apply_en);
    }
}
