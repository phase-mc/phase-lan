//! WebView2: the installer window needs it even before the window is created.
//!
//! If the runtime is missing, with the user's consent Microsoft's Evergreen Bootstrapper is downloaded.
//! Before launch the Authenticode signature, the publisher and the Microsoft root are checked.

use std::{io, path::Path};

const CLIENT_GUID: &str = "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
/// Microsoft's permanent link to the Evergreen Bootstrapper.
pub const BOOTSTRAPPER_URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";
const PUBLISHER: &str = "Microsoft Corporation";

fn usable_version(version: Option<String>) -> bool {
    version.is_some_and(|version| !version.is_empty() && version != "0.0.0.0")
}

/// Whether the WebView2 runtime is installed (keys from Microsoft's documentation).
#[cfg(windows)]
pub fn is_installed() -> bool {
    use windows_registry::{CURRENT_USER, LOCAL_MACHINE};
    let machine = [
        format!(r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{CLIENT_GUID}"),
        format!(r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{CLIENT_GUID}"),
    ];
    let user = format!(r"Software\Microsoft\EdgeUpdate\Clients\{CLIENT_GUID}");
    machine.iter().any(|key| {
        usable_version(
            LOCAL_MACHINE
                .open(key)
                .and_then(|key| key.get_string("pv"))
                .ok(),
        )
    }) || usable_version(
        CURRENT_USER
            .open(&user)
            .and_then(|key| key.get_string("pv"))
            .ok(),
    )
}

#[cfg(not(windows))]
pub fn is_installed() -> bool {
    true
}

#[cfg(windows)]
pub use win::{download_bootstrapper, verify_microsoft_signature};

/// Downloads and launches the WebView2 installer, verifying the signature. The install window is its
/// own, so the user sees the progress and can cancel.
#[cfg(windows)]
pub fn install_runtime(temp: &Path) -> io::Result<()> {
    std::fs::create_dir_all(temp)?;
    let file = temp.join(format!(
        "MicrosoftEdgeWebview2Setup-{}.exe",
        std::process::id()
    ));
    let result = (|| {
        download_bootstrapper(&file)?;
        verify_microsoft_signature(&file)?;
        let status = std::process::Command::new(&file).arg("/install").status()?;
        if !status.success() {
            return Err(io::Error::other(format!("код завершения {status}")));
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&file);
    result
}

#[cfg(windows)]
mod win {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::{
        Win32::{
            Foundation::{HWND, INVALID_HANDLE_VALUE},
            Security::{
                Cryptography::{
                    CERT_CHAIN_POLICY_MICROSOFT_ROOT, CERT_CHAIN_POLICY_PARA,
                    CERT_CHAIN_POLICY_STATUS, CERT_NAME_SIMPLE_DISPLAY_TYPE, CertGetNameStringW,
                    CertVerifyCertificateChainPolicy,
                    MICROSOFT_ROOT_CERT_CHAIN_POLICY_CHECK_APPLICATION_ROOT_FLAG,
                },
                WinTrust::{
                    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0,
                    WINTRUST_FILE_INFO, WTD_CHOICE_FILE, WTD_REVOKE_WHOLECHAIN,
                    WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
                    WTHelperGetProvCertFromChain, WTHelperGetProvSignerFromChain,
                    WTHelperProvDataFromStateData, WinVerifyTrust,
                },
            },
        },
        core::GUID,
    };

    use super::*;

    #[link(name = "urlmon")]
    unsafe extern "system" {
        fn URLDownloadToFileW(
            caller: *mut core::ffi::c_void,
            url: *const u16,
            file: *const u16,
            reserved: u32,
            callback: *mut core::ffi::c_void,
        ) -> i32;
    }

    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain(Some(0)).collect()
    }

    pub fn download_bootstrapper(file: &Path) -> io::Result<()> {
        let url = wide(BOOTSTRAPPER_URL.as_ref());
        let target = wide(file.as_os_str());
        // SAFETY: the strings are NUL-terminated, the optional pointers are null.
        let status = unsafe {
            URLDownloadToFileW(
                std::ptr::null_mut(),
                url.as_ptr(),
                target.as_ptr(),
                0,
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            return Err(io::Error::other(format!("загрузка: 0x{status:08x}")));
        }
        Ok(())
    }

    /// The signature is valid, the publisher is Microsoft Corporation, the chain goes to a Microsoft root.
    pub fn verify_microsoft_signature(file: &Path) -> io::Result<()> {
        let path = wide(file.as_os_str());
        // SAFETY: the structures are filled per the WinVerifyTrust documentation and live until the end of
        // the function; the state is closed by a call with WTD_STATEACTION_CLOSE.
        unsafe {
            let mut file_info: WINTRUST_FILE_INFO = std::mem::zeroed();
            file_info.cbStruct = std::mem::size_of::<WINTRUST_FILE_INFO>() as u32;
            file_info.pcwszFilePath = path.as_ptr();

            let mut data: WINTRUST_DATA = std::mem::zeroed();
            data.cbStruct = std::mem::size_of::<WINTRUST_DATA>() as u32;
            data.dwUIChoice = WTD_UI_NONE;
            data.fdwRevocationChecks = WTD_REVOKE_WHOLECHAIN;
            data.dwUnionChoice = WTD_CHOICE_FILE;
            data.dwStateAction = WTD_STATEACTION_VERIFY;
            data.Anonymous = WINTRUST_DATA_0 {
                pFile: &mut file_info,
            };

            let mut action: GUID = WINTRUST_ACTION_GENERIC_VERIFY_V2;
            let window = INVALID_HANDLE_VALUE as HWND;
            let status = WinVerifyTrust(
                window,
                &mut action,
                (&mut data as *mut WINTRUST_DATA).cast(),
            );
            let result = if status != 0 {
                Err(io::Error::other(format!("подпись: 0x{status:08x}")))
            } else {
                check_signer(data.hWVTStateData)
            };
            data.dwStateAction = WTD_STATEACTION_CLOSE;
            WinVerifyTrust(
                window,
                &mut action,
                (&mut data as *mut WINTRUST_DATA).cast(),
            );
            result
        }
    }

    unsafe fn check_signer(state: windows_sys::Win32::Foundation::HANDLE) -> io::Result<()> {
        let invalid = |what: &str| io::Error::other(format!("подпись: {what}"));
        // SAFETY: `state` was obtained from a successful WinVerifyTrust; the pointers are checked.
        unsafe {
            let provider = WTHelperProvDataFromStateData(state);
            if provider.is_null() {
                return Err(invalid("нет данных"));
            }
            let signer = WTHelperGetProvSignerFromChain(provider, 0, 0, 0);
            if signer.is_null() {
                return Err(invalid("нет подписанта"));
            }
            let certificate = WTHelperGetProvCertFromChain(signer, 0);
            if certificate.is_null() {
                return Err(invalid("нет сертификата"));
            }
            let mut name = [0u16; 256];
            let len = CertGetNameStringW(
                (*certificate).pCert,
                CERT_NAME_SIMPLE_DISPLAY_TYPE,
                0,
                std::ptr::null(),
                name.as_mut_ptr(),
                name.len() as u32,
            );
            let subject = String::from_utf16_lossy(&name[..(len as usize).saturating_sub(1)]);
            if subject != PUBLISHER {
                return Err(invalid(&format!("издатель {subject}")));
            }

            let mut para: CERT_CHAIN_POLICY_PARA = std::mem::zeroed();
            para.cbSize = std::mem::size_of::<CERT_CHAIN_POLICY_PARA>() as u32;
            // Microsoft programs are signed from the apps root (2011 and newer):
            // without this flag the policy accepts only the old roots of Windows itself.
            para.dwFlags = MICROSOFT_ROOT_CERT_CHAIN_POLICY_CHECK_APPLICATION_ROOT_FLAG;
            let mut policy: CERT_CHAIN_POLICY_STATUS = std::mem::zeroed();
            policy.cbSize = std::mem::size_of::<CERT_CHAIN_POLICY_STATUS>() as u32;
            let ok = CertVerifyCertificateChainPolicy(
                CERT_CHAIN_POLICY_MICROSOFT_ROOT,
                (*signer).pChainContext,
                &para,
                &mut policy,
            );
            if ok == 0 || policy.dwError != 0 {
                return Err(invalid("корень не Microsoft"));
            }
        }
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsigned_files() {
        let exe = std::env::current_exe().unwrap();
        assert!(verify_microsoft_signature(&exe).is_err());
    }

    #[test]
    fn accepts_a_microsoft_signed_file_when_present() {
        // WebView2 itself is signed by Microsoft and is present on developers' machines.
        let root = Path::new(r"C:\Program Files (x86)\Microsoft\EdgeWebView\Application");
        let Some(file) = std::fs::read_dir(root).ok().and_then(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path().join("msedgewebview2.exe"))
                .find(|path| path.is_file())
        }) else {
            return;
        };
        verify_microsoft_signature(&file).unwrap();
    }
}
