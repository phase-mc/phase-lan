//! Phase language: Russian and English, with room for more languages.
//!
//! The whole process speaks one language, [`current`]. The window picks it and tells the core
//! with the `set_language` command; the console program uses the system language ([`init_from_system`]).
//! Rust texts are written in pairs: `tr!("по-русски", "in English")`. Only the expression for
//! the chosen language is evaluated, so a `format!` inside costs nothing for the other one.
//! A language without a translation gets English. A new language: a variant in [`Lang`], a code in
//! [`Lang::from_code`], and the compiler shows where else a choice is needed.
//!
//! Selection order ([`resolve`]): the saved setting, then the installer's choice
//! (`HKCU\Software\Phase`, string `Language`: `ru` or `en`), then the Windows interface
//! language (Russian means Russian, anything else means English).

use std::sync::atomic::{AtomicU8, Ordering};

/// Interface language.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    Ru,
    En,
}

impl Lang {
    /// All languages, in display order.
    pub const ALL: [Lang; 2] = [Lang::Ru, Lang::En];

    /// Two-letter code: this is how the language is stored in settings, the registry and `room.json`.
    pub const fn code(self) -> &'static str {
        match self {
            Lang::Ru => "ru",
            Lang::En => "en",
        }
    }

    /// The language's name in that language.
    pub const fn native_name(self) -> &'static str {
        match self {
            Lang::Ru => "Русский",
            Lang::En => "English",
        }
    }

    /// Code (`ru`, `EN`, `ru-RU`, `en_US`) → language; an unknown code gives `None`.
    pub fn from_code(code: &str) -> Option<Lang> {
        let primary = code.trim().split(['-', '_']).next().unwrap_or("");
        Lang::ALL
            .into_iter()
            .find(|lang| lang.code().eq_ignore_ascii_case(primary))
    }

    /// System language from a tag like `ru-RU`: Russian means Russian, anything else means English.
    pub fn from_system_tag(tag: &str) -> Lang {
        Lang::from_code(tag).unwrap_or(Lang::En)
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Process language. Until it is set, Russian (how the program always behaved).
pub fn current() -> Lang {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Lang::En,
        _ => Lang::Ru,
    }
}

/// Set the process language (the window calls this at startup and when the language changes).
pub fn set(lang: Lang) {
    let value = match lang {
        Lang::Ru => 0,
        Lang::En => 1,
    };
    CURRENT.store(value, Ordering::Relaxed);
}

/// Is it Russian right now? For an `if` where a `tr!` pair does not fit.
pub fn is_ru() -> bool {
    current() == Lang::Ru
}

/// Text in the chosen language: `tr!("Привет", "Hello")`. Only the needed branch is evaluated.
#[macro_export]
macro_rules! tr {
    ($ru:expr, $en:expr $(,)?) => {
        match $crate::current() {
            $crate::Lang::Ru => $ru,
            _ => $en,
        }
    };
}

/// Language selection in order: saved setting → installer → Windows language → English.
/// Unknown values are skipped rather than breaking the choice.
pub fn resolve(stored: Option<&str>, installer: Option<&str>, os_tag: Option<&str>) -> Lang {
    stored
        .and_then(Lang::from_code)
        .or_else(|| installer.and_then(Lang::from_code))
        .or_else(|| os_tag.map(Lang::from_system_tag))
        .unwrap_or(Lang::En)
}

/// The installer's choice: `HKCU\Software\Phase`, string `Language`.
pub fn installer_choice() -> Option<Lang> {
    registry_language().and_then(|code| Lang::from_code(&code))
}

/// Windows interface language as a tag (`ru`, `en`, …), primary language only.
pub fn os_language_tag() -> Option<String> {
    os_tag()
}

/// Language at startup: the saved setting (if any) → installer → Windows language.
pub fn detect(stored: Option<&str>) -> Lang {
    resolve(stored, registry_language().as_deref(), os_tag().as_deref())
}

/// Console and tests: the system language; `PHASE_LANG=ru|en` overrides everything else.
pub fn init_from_system() -> Lang {
    let forced = std::env::var("PHASE_LANG").ok();
    let lang = detect(forced.as_deref());
    set(lang);
    lang
}

#[cfg(windows)]
fn registry_language() -> Option<String> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let key = wide(r"Software\Phase");
    let value = wide("Language");
    let mut buf = [0u16; 32];
    let mut size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: the strings are null-terminated, the buffer is 32 u16, `size` is its size in bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let units = (size as usize / 2).min(buf.len());
    let text = String::from_utf16_lossy(&buf[..units]);
    let text = text.trim_end_matches('\0').trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(not(windows))]
fn registry_language() -> Option<String> {
    None
}

#[cfg(windows)]
fn os_tag() -> Option<String> {
    use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
    // SAFETY: a function without parameters, returns a LANGID.
    let id = unsafe { GetUserDefaultUILanguage() };
    // The primary language is the low 10 bits; 0x19 is Russian.
    Some(if id & 0x3ff == 0x19 { "ru" } else { "en" }.to_string())
}

#[cfg(not(windows))]
fn os_tag() -> Option<String> {
    std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LANG"))
        .ok()
        .filter(|tag| !tag.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip() {
        for lang in Lang::ALL {
            assert_eq!(Lang::from_code(lang.code()), Some(lang));
        }
        assert_eq!(Lang::from_code("RU"), Some(Lang::Ru));
        assert_eq!(Lang::from_code("en_US"), Some(Lang::En));
        assert_eq!(Lang::from_code("ru-RU"), Some(Lang::Ru));
        assert_eq!(Lang::from_code("de"), None);
        assert_eq!(Lang::from_code(""), None);
    }

    #[test]
    fn native_names() {
        assert_eq!(Lang::Ru.native_name(), "Русский");
        assert_eq!(Lang::En.native_name(), "English");
    }

    #[test]
    fn system_language_ru_is_russian_everything_else_english() {
        assert_eq!(Lang::from_system_tag("ru-RU"), Lang::Ru);
        assert_eq!(Lang::from_system_tag("uk-UA"), Lang::En);
        assert_eq!(Lang::from_system_tag("de-DE"), Lang::En);
        assert_eq!(Lang::from_system_tag(""), Lang::En);
    }

    #[test]
    fn resolve_order_is_stored_then_installer_then_system() {
        // The setting wins over everything.
        assert_eq!(resolve(Some("en"), Some("ru"), Some("ru-RU")), Lang::En);
        assert_eq!(resolve(Some("ru"), Some("en"), Some("en-US")), Lang::Ru);
        // No setting: the installer's choice.
        assert_eq!(resolve(None, Some("en"), Some("ru-RU")), Lang::En);
        assert_eq!(resolve(None, Some("ru"), Some("en-US")), Lang::Ru);
        // Not that either: the Windows language.
        assert_eq!(resolve(None, None, Some("ru-RU")), Lang::Ru);
        assert_eq!(resolve(None, None, Some("fr-FR")), Lang::En);
        // Nothing known: English.
        assert_eq!(resolve(None, None, None), Lang::En);
    }

    #[test]
    fn resolve_skips_unknown_values() {
        assert_eq!(resolve(Some("xx"), Some("ru"), None), Lang::Ru);
        assert_eq!(resolve(Some(""), Some("zz"), Some("ru")), Lang::Ru);
    }

    #[test]
    fn tr_picks_the_current_language() {
        // The only test that changes the process language: the others leave it alone.
        set(Lang::Ru);
        assert_eq!(tr!("да", "yes"), "да");
        assert!(is_ru());
        set(Lang::En);
        assert_eq!(tr!("да", "yes"), "yes");
        assert_eq!(tr!(format!("{}", 1), format!("{}", 2)), "2");
        set(Lang::Ru);
        assert_eq!(current(), Lang::Ru);
    }
}
