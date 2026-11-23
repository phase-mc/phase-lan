//! Change journal: everything the installation did can be undone in reverse order.
//!
//! Files aren't deleted at once but move to a service folder next to them (the same drive, so
//! a rename is instant and reliable). Until the journal is committed, a rollback restores everything as it
//! was, including on a panic: there is a rollback in `Drop` too.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::{env::SetupEnv, reg::RegVal};

/// The start of the service folder's name inside the install folder.
pub const WORK_PREFIX: &str = ".phase-setup.tmp-";

enum Undo {
    RemoveFile(PathBuf),
    /// Put the file back from the service folder.
    RestoreFile {
        backup: PathBuf,
        target: PathBuf,
    },
    RemoveDirIfEmpty(PathBuf),
    RestoreValue {
        key: String,
        name: String,
        old: Option<RegVal>,
    },
    RemoveKeyIfEmpty(String),
}

/// Moves a file. Shortcuts may lie on another drive (a desktop in OneDrive, etc.):
/// then instead of a rename — a copy and a delete.
fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    /// `ERROR_NOT_SAME_DEVICE`.
    const OTHER_DISK: i32 = 17;
    match fs::rename(from, to) {
        Err(err) if err.raw_os_error() == Some(OTHER_DISK) => {
            fs::copy(from, to)?;
            fs::remove_file(from)
        }
        result => result,
    }
}

pub struct Tx<'a> {
    env: &'a SetupEnv,
    work_dir: PathBuf,
    backups: usize,
    undo: Vec<Undo>,
    finished: bool,
}

impl<'a> Tx<'a> {
    /// `work_dir` is created in advance, its creation is the journal's first entry.
    pub fn new(env: &'a SetupEnv, work_dir: PathBuf) -> Self {
        Self {
            env,
            work_dir,
            backups: 0,
            undo: Vec::new(),
            finished: false,
        }
    }

    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    /// Creates a folder with all its parents and remembers which of them appeared just now.
    pub fn create_dir_all(&mut self, dir: &Path) -> io::Result<()> {
        let mut missing = Vec::new();
        let mut cursor = Some(dir);
        while let Some(path) = cursor {
            if path.exists() {
                break;
            }
            missing.push(path.to_path_buf());
            cursor = path.parent();
        }
        for path in missing.into_iter().rev() {
            fs::create_dir(&path)?;
            self.undo.push(Undo::RemoveDirIfEmpty(path));
        }
        Ok(())
    }

    /// Moves a file to the service folder (if it exists). A rollback will restore it.
    pub fn set_aside(&mut self, target: &Path) -> io::Result<bool> {
        if !target.exists() {
            return Ok(false);
        }
        self.backups += 1;
        let backup = self.work_dir.join(format!("backup-{}", self.backups));
        move_file(target, &backup)?;
        self.undo.push(Undo::RestoreFile {
            backup,
            target: target.to_path_buf(),
        });
        Ok(true)
    }

    /// Puts a prepared file in place, keeping the previous one.
    pub fn place(&mut self, staged: &Path, target: &Path) -> io::Result<()> {
        self.set_aside(target)?;
        fs::rename(staged, target)?;
        self.undo.push(Undo::RemoveFile(target.to_path_buf()));
        Ok(())
    }

    /// Remembers a file created directly (a shortcut): a rollback will delete it.
    pub fn created_file(&mut self, target: &Path) {
        self.undo.push(Undo::RemoveFile(target.to_path_buf()));
    }

    /// Writes a registry value, remembering the previous one and the keys created.
    pub fn set_value(&mut self, key: &str, name: &str, value: RegVal) -> io::Result<()> {
        let reg = self.env.reg.clone();
        let old = reg.get(key, name)?;
        if old.as_ref() == Some(&value) {
            return Ok(());
        }
        // Parents from the top down: the rollback goes from the bottom up.
        let mut created = Vec::new();
        let mut cursor = Some(key);
        while let Some(path) = cursor {
            if reg.key_exists(path) {
                break;
            }
            created.push(path.to_string());
            cursor = path.rsplit_once('\\').map(|(parent, _)| parent);
        }
        for path in created.into_iter().rev() {
            self.undo.push(Undo::RemoveKeyIfEmpty(path));
        }
        self.undo.push(Undo::RestoreValue {
            key: key.to_string(),
            name: name.to_string(),
            old,
        });
        reg.set(key, name, &value)
    }

    /// Deletes a registry value, remembering it.
    pub fn delete_value(&mut self, key: &str, name: &str) -> io::Result<()> {
        let reg = self.env.reg.clone();
        let Some(old) = reg.get(key, name)? else {
            return Ok(());
        };
        self.undo.push(Undo::RestoreValue {
            key: key.to_string(),
            name: name.to_string(),
            old: Some(old),
        });
        reg.delete_value(key, name)
    }

    /// Everything succeeded: the service folder is no longer needed, nothing to roll back.
    pub fn commit(mut self) {
        self.finished = true;
        self.undo.clear();
        let _ = fs::remove_dir_all(&self.work_dir);
    }

    /// Undoes everything in reverse order and removes the service folder.
    pub fn rollback(mut self) {
        self.run_rollback();
    }

    fn run_rollback(&mut self) {
        self.finished = true;
        for step in std::mem::take(&mut self.undo).into_iter().rev() {
            // The rollback runs to the end: a failure of one step must not stop the others.
            let _ = match step {
                Undo::RemoveFile(path) => fs::remove_file(path),
                Undo::RestoreFile { backup, target } => {
                    let _ = fs::remove_file(&target);
                    move_file(&backup, &target)
                }
                Undo::RemoveDirIfEmpty(path) => {
                    // We remove the service folder before the parent it lies in.
                    if path == self.work_dir {
                        let _ = fs::remove_dir_all(&path);
                    }
                    fs::remove_dir(path)
                }
                Undo::RestoreValue { key, name, old } => match old {
                    Some(value) => self.env.reg.set(&key, &name, &value),
                    None => self.env.reg.delete_value(&key, &name),
                },
                Undo::RemoveKeyIfEmpty(key) => self.env.reg.delete_key_if_empty(&key).map(|_| ()),
            };
        }
        let _ = fs::remove_dir_all(&self.work_dir);
    }
}

impl Drop for Tx<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.run_rollback();
        }
    }
}
