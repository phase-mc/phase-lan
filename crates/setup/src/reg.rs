//! The current user's registry behind a small interface.
//!
//! The real implementation is [`WinReg`]; it may have a prefix (only the debug build's sandbox),
//! then all keys live inside it. [`MemReg`] is for tests.

use std::{collections::BTreeMap, io, sync::Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegVal {
    Str(String),
    Dword(u32),
    Bin(Vec<u8>),
}

impl RegVal {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(text) => Some(text),
            _ => None,
        }
    }
}

/// A key's default value — the empty name.
pub trait RegStore: Send + Sync {
    fn get(&self, key: &str, name: &str) -> io::Result<Option<RegVal>>;
    fn set(&self, key: &str, name: &str, value: &RegVal) -> io::Result<()>;
    fn delete_value(&self, key: &str, name: &str) -> io::Result<()>;
    fn value_names(&self, key: &str) -> io::Result<Vec<String>>;
    fn key_exists(&self, key: &str) -> bool;
    /// Deletes a key only if it has no values or subkeys.
    fn delete_key_if_empty(&self, key: &str) -> io::Result<bool>;

    fn get_str(&self, key: &str, name: &str) -> Option<String> {
        match self.get(key, name) {
            Ok(Some(RegVal::Str(text))) => Some(text),
            _ => None,
        }
    }
}

#[cfg(windows)]
pub use win::WinReg;

#[cfg(windows)]
mod win {
    use windows_registry::{CURRENT_USER, Key, Type};

    use super::*;

    /// The `HRESULT` "file not found".
    const NOT_FOUND: i32 = 0x8007_0002_u32 as i32;

    pub struct WinReg {
        prefix: String,
    }

    impl WinReg {
        /// The real `HKCU`.
        pub fn real() -> Self {
            Self {
                prefix: String::new(),
            }
        }

        /// All keys are inside `HKCU\<prefix>`. For the sandbox and tests only.
        pub fn sandboxed(prefix: &str) -> Self {
            Self {
                prefix: format!("{}\\", prefix.trim_end_matches('\\')),
            }
        }

        fn path(&self, key: &str) -> String {
            format!("{}{key}", self.prefix)
        }

        fn open(&self, key: &str) -> windows_registry::Result<Key> {
            CURRENT_USER.open(self.path(key))
        }
    }

    fn missing(err: &windows::core::Error) -> bool {
        err.code().0 == NOT_FOUND
    }

    fn to_io(err: windows::core::Error) -> io::Error {
        io::Error::from(err)
    }

    impl RegStore for WinReg {
        fn get(&self, key: &str, name: &str) -> io::Result<Option<RegVal>> {
            let handle = match self.open(key) {
                Ok(handle) => handle,
                Err(err) if missing(&err) => return Ok(None),
                Err(err) => return Err(to_io(err)),
            };
            let kind = match handle.get_type(name) {
                Ok(kind) => kind,
                Err(err) if missing(&err) => return Ok(None),
                Err(err) => return Err(to_io(err)),
            };
            let value = match kind {
                Type::String | Type::ExpandString => {
                    RegVal::Str(handle.get_string(name).map_err(to_io)?)
                }
                Type::U32 => RegVal::Dword(handle.get_u32(name).map_err(to_io)?),
                _ => RegVal::Bin(handle.get_value(name).map_err(to_io)?.to_vec()),
            };
            Ok(Some(value))
        }

        fn set(&self, key: &str, name: &str, value: &RegVal) -> io::Result<()> {
            let handle = CURRENT_USER.create(self.path(key)).map_err(to_io)?;
            match value {
                RegVal::Str(text) => handle.set_string(name, text),
                RegVal::Dword(number) => handle.set_u32(name, *number),
                RegVal::Bin(bytes) => handle.set_bytes(name, Type::Bytes, bytes),
            }
            .map_err(to_io)
        }

        fn delete_value(&self, key: &str, name: &str) -> io::Result<()> {
            let handle = match CURRENT_USER.options().read().write().open(self.path(key)) {
                Ok(handle) => handle,
                Err(err) if missing(&err) => return Ok(()),
                Err(err) => return Err(to_io(err)),
            };
            match handle.remove_value(name) {
                Err(err) if !missing(&err) => Err(to_io(err)),
                _ => Ok(()),
            }
        }

        fn value_names(&self, key: &str) -> io::Result<Vec<String>> {
            let handle = match self.open(key) {
                Ok(handle) => handle,
                Err(err) if missing(&err) => return Ok(Vec::new()),
                Err(err) => return Err(to_io(err)),
            };
            Ok(handle
                .values()
                .map_err(to_io)?
                .map(|(name, _)| name)
                .collect())
        }

        fn key_exists(&self, key: &str) -> bool {
            self.open(key).is_ok()
        }

        fn delete_key_if_empty(&self, key: &str) -> io::Result<bool> {
            let handle = match self.open(key) {
                Ok(handle) => handle,
                Err(err) if missing(&err) => return Ok(false),
                Err(err) => return Err(to_io(err)),
            };
            let empty = handle.keys().map_err(to_io)?.next().is_none()
                && handle.values().map_err(to_io)?.next().is_none();
            drop(handle);
            if !empty {
                return Ok(false);
            }
            CURRENT_USER.remove_tree(self.path(key)).map_err(to_io)?;
            Ok(true)
        }
    }
}

/// An in-memory registry for tests. Names, as in Windows, are case-insensitive.
#[derive(Default)]
pub struct MemReg {
    keys: Mutex<BTreeMap<String, BTreeMap<String, RegVal>>>,
}

impl MemReg {
    fn lower(text: &str) -> String {
        text.to_lowercase()
    }

    /// All keys (for "nothing is left" checks).
    pub fn key_names(&self) -> Vec<String> {
        self.keys.lock().unwrap().keys().cloned().collect()
    }
}

impl RegStore for MemReg {
    fn get(&self, key: &str, name: &str) -> io::Result<Option<RegVal>> {
        Ok(self
            .keys
            .lock()
            .unwrap()
            .get(&Self::lower(key))
            .and_then(|values| values.get(&Self::lower(name)))
            .cloned())
    }

    fn set(&self, key: &str, name: &str, value: &RegVal) -> io::Result<()> {
        let mut keys = self.keys.lock().unwrap();
        // As in the registry: creating a key creates its parents too.
        let lower = Self::lower(key);
        let mut path = String::new();
        for part in lower.split('\\') {
            if !path.is_empty() {
                path.push('\\');
            }
            path.push_str(part);
            keys.entry(path.clone()).or_default();
        }
        keys.get_mut(&lower)
            .expect("ключ только что создан")
            .insert(Self::lower(name), value.clone());
        Ok(())
    }

    fn delete_value(&self, key: &str, name: &str) -> io::Result<()> {
        if let Some(values) = self.keys.lock().unwrap().get_mut(&Self::lower(key)) {
            values.remove(&Self::lower(name));
        }
        Ok(())
    }

    fn value_names(&self, key: &str) -> io::Result<Vec<String>> {
        Ok(self
            .keys
            .lock()
            .unwrap()
            .get(&Self::lower(key))
            .map(|values| values.keys().cloned().collect())
            .unwrap_or_default())
    }

    fn key_exists(&self, key: &str) -> bool {
        self.keys.lock().unwrap().contains_key(&Self::lower(key))
    }

    fn delete_key_if_empty(&self, key: &str) -> io::Result<bool> {
        let mut keys = self.keys.lock().unwrap();
        let lower = Self::lower(key);
        let child = format!("{lower}\\");
        let empty = keys.get(&lower).is_some_and(BTreeMap::is_empty)
            && !keys.keys().any(|name| name.starts_with(&child));
        if empty {
            keys.remove(&lower);
        }
        Ok(empty)
    }
}
