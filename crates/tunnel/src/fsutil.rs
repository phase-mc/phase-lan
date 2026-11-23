//! Safe file writes: an interrupted write (crash, PC shutdown) does not leave
//! a corrupted file.

use phase_lang::tr;
use std::{
    fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

/// Temporary file next to the target (in the same folder, so the rename is atomic).
fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp{}", std::process::id()));
    path.with_file_name(name)
}

fn write_temp(path: &Path, data: &[u8]) -> Result<PathBuf> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            tr!(
                format!("не удалось создать папку {}", parent.display()),
                format!("could not create folder {}", parent.display())
            )
        })?;
    }
    let tmp = temp_path(path);
    let mut file = fs::File::create(&tmp).with_context(|| {
        tr!(
            format!("не удалось создать {}", tmp.display()),
            format!("could not create {}", tmp.display())
        )
    })?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(tmp)
}

/// Writes the file in full or does not change it at all (write to a temp file + replace).
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = write_temp(path, data)?;
    fs::rename(&tmp, path).with_context(|| {
        let _ = fs::remove_file(&tmp);
        tr!(
            format!("не удалось сохранить {}", path.display()),
            format!("could not save {}", path.display())
        )
    })
}

/// Creates the file only if it does not exist yet. Returns `false` if the file already existed
/// (for example, another program instance created it at the same time); then it is not changed.
pub fn create_new_atomic(path: &Path, data: &[u8]) -> Result<bool> {
    let tmp = write_temp(path, data)?;
    let result = fs::hard_link(&tmp, path);
    let _ = fs::remove_file(&tmp);
    match result {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(err) => Err(err).with_context(|| {
            tr!(
                format!("не удалось создать {}", path.display()),
                format!("could not create {}", path.display())
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_and_create_new() {
        let dir = std::env::temp_dir().join(format!("phase-test-fs-{}", std::process::id()));
        let file = dir.join("data.txt");
        let _ = fs::remove_dir_all(&dir);

        assert!(create_new_atomic(&file, b"first").unwrap());
        assert!(
            !create_new_atomic(&file, b"second").unwrap(),
            "an existing file is left alone"
        );
        assert_eq!(fs::read(&file).unwrap(), b"first");

        write_atomic(&file, b"replaced").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"replaced");
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            1,
            "no temporary files remain"
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
