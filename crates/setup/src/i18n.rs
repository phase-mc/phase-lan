//! Installer languages. One dictionary per language: `crates/setup/i18n/<code>.json`.
//!
//! To add a language: put the dictionary file in place and add a line to [`LANGUAGES`]. The window,
//! system dialogs and silent mode output take their strings from here; a test checks that
//! every dictionary has all the keys.

use std::collections::BTreeMap;

pub struct Language {
    pub code: &'static str,
    /// The name in the language itself: that's how it is found in the menu.
    pub native_name: &'static str,
    pub source: &'static str,
}

pub const LANGUAGES: &[Language] = &[
    Language {
        code: "ru",
        native_name: "Русский",
        source: include_str!("../i18n/ru.json"),
    },
    Language {
        code: "en",
        native_name: "English",
        source: include_str!("../i18n/en.json"),
    },
];

/// The language if the system one isn't supported.
pub const FALLBACK: &str = "en";

/// The code of a supported language or `None`.
pub fn supported(code: &str) -> Option<&'static str> {
    LANGUAGES
        .iter()
        .find(|language| language.code.eq_ignore_ascii_case(code.trim()))
        .map(|language| language.code)
}

/// The language's dictionary; an unknown code — the fallback language's dictionary.
pub fn dictionary(code: &str) -> BTreeMap<String, String> {
    let language = LANGUAGES
        .iter()
        .find(|language| language.code == code)
        .or_else(|| LANGUAGES.iter().find(|language| language.code == FALLBACK))
        .expect("запасной язык есть в списке");
    serde_json::from_str(language.source).expect("словарь — корректный JSON")
}

/// A dictionary string; no key — the key itself (a test keeps this from happening in the build).
pub fn text(code: &str, key: &str) -> String {
    dictionary(code)
        .remove(key)
        .unwrap_or_else(|| key.to_string())
}

/// A size for a message: the window formats it with units, here — only the number.
pub fn megabytes(bytes: u64) -> String {
    bytes.div_ceil(1024 * 1024).to_string()
}

/// The language code from a Windows language identifier (LANGID).
pub fn from_langid(langid: u16) -> &'static str {
    // The low 10 bits are the primary language.
    match langid & 0x3ff {
        0x19 => "ru",
        0x09 => "en",
        _ => FALLBACK,
    }
}

/// The Windows UI language if it is supported, otherwise the fallback.
#[cfg(windows)]
pub fn system_language() -> &'static str {
    // SAFETY: a function without arguments.
    from_langid(unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() })
}

#[cfg(not(windows))]
pub fn system_language() -> &'static str {
    FALLBACK
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn placeholders(text: &str) -> BTreeSet<String> {
        text.split('{')
            .skip(1)
            .filter_map(|part| part.split_once('}').map(|(name, _)| name.to_string()))
            .collect()
    }

    #[test]
    fn every_language_has_every_key_and_the_same_placeholders() {
        let reference = dictionary(LANGUAGES[0].code);
        assert!(!reference.is_empty());
        for language in LANGUAGES {
            let dict: BTreeMap<String, String> = serde_json::from_str(language.source).unwrap();
            let missing: Vec<_> = reference
                .keys()
                .filter(|k| !dict.contains_key(*k))
                .collect();
            let extra: Vec<_> = dict
                .keys()
                .filter(|k| !reference.contains_key(*k))
                .collect();
            assert!(
                missing.is_empty(),
                "{}: нет ключей {missing:?}",
                language.code
            );
            assert!(
                extra.is_empty(),
                "{}: лишние ключи {extra:?}",
                language.code
            );
            for (key, value) in &dict {
                assert!(!value.trim().is_empty(), "{}: пустой {key}", language.code);
                assert_eq!(
                    placeholders(value),
                    placeholders(&reference[key]),
                    "{}: подстановки в {key}",
                    language.code
                );
            }
        }
    }

    #[test]
    fn every_key_used_by_the_window_and_errors_exists() {
        let dict = dictionary("ru");
        let source = concat!(
            include_str!("../ui/app.js"),
            include_str!("../ui/index.html")
        );
        let mut used = BTreeSet::new();
        for marker in ["t(\"", "data-i18n=\"", "data-i18n-label=\""] {
            for (index, _) in source.match_indices(marker) {
                // `split("` and the like are not a t() call.
                let before = source[..index].chars().next_back().unwrap_or(' ');
                if marker == "t(\"" && (before.is_alphanumeric() || before == '.' || before == '_')
                {
                    continue;
                }
                if let Some((key, _)) = source[index + marker.len()..].split_once('"') {
                    used.insert(key.to_string());
                }
            }
        }
        assert!(used.len() > 20, "ключи окна не найдены");
        for key in used {
            assert!(dict.contains_key(&key), "в словаре нет {key}");
        }
        // Keys that the window and the output assemble from parts.
        for stage in [
            "verify",
            "extract",
            "uninstaller",
            "files",
            "shortcuts",
            "registry",
            "finish",
        ] {
            assert!(dict.contains_key(&format!("stage.{stage}")), "{stage}");
        }
        use crate::paths::PathProblem::*;
        for problem in [
            Empty,
            NotAbsolute,
            Network,
            DriveRoot,
            TooLong,
            BadName,
            NotAFolder,
            SystemFolder,
            InsideData,
        ] {
            assert!(
                dict.contains_key(&format!("path.{}", problem.key())),
                "{problem:?}"
            );
        }
        for error in [
            crate::error::SetupError::CorruptPayload,
            crate::error::SetupError::Cancelled,
            crate::error::SetupError::NotInstalled,
            crate::error::SetupError::AlreadyRunning,
            crate::error::SetupError::NotEnoughDisk { need: 0, free: 0 },
            crate::error::SetupError::NotEnoughDisk { need: 1, free: 0 },
            crate::error::SetupError::PhaseRunning { pids: Vec::new() },
            crate::error::SetupError::NoWriteAccess {
                path: String::new(),
            },
            crate::error::SetupError::FileLocked {
                path: String::new(),
            },
            crate::error::SetupError::InstalledElsewhere {
                path: String::new(),
            },
            crate::error::SetupError::InvalidPath {
                problem: String::new(),
            },
            crate::error::SetupError::Io {
                step: String::new(),
                detail: String::new(),
            },
        ] {
            assert!(dict.contains_key(error.key()), "{}", error.key());
        }
    }

    #[test]
    fn language_detection() {
        assert_eq!(from_langid(0x0419), "ru");
        assert_eq!(from_langid(0x0409), "en");
        assert_eq!(from_langid(0x0809), "en");
        // Unsupported (German) — English.
        assert_eq!(from_langid(0x0407), "en");
        assert_eq!(supported("EN"), Some("en"));
        assert_eq!(supported("de"), None);
    }
}
