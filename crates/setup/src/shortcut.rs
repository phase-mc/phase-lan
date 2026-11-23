//! `.lnk` shortcuts: the same ones the NSIS installer creates, with the Phase window's AppUserModelID
//! (without it Windows won't link notifications and the taskbar icon to the shortcut).

use std::{
    io,
    path::{Path, PathBuf},
};

#[cfg(windows)]
pub use win::{create, target_of};

/// A shortcut can be considered ours: it points to our `Phase.exe`.
pub fn points_to(link: &Path, exe: &Path) -> bool {
    target_of(link).is_ok_and(|target| crate::paths::same_path(&target, exe))
}

#[cfg(not(windows))]
pub fn create(_link: &Path, _exe: &Path, _id: &str) -> io::Result<()> {
    Err(io::Error::other("только Windows"))
}

#[cfg(not(windows))]
pub fn target_of(_link: &Path) -> io::Result<PathBuf> {
    Err(io::Error::other("только Windows"))
}

#[cfg(windows)]
mod win {
    use std::{
        ffi::OsString,
        os::windows::ffi::{OsStrExt, OsStringExt},
    };

    use windows::{
        Win32::{
            Foundation::{MAX_PATH, RPC_E_CHANGED_MODE},
            Storage::EnhancedStorage::PKEY_AppUserModel_ID,
            System::{
                Com::{
                    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
                    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, STGM_READ,
                    StructuredStorage::PROPVARIANT,
                },
                Variant::VT_LPWSTR,
            },
            UI::Shell::{
                IShellLinkW, PropertiesSystem::IPropertyStore, SHStrDupW, SLGP_RAWPATH, ShellLink,
            },
        },
        core::{HSTRING, Interface, PCWSTR},
    };

    use super::*;

    fn to_io(err: windows::core::Error) -> io::Error {
        io::Error::other(err.message())
    }

    /// COM for the duration of the operation. If COM already runs in the thread with another mode — we leave it be.
    struct Com(bool);

    impl Com {
        fn init() -> io::Result<Self> {
            // SAFETY: the paired CoUninitialize call is in Drop.
            let status =
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
            if status == RPC_E_CHANGED_MODE {
                return Ok(Self(false));
            }
            status.ok().map_err(to_io)?;
            Ok(Self(true))
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: initialization in this same thread succeeded.
                unsafe { CoUninitialize() };
            }
        }
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    /// Creates or replaces a shortcut.
    pub fn create(link: &Path, exe: &Path, app_id: &str) -> io::Result<()> {
        let _com = Com::init()?;
        let exe_wide = wide(exe);
        let dir_wide = wide(exe.parent().unwrap_or(exe));
        let link_wide = wide(link);
        // SAFETY: the strings live to the end of the block and are NUL-terminated; the PROPVARIANT is cleared.
        unsafe {
            let shell: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(to_io)?;
            shell.SetPath(PCWSTR(exe_wide.as_ptr())).map_err(to_io)?;
            shell
                .SetWorkingDirectory(PCWSTR(dir_wide.as_ptr()))
                .map_err(to_io)?;
            shell
                .SetIconLocation(PCWSTR(exe_wide.as_ptr()), 0)
                .map_err(to_io)?;

            // The value is VT_LPWSTR; the string is freed by the PROPVARIANT destructor (CoTaskMemFree).
            let store: IPropertyStore = shell.cast().map_err(to_io)?;
            let id = HSTRING::from(app_id);
            let mut value = PROPVARIANT::default();
            let inner = &mut *value.Anonymous.Anonymous;
            inner.Anonymous.pwszVal = SHStrDupW(PCWSTR(id.as_ptr())).map_err(to_io)?;
            inner.vt = VT_LPWSTR;
            store
                .SetValue(&PKEY_AppUserModel_ID, &value)
                .map_err(to_io)?;
            store.Commit().map_err(to_io)?;

            let file: IPersistFile = shell.cast().map_err(to_io)?;
            file.Save(PCWSTR(link_wide.as_ptr()), true).map_err(to_io)
        }
    }

    /// The path the shortcut points to.
    pub fn target_of(link: &Path) -> io::Result<PathBuf> {
        let _com = Com::init()?;
        let link_wide = wide(link);
        // SAFETY: the buffer has the stated size, the path string is NUL-terminated.
        unsafe {
            let shell: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(to_io)?;
            let file: IPersistFile = shell.cast().map_err(to_io)?;
            file.Load(PCWSTR(link_wide.as_ptr()), STGM_READ)
                .map_err(to_io)?;
            let mut buffer = [0u16; MAX_PATH as usize * 2];
            shell
                .GetPath(&mut buffer, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)
                .map_err(to_io)?;
            let len = buffer.iter().position(|&c| c == 0).unwrap_or(0);
            Ok(PathBuf::from(OsString::from_wide(&buffer[..len])))
        }
    }
}
