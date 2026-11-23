//! The install manifest `phase-install.json`: exactly what belongs to the installer.
//!
//! Uninstall goes only by this list, so user files that happen to be nearby
//! (the NSIS install folder is also the data folder) stay in place.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{APP_EXE, MANIFEST_FILE, UNINSTALL_EXE, paths};

const SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedShortcuts {
    pub start_menu: bool,
    pub desktop: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub product: String,
    pub version: String,
    pub files: Vec<OwnedFile>,
    pub shortcuts: OwnedShortcuts,
    /// The value `HKCU\Software\Phase\Language` was created by the installer.
    #[serde(default)]
    pub language_created: bool,
}

impl Manifest {
    pub fn new(version: &str, files: Vec<OwnedFile>, shortcuts: OwnedShortcuts) -> Self {
        Self {
            schema: SCHEMA,
            product: crate::PRODUCT.to_string(),
            version: version.to_string(),
            files,
            shortcuts,
            language_created: false,
        }
    }

    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(MANIFEST_FILE)
    }

    /// Reads the manifest from a folder. `Ok(None)` — there is no file; a corrupted manifest and paths
    /// outside the folder are an error: nothing may be deleted by such a list.
    pub fn load(dir: &Path) -> io::Result<Option<Self>> {
        let bytes = match fs::read(Self::path_in(dir)) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let manifest: Self = serde_json::from_slice(&bytes)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        if manifest.schema != SCHEMA || manifest.product != crate::PRODUCT {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "чужой манифест"));
        }
        if manifest
            .files
            .iter()
            .any(|file| paths::safe_relative(&file.path).is_none())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "путь вне папки установки",
            ));
        }
        Ok(Some(manifest))
    }

    /// Writes the manifest to the given file.
    pub fn write_to(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        fs::write(path, json)
    }

    /// What counts as ours in an NSIS installation that has no manifest yet: exactly the two files
    /// it put there.
    pub fn for_nsis_install() -> Self {
        let owned = |path: &str| OwnedFile {
            path: path.to_string(),
            size: 0,
            sha256: String::new(),
        };
        Self::new(
            "",
            vec![owned(APP_EXE), owned(UNINSTALL_EXE)],
            OwnedShortcuts {
                start_menu: true,
                desktop: true,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("phase-manifest-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trip_and_missing_file() {
        let dir = temp("round");
        assert_eq!(Manifest::load(&dir).unwrap(), None);
        let manifest = Manifest::new(
            "1.2.3",
            vec![OwnedFile {
                path: APP_EXE.into(),
                size: 5,
                sha256: "ab".into(),
            }],
            OwnedShortcuts::default(),
        );
        manifest.write_to(&Manifest::path_in(&dir)).unwrap();
        assert_eq!(Manifest::load(&dir).unwrap(), Some(manifest));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_paths_outside_the_folder_and_garbage() {
        let dir = temp("evil");
        let mut manifest = Manifest::new("1", Vec::new(), OwnedShortcuts::default());
        manifest.files.push(OwnedFile {
            path: r"..\profiles\device.key".into(),
            size: 0,
            sha256: String::new(),
        });
        manifest.write_to(&Manifest::path_in(&dir)).unwrap();
        assert!(Manifest::load(&dir).is_err());
        fs::write(Manifest::path_in(&dir), b"{not json").unwrap();
        assert!(Manifest::load(&dir).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
