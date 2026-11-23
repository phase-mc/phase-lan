//! Installer errors: a code for the window, text from the dictionary and an exit code for `/S`.

use std::{io, path::Path};

use serde::Serialize;

use crate::i18n;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "camelCase")]
pub enum SetupError {
    /// The folder doesn't fit; `problem` — the reason key from [`crate::paths::PathProblem`].
    #[serde(rename_all = "camelCase")]
    InvalidPath {
        problem: String,
    },
    /// Phase is already installed in another folder, and `/D` points here.
    #[serde(rename_all = "camelCase")]
    InstalledElsewhere {
        path: String,
    },
    #[serde(rename_all = "camelCase")]
    PhaseRunning {
        pids: Vec<u32>,
    },
    #[serde(rename_all = "camelCase")]
    NoWriteAccess {
        path: String,
    },
    #[serde(rename_all = "camelCase")]
    NotEnoughDisk {
        need: u64,
        free: u64,
    },
    CorruptPayload,
    Cancelled,
    #[serde(rename_all = "camelCase")]
    FileLocked {
        path: String,
    },
    NotInstalled,
    AlreadyRunning,
    #[serde(rename_all = "camelCase")]
    Io {
        step: String,
        detail: String,
    },
}

impl SetupError {
    /// Exit code for silent mode. Zero — success, 2 — invalid arguments.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Io { .. } => 1,
            Self::PhaseRunning { .. } | Self::FileLocked { .. } => 3,
            Self::NoWriteAccess { .. } => 4,
            Self::NotEnoughDisk { .. } => 5,
            Self::CorruptPayload => 6,
            Self::InvalidPath { .. } | Self::InstalledElsewhere { .. } => 7,
            Self::AlreadyRunning => 8,
            Self::Cancelled => 9,
            Self::NotInstalled => 10,
        }
    }

    /// The text key in the dictionary.
    pub fn key(&self) -> &'static str {
        match self {
            Self::InvalidPath { .. } => "error.invalidPath",
            Self::InstalledElsewhere { .. } => "error.installedElsewhere",
            Self::PhaseRunning { .. } => "error.phaseRunning",
            Self::NoWriteAccess { .. } => "error.noWriteAccess",
            // Zero need — the disk filled up during the write itself.
            Self::NotEnoughDisk { need: 0, .. } => "error.diskFull",
            Self::NotEnoughDisk { .. } => "error.notEnoughDisk",
            Self::CorruptPayload => "error.corruptPayload",
            Self::Cancelled => "error.cancelled",
            Self::FileLocked { .. } => "error.fileLocked",
            Self::NotInstalled => "error.notInstalled",
            Self::AlreadyRunning => "error.alreadyRunning",
            Self::Io { .. } => "error.io",
        }
    }

    /// The error text in language `lang` (silent mode output).
    pub fn message(&self, lang: &str) -> String {
        let text = i18n::text(lang, self.key());
        match self {
            Self::NoWriteAccess { path }
            | Self::FileLocked { path }
            | Self::InstalledElsewhere { path } => text.replace("{path}", path),
            Self::Io { detail, .. } => text.replace("{detail}", detail),
            Self::NotEnoughDisk { need, free } => text
                .replace("{need}", &i18n::megabytes(*need))
                .replace("{free}", &i18n::megabytes(*free)),
            Self::InvalidPath { problem } => {
                format!("{text} {}", i18n::text(lang, &format!("path.{problem}")))
            }
            _ => text,
        }
    }

    /// An I/O error at step `step`, with a clear reason for common cases.
    pub fn from_io(step: &str, path: &Path, err: &io::Error) -> Self {
        // ERROR_SHARING_VIOLATION and ERROR_LOCK_VIOLATION: the file is busy with another program.
        if matches!(err.raw_os_error(), Some(32 | 33)) {
            return Self::FileLocked {
                path: path.display().to_string(),
            };
        }
        // ERROR_DISK_FULL and ERROR_HANDLE_DISK_FULL: space ran out during the write itself.
        if matches!(err.raw_os_error(), Some(39 | 112)) {
            return Self::NotEnoughDisk { need: 0, free: 0 };
        }
        if err.kind() == io::ErrorKind::PermissionDenied {
            return Self::NoWriteAccess {
                path: path.display().to_string(),
            };
        }
        Self::Io {
            step: step.to_string(),
            detail: format!("{}: {err}", path.display()),
        }
    }
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for SetupError {}

pub type Result<T> = std::result::Result<T, SetupError>;
