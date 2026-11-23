//! Validating the install folder and paths from the manifest.

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::env::SetupEnv;

/// Why the folder doesn't fit. The value is the suffix of a `path.*` dictionary key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PathProblem {
    Empty,
    NotAbsolute,
    Network,
    DriveRoot,
    TooLong,
    BadName,
    NotAFolder,
    SystemFolder,
    InsideData,
}

impl PathProblem {
    pub fn key(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::NotAbsolute => "notAbsolute",
            Self::Network => "network",
            Self::DriveRoot => "driveRoot",
            Self::TooLong => "tooLong",
            Self::BadName => "badName",
            Self::NotAFolder => "notAFolder",
            Self::SystemFolder => "systemFolder",
            Self::InsideData => "insideData",
        }
    }
}

const MAX_DIR_LEN: usize = 200;
const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A comparable form of a path: lowercase, backslashes, no trailing slash or `\\?\`.
pub fn normalized(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    text.trim_end_matches('\\').to_lowercase()
}

/// The same file or folder. Text first; when it differs and both exist, the file system decides:
/// one side may use short 8.3 names (`C:\Users\RUNNER~1\...`, a short TEMP) while a shortcut
/// stores the long form of the same path.
pub fn same_path(a: &Path, b: &Path) -> bool {
    if normalized(a) == normalized(b) {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => normalized(&a) == normalized(&b),
        _ => false,
    }
}

/// `path` lies inside `parent` (is not equal to it).
pub fn is_inside(path: &Path, parent: &Path) -> bool {
    normalized(path).starts_with(&format!("{}\\", normalized(parent)))
}

fn valid_component(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim().to_lowercase();
    !name.is_empty()
        && !name.ends_with(['.', ' '])
        && !name
            .chars()
            .any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || (c as u32) < 32)
        && !RESERVED.contains(&stem.as_str())
}

/// Validates the install folder and returns it in clean form.
pub fn validate_install_dir(raw: &str, env: &SetupEnv) -> Result<PathBuf, PathProblem> {
    let text = raw.trim().replace('/', "\\");
    let text = text.trim_end_matches('\\');
    if text.is_empty() {
        return Err(PathProblem::Empty);
    }
    if text.starts_with(r"\\") {
        return Err(PathProblem::Network);
    }
    let bytes = text.as_bytes();
    let has_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if !has_drive || (bytes.len() > 2 && bytes[2] != b'\\') {
        return Err(PathProblem::NotAbsolute);
    }
    if bytes.len() == 2 {
        return Err(PathProblem::DriveRoot);
    }
    if text.chars().count() > MAX_DIR_LEN {
        return Err(PathProblem::TooLong);
    }
    if text[3..]
        .split('\\')
        .any(|part| part == "." || part == ".." || !valid_component(part))
    {
        return Err(PathProblem::BadName);
    }
    let dir = PathBuf::from(text);
    if dir.exists() && !dir.is_dir() {
        return Err(PathProblem::NotAFolder);
    }

    // Windows system folders and containers where Phase must not be.
    let windows = std::env::var_os("SystemRoot").map(PathBuf::from);
    if windows
        .as_deref()
        .is_some_and(|win| same_path(&dir, win) || is_inside(&dir, win))
    {
        return Err(PathProblem::SystemFolder);
    }
    for container in [
        &env.local_appdata,
        &env.roaming_appdata,
        &env.temp,
        &env.desktop,
        &env.start_menu,
    ] {
        if same_path(&dir, container) {
            return Err(PathProblem::SystemFolder);
        }
    }
    if is_inside(&dir, &env.start_menu) || is_inside(&dir, &env.temp) {
        return Err(PathProblem::SystemFolder);
    }
    // Inside the data folder an installation is dangerous: deleting the data would take it out too, and vice versa.
    // The data folder itself is allowed: the previous NSIS installer installed there.
    if is_inside(&dir, &env.data_dir()) {
        return Err(PathProblem::InsideData);
    }
    Ok(dir)
}

/// A path from the manifest: only a simple relative name inside the install folder.
pub fn safe_relative(path: &str) -> Option<PathBuf> {
    if path.is_empty() || path.contains(':') {
        return None;
    }
    let relative = PathBuf::from(path.replace('/', "\\"));
    let safe = relative.components().all(|part| match part {
        Component::Normal(name) => name.to_str().is_some_and(valid_component),
        _ => false,
    });
    safe.then_some(relative)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::reg::MemReg;

    fn env() -> SetupEnv {
        SetupEnv::in_folder(Path::new(r"C:\sandbox-x"), Arc::new(MemReg::default()))
    }

    fn check(path: &str) -> Result<PathBuf, PathProblem> {
        validate_install_dir(path, &env())
    }

    #[test]
    fn accepts_normal_folders() {
        assert_eq!(
            check(r"D:\Games\Phase\").unwrap(),
            PathBuf::from(r"D:\Games\Phase")
        );
        assert!(check(r"C:\sandbox-x\LocalAppData\Programs\Phase").is_ok());
        assert!(check("D:/Игры/Phase").is_ok());
        // The data folder is allowed: an NSIS installation lives there.
        assert!(check(r"C:\sandbox-x\LocalAppData\Phase").is_ok());
    }

    #[test]
    fn rejects_bad_folders() {
        assert_eq!(check("  "), Err(PathProblem::Empty));
        assert_eq!(check(r"Games\Phase"), Err(PathProblem::NotAbsolute));
        assert_eq!(check(r"C:Phase"), Err(PathProblem::NotAbsolute));
        assert_eq!(check(r"\\server\share\Phase"), Err(PathProblem::Network));
        assert_eq!(check(r"C:\"), Err(PathProblem::DriveRoot));
        assert_eq!(check(r"D:\a?b"), Err(PathProblem::BadName));
        assert_eq!(check(r"D:\CON\Phase"), Err(PathProblem::BadName));
        assert_eq!(check(r"D:\x\..\Phase"), Err(PathProblem::BadName));
        assert_eq!(check(r"D:\trailing."), Err(PathProblem::BadName));
        assert_eq!(
            check(&format!(r"D:\{}", "a".repeat(240))),
            Err(PathProblem::TooLong)
        );
        assert_eq!(
            check(r"C:\sandbox-x\LocalAppData"),
            Err(PathProblem::SystemFolder)
        );
        assert_eq!(
            check(r"C:\sandbox-x\Temp\Phase"),
            Err(PathProblem::SystemFolder)
        );
        assert_eq!(
            check(r"C:\sandbox-x\LocalAppData\Phase\profiles"),
            Err(PathProblem::InsideData)
        );
    }

    #[test]
    fn manifest_paths_stay_inside() {
        assert!(safe_relative("Phase.exe").is_some());
        assert!(safe_relative("sub/file.dll").is_some());
        for bad in [
            "",
            "..\\x",
            "a\\..\\..\\x",
            "C:\\x",
            "\\x",
            "file.txt:stream",
            "/etc",
        ] {
            assert!(safe_relative(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn comparison_ignores_case_and_prefix() {
        assert!(same_path(Path::new(r"\\?\C:\A\b\"), Path::new("c:/a/B")));
        assert!(is_inside(Path::new(r"C:\a\b"), Path::new(r"c:\A")));
        assert!(!is_inside(Path::new(r"C:\ab"), Path::new(r"C:\a")));
    }
}
