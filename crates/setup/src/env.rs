//! Where the installer works: user folders and the registry.
//!
//! The sandbox (`PHASE_SETUP_SANDBOX=<folder>`) exists only in debug builds: all
//! paths and the registry go inside it, the real installation is untouched. In a release
//! build this code doesn't exist at all.

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{DATA_DIR_NAME, PRODUCT, reg::RegStore};

pub struct SetupEnv {
    pub local_appdata: PathBuf,
    pub roaming_appdata: PathBuf,
    /// The Start menu "Programs" folder.
    pub start_menu: PathBuf,
    pub desktop: PathBuf,
    pub temp: PathBuf,
    pub reg: Arc<dyn RegStore>,
    /// Sandbox: only notice Phase processes inside this folder.
    pub process_scope: Option<PathBuf>,
    /// The step at which a debug build simulates a failure (testing rollback).
    pub fault: Option<String>,
}

impl SetupEnv {
    /// The default install folder.
    pub fn default_install_dir(&self) -> PathBuf {
        self.local_appdata.join("Programs").join(PRODUCT)
    }

    /// Phase data folder: profiles with device keys, friends, settings.
    pub fn data_dir(&self) -> PathBuf {
        self.local_appdata.join(DATA_DIR_NAME)
    }

    pub fn is_sandbox(&self) -> bool {
        self.process_scope.is_some()
    }

    /// Rollback checkpoint: in a debug build a failure can be triggered here.
    pub fn checkpoint(&self, step: &str) -> io::Result<()> {
        if cfg!(debug_assertions) && self.fault.as_deref() == Some(step) {
            return Err(io::Error::other(format!("тестовый сбой на шаге {step}")));
        }
        Ok(())
    }

    /// An environment inside the `root` folder with the given registry (tests and the sandbox).
    pub fn in_folder(root: &Path, reg: Arc<dyn RegStore>) -> Self {
        Self {
            local_appdata: root.join("LocalAppData"),
            roaming_appdata: root.join("RoamingAppData"),
            start_menu: root.join("StartMenu").join("Programs"),
            desktop: root.join("Desktop"),
            temp: root.join("Temp"),
            reg,
            process_scope: Some(root.to_path_buf()),
            fault: None,
        }
    }

    /// Creates the environment folders in the sandbox.
    pub fn create_folders(&self) -> io::Result<()> {
        for dir in [
            &self.local_appdata,
            &self.roaming_appdata,
            &self.start_menu,
            &self.desktop,
            &self.temp,
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use win::detect;

#[cfg(windows)]
mod win {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};

    use windows_sys::{
        Win32::{
            System::Com::CoTaskMemFree,
            UI::Shell::{
                FOLDERID_Desktop, FOLDERID_LocalAppData, FOLDERID_Programs,
                FOLDERID_RoamingAppData, SHGetKnownFolderPath,
            },
        },
        core::GUID,
    };

    use super::*;
    use crate::reg::WinReg;

    fn known_folder(id: &GUID) -> io::Result<PathBuf> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: `raw` receives a COM string, we free it below.
        let status = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut raw) };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status));
        }
        // SAFETY: on success `raw` is a NUL-terminated string.
        let path = unsafe {
            let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
            OsString::from_wide(std::slice::from_raw_parts(raw, len))
        };
        // SAFETY: the string was allocated by SHGetKnownFolderPath.
        unsafe { CoTaskMemFree(raw.cast()) };
        Ok(PathBuf::from(path))
    }

    /// The user's real environment (or the sandbox of a debug build).
    pub fn detect() -> io::Result<SetupEnv> {
        #[cfg(debug_assertions)]
        if let Some(root) = std::env::var_os("PHASE_SETUP_SANDBOX") {
            let root = PathBuf::from(root);
            let name = root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "default".into());
            let prefix = format!(r"Software\PhaseSetupSandbox\{name}");
            let mut env = SetupEnv::in_folder(&root, Arc::new(WinReg::sandboxed(&prefix)));
            env.fault = std::env::var("PHASE_SETUP_FAULT").ok();
            env.create_folders()?;
            return Ok(env);
        }
        // The data folder — as in Phase itself: the LOCALAPPDATA variable first.
        let local_appdata = match std::env::var_os("LOCALAPPDATA") {
            Some(path) => PathBuf::from(path),
            None => known_folder(&FOLDERID_LocalAppData)?,
        };
        Ok(SetupEnv {
            local_appdata,
            roaming_appdata: known_folder(&FOLDERID_RoamingAppData)?,
            start_menu: known_folder(&FOLDERID_Programs)?,
            desktop: known_folder(&FOLDERID_Desktop)?,
            temp: std::env::temp_dir(),
            reg: Arc::new(WinReg::real()),
            process_scope: None,
            fault: None,
        })
    }
}
