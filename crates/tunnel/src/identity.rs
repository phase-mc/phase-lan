//! The persistent device key.
//!
//! The public half of the key is the device ID (`EndpointId`). The host uses it to decide
//! whom to let in. The key is created once and stored in the profile folder. So the ID does not
//! change between runs, and a friend approved once connects without questions.
//!
//! **Key protection (Windows):** the `device.key` file is encrypted by Windows (DPAPI,
//! `CryptProtectData`). Only the same Windows account on the same PC can decrypt it:
//! a file copied to another computer or read by another user
//! is useless. No password is needed: Windows handles it at sign-in.
//!
//! File format: the [`MAGIC`] tag + DPAPI-encrypted data. The old format (32 bytes
//! of plaintext key, versions before encryption) is automatically rewritten as encrypted
//! on the first run; the device ID does not change.
//!
//! Honest caveat: rewriting does not guarantee the old bytes are physically erased from
//! the disk (the file system and SSDs can keep old blocks). To be fully sure,
//! create a new key (delete the profile); then friends will have to approve the
//! device again.

use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use iroh::SecretKey;
use tracing::{info, warn};
use zeroize::Zeroizing;

use crate::fsutil;

const KEY_FILE: &str = "device.key";
/// Tag of the encrypted key file.
pub const MAGIC: &[u8; 8] = b"LBDPAPI1";
/// Ed25519 key length.
const KEY_LEN: usize = 32;

/// Profile folder: `%LOCALAPPDATA%\Phase\profiles\<profile>`.
///
/// Profiles let a host and a guest with different keys run on one PC
/// (test T1).
pub fn profile_dir(profile: &str) -> Result<PathBuf> {
    if !is_valid_profile_name(profile) {
        bail!(phase_lang::tr!(
            "имя профиля может содержать только латинские буквы, цифры, '-' и '_' (до 32 символов)",
            "a profile name may contain only Latin letters, digits, '-' and '_' (up to 32 characters)"
        ));
    }
    Ok(profiles_root()?.join(profile))
}

/// A valid profile name: Latin letters, digits, `-`, `_`, up to 32 characters.
pub fn is_valid_profile_name(profile: &str) -> bool {
    !profile.is_empty()
        && profile.len() <= 32
        && profile
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Name of the program's data folder.
const DATA_DIR_NAME: &str = "Phase";
/// Name of the data folder before the program was renamed (LanBridge).
const LEGACY_DATA_DIR_NAME: &str = "LanBridge";

/// The program's data folder: `%LOCALAPPDATA%\Phase`.
///
/// Migration from the old name: if `Phase` does not exist yet but `LanBridge` does, the folder
/// is renamed as a whole. The device key is DPAPI-encrypted for the Windows user,
/// not for the path, so the device ID and approved friends are kept.
pub fn data_dir() -> Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .context(phase_lang::tr!(
            "не найдена папка данных пользователя (LOCALAPPDATA)",
            "the user data folder (LOCALAPPDATA) was not found"
        ))?;
    Ok(migrated_data_dir(&base))
}

/// The data folder inside `base`, migrated from the old one (see [`data_dir`]).
///
/// If the rename fails (an old program version is still open and holds files),
/// we work in the old folder: the device key matters more than the folder name. The migration is retried
/// on the next run.
fn migrated_data_dir(base: &Path) -> PathBuf {
    let dir = base.join(DATA_DIR_NAME);
    let legacy = base.join(LEGACY_DATA_DIR_NAME);
    if dir.exists() || !legacy.is_dir() {
        return dir;
    }
    match fs::rename(&legacy, &dir) {
        Ok(()) => {
            info!(
                "папка данных перенесена: {} → {}",
                legacy.display(),
                dir.display()
            );
            dir
        }
        // Another program instance migrated the folder before us.
        Err(_) if dir.exists() => dir,
        Err(err) => {
            warn!(
                "не удалось перенести {} в {}: {err}; пока работаем в старой папке",
                legacy.display(),
                dir.display()
            );
            legacy
        }
    }
}

/// Folder of all profiles: `%LOCALAPPDATA%\Phase\profiles`.
pub fn profiles_root() -> Result<PathBuf> {
    Ok(data_dir()?.join("profiles"))
}

/// Names of existing profiles (folders with a valid name), alphabetically.
pub fn list_profiles() -> Result<Vec<String>> {
    let root = profiles_root()?;
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(err).with_context(|| {
                phase_lang::tr!(
                    format!("не удалось прочитать {}", root.display()),
                    format!("could not read {}", root.display())
                )
            });
        }
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| is_valid_profile_name(name))
        .collect();
    names.sort();
    Ok(names)
}

/// The device key from the profile folder, **without creating** a new one (`None` means no key yet).
pub fn load_secret_key(dir: &Path) -> Result<Option<SecretKey>> {
    read_key(&dir.join(KEY_FILE))
}

/// Loads the device key from the profile folder or creates a new one.
///
/// - A new key is written encrypted and atomically right away: an interrupted write does not leave
///   a corrupted file, and when two program instances start for the first time at once, both
///   get the same key.
/// - An old plaintext key is rewritten in encrypted form (the ID does not change).
/// - A corrupted or foreign key file is not silently deleted: the program reports an error.
pub fn load_or_create_secret_key(dir: &Path) -> Result<SecretKey> {
    let path = dir.join(KEY_FILE);
    if let Some(key) = read_key(&path)? {
        return Ok(key);
    }
    let key = SecretKey::generate();
    let plain = Zeroizing::new(key.to_bytes());
    if fsutil::create_new_atomic(&path, &seal_new(plain.as_slice()))? {
        return Ok(key);
    }
    // Another run created the file first, so use its key.
    read_key(&path)?.with_context(|| {
        phase_lang::tr!(
            format!("файл ключа пропал: {}", path.display()),
            format!("the key file disappeared: {}", path.display())
        )
    })
}

fn read_key(path: &Path) -> Result<Option<SecretKey>> {
    let data = match fs::read(path) {
        Ok(data) => Zeroizing::new(data),
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| {
                phase_lang::tr!(
                    format!("не удалось прочитать {}", path.display()),
                    format!("could not read {}", path.display())
                )
            });
        }
    };
    if let Some(sealed) = data.strip_prefix(MAGIC.as_slice()) {
        let plain = unseal(sealed).with_context(|| {
            phase_lang::tr!(
                format!(
                    "не удалось расшифровать ключ устройства {}. Возможные причины: программа \
                     запущена не из вашей обычной учётной записи Windows (другой пользователь, \
                     служба, планировщик задач), файл скопирован с другого ПК или повреждён. \
                     Сначала попробуйте запустить программу обычным способом из своей учётной \
                     записи. Только если это не помогло — удалите этот файл: будет создан новый ID, \
                     и друзьям придётся одобрить устройство заново",
                    path.display()
                ),
                format!(
                    "could not decrypt the device key {}. Possible causes: the program is not \
                     running from your usual Windows account (another user, a service, Task \
                     Scheduler), or the file was copied from another PC or is damaged. First try \
                     starting the program normally from your own account. Only if that does not \
                     help, delete this file: a new ID will be created and your friends will have \
                     to approve the device again",
                    path.display()
                )
            )
        })?;
        return Ok(Some(key_from_bytes(&plain, path)?));
    }
    if data.len() == KEY_LEN {
        // Old format: plaintext key. Rewrite it encrypted.
        let key = key_from_bytes(&data, path)?;
        if cfg!(windows) {
            migrate(path, &data);
        }
        return Ok(Some(key));
    }
    bail!(phase_lang::tr!(
        format!("файл ключа повреждён: {}", path.display()),
        format!("the key file is damaged: {}", path.display())
    ))
}

/// Converting a plaintext key to encrypted form. A failure does not get in the way: the key is already
/// read, the program continues with it, and the conversion is retried on the next run.
/// The file is replaced only if the new encrypted copy decrypts back
/// to the same key, so the only copy of the key cannot get corrupted.
fn migrate(path: &Path, plain: &[u8]) {
    let result = (|| -> Result<()> {
        let sealed = seal(plain)?;
        let check = unseal(&sealed[MAGIC.len()..])?;
        if check.as_slice() != plain {
            bail!(phase_lang::tr!(
                "проверка зашифрованной копии не прошла",
                "the encrypted copy failed verification"
            ));
        }
        fsutil::write_atomic(path, &sealed)
    })();
    match result {
        Ok(()) => info!("ключ устройства зашифрован средствами Windows (DPAPI); ID не изменился"),
        Err(err) => warn!(
            "ключ устройства пока хранится незашифрованным: {err:#}. Программа работает как \
             обычно, шифрование будет повторено при следующем запуске"
        ),
    }
}

/// Contents of a new key file. If Windows could not encrypt it (for example,
/// the program runs without a loaded user profile), the key is saved as is
/// and will be encrypted on the next run: better than not starting at all.
fn seal_new(plain: &[u8]) -> Vec<u8> {
    match seal(plain) {
        Ok(sealed) => sealed,
        Err(err) => {
            warn!("не удалось зашифровать новый ключ устройства: {err:#}. Сохраняю без шифрования");
            plain.to_vec()
        }
    }
}

fn key_from_bytes(bytes: &[u8], path: &Path) -> Result<SecretKey> {
    let bytes: Zeroizing<[u8; KEY_LEN]> = Zeroizing::new(bytes.try_into().map_err(|_| {
        anyhow::anyhow!(phase_lang::tr!(
            format!("файл ключа повреждён: {}", path.display()),
            format!("the key file is damaged: {}", path.display())
        ))
    })?);
    Ok(SecretKey::from_bytes(&bytes))
}

/// Key file contents: tag + encrypted key (on other OSes, the key as is).
fn seal(plain: &[u8]) -> Result<Vec<u8>> {
    if !cfg!(windows) {
        return Ok(plain.to_vec());
    }
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&dpapi::protect(plain)?);
    Ok(out)
}

fn unseal(sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    dpapi::unprotect(sealed)
}

/// Data encryption by Windows (DPAPI), bound to the user account.
#[cfg(windows)]
pub mod dpapi {
    use anyhow::{Result, bail};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    use zeroize::{Zeroize, Zeroizing};

    /// An extra "salt" (entropy) parameter: data encrypted by another program under the same
    /// account will not pass as our key, and vice versa.
    /// The old program name is kept on purpose: with a different salt, keys created before
    /// the rename to Phase would stop decrypting and the device ID would change.
    const ENTROPY: &[u8] = b"LanBridge device key v1";

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr().cast_mut(),
        }
    }

    /// Takes the result allocated by Windows and frees its memory.
    /// `wipe` means wiping the memory before freeing it (it holds decrypted data).
    ///
    /// # Safety
    /// `out` must be filled by a successful CryptProtectData/CryptUnprotectData call.
    unsafe fn take_output(out: CRYPT_INTEGER_BLOB, wipe: bool) -> Vec<u8> {
        if out.pbData.is_null() {
            return Vec::new();
        }
        // SAFETY: Windows allocated `cbData` bytes at `pbData`.
        let slice = unsafe { std::slice::from_raw_parts_mut(out.pbData, out.cbData as usize) };
        let data = slice.to_vec();
        if wipe {
            slice.zeroize();
        }
        // SAFETY: the memory was allocated by Windows via LocalAlloc, so free it with LocalFree.
        unsafe { LocalFree(out.pbData.cast()) };
        data
    }

    pub fn protect(plain: &[u8]) -> Result<Vec<u8>> {
        protect_with(plain, Some(ENTROPY))
    }

    pub fn unprotect(sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        unprotect_with(sealed, Some(ENTROPY))
    }

    /// Pointer to the DPAPI salt (`None` means none).
    fn entropy_ptr(blob: &Option<CRYPT_INTEGER_BLOB>) -> *const CRYPT_INTEGER_BLOB {
        blob.as_ref().map_or(std::ptr::null(), |b| b as *const _)
    }

    pub(super) fn protect_with(plain: &[u8], entropy: Option<&[u8]>) -> Result<Vec<u8>> {
        let input = blob(plain);
        let entropy = entropy.map(blob);
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: the input structs point to live slices; Windows fills the output.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                entropy_ptr(&entropy),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            let os_error = std::io::Error::last_os_error();
            bail!(phase_lang::tr!(
                format!("Windows не смогла зашифровать ключ (DPAPI): {os_error}"),
                format!("Windows could not encrypt the key (DPAPI): {os_error}")
            ));
        }
        // SAFETY: the call succeeded.
        Ok(unsafe { take_output(out, false) })
    }

    pub(super) fn unprotect_with(
        sealed: &[u8],
        entropy: Option<&[u8]>,
    ) -> Result<Zeroizing<Vec<u8>>> {
        let input = blob(sealed);
        let entropy = entropy.map(blob);
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: the input structs point to live slices; Windows fills the output.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                entropy_ptr(&entropy),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            bail!("DPAPI: {}", std::io::Error::last_os_error());
        }
        // SAFETY: the call succeeded.
        Ok(Zeroizing::new(unsafe { take_output(out, true) }))
    }
}

/// Other OSes have no DPAPI: the key is stored in plaintext (the program targets Windows).
#[cfg(not(windows))]
pub mod dpapi {
    use anyhow::{Result, bail};
    use zeroize::Zeroizing;

    pub fn protect(_plain: &[u8]) -> Result<Vec<u8>> {
        bail!(phase_lang::tr!(
            "шифрование ключа доступно только в Windows",
            "key encryption is only available on Windows"
        ))
    }

    pub fn unprotect(_sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        bail!(phase_lang::tr!(
            "шифрование ключа доступно только в Windows",
            "key encryption is only available on Windows"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("phase-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[cfg(windows)]
    #[test]
    fn key_is_persistent_and_encrypted() {
        let dir = temp_dir("key");
        let first = load_or_create_secret_key(&dir).unwrap();
        let second = load_or_create_secret_key(&dir).unwrap();
        assert_eq!(first.public(), second.public());

        let file = fs::read(dir.join(KEY_FILE)).unwrap();
        assert!(file.starts_with(MAGIC), "the file must be encrypted");
        let secret = first.to_bytes();
        assert!(
            !file.windows(KEY_LEN).any(|w| w == secret),
            "the file must not contain the plaintext key"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn failed_migration_keeps_working_with_old_key() {
        let dir = temp_dir("migrate-fail");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(KEY_FILE);
        let old = SecretKey::generate();
        fs::write(&path, old.to_bytes()).unwrap();
        // A read-only file: it cannot be replaced (as when an antivirus locks it).
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&path, perms.clone()).unwrap();

        let loaded = load_or_create_secret_key(&dir).expect("the program must start");
        assert_eq!(loaded.public(), old.public());
        assert_eq!(
            fs::read(&path).unwrap(),
            old.to_bytes(),
            "the key is not corrupted"
        );

        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(&path, perms).unwrap();
        // Once the obstacle is gone, the conversion succeeds on the next run.
        let again = load_or_create_secret_key(&dir).unwrap();
        assert_eq!(again.public(), old.public());
        assert!(fs::read(&path).unwrap().starts_with(MAGIC));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn old_plain_key_is_migrated_without_changing_id() {
        let dir = temp_dir("migrate");
        fs::create_dir_all(&dir).unwrap();
        let old = SecretKey::generate();
        fs::write(dir.join(KEY_FILE), old.to_bytes()).unwrap();

        let loaded = load_or_create_secret_key(&dir).unwrap();
        assert_eq!(loaded.public(), old.public(), "the ID must not change");
        let file = fs::read(dir.join(KEY_FILE)).unwrap();
        assert!(
            file.starts_with(MAGIC),
            "the old key must be rewritten encrypted"
        );
        assert!(!file.windows(KEY_LEN).any(|w| w == old.to_bytes()));

        // And after the conversion the key reads as usual.
        let again = load_or_create_secret_key(&dir).unwrap();
        assert_eq!(again.public(), old.public());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn damaged_or_foreign_key_is_an_error_not_a_new_id() {
        let dir = temp_dir("damaged");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(KEY_FILE);

        fs::write(&path, b"short").unwrap();
        assert!(load_or_create_secret_key(&dir).is_err());

        // Encrypted data Windows cannot decrypt (like a file from another PC).
        let mut foreign = MAGIC.to_vec();
        foreign.extend_from_slice(&[0x01, 0x00, 0x00, 0x00, 0xde, 0xad, 0xbe, 0xef]);
        fs::write(&path, &foreign).unwrap();
        let err = load_or_create_secret_key(&dir).unwrap_err();
        assert!(format!("{err:#}").contains("удалите этот файл"), "{err:#}");
        assert_eq!(
            fs::read(&path).unwrap(),
            foreign,
            "the file must not change"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_roundtrip_and_entropy() {
        let sealed = dpapi::protect(b"secret data").unwrap();
        assert_ne!(sealed.as_slice(), b"secret data");
        assert_eq!(
            dpapi::unprotect(&sealed).unwrap().as_slice(),
            b"secret data"
        );
        // Corrupted data does not decrypt.
        let mut broken = sealed.clone();
        let last = broken.len() - 1;
        broken[last] ^= 0xFF;
        assert!(dpapi::unprotect(&broken).is_err());
        // The salt really takes part: without it or with someone else's, it does not decrypt,
        // and vice versa, another program's data (without our salt) does not fit us.
        assert!(dpapi::unprotect_with(&sealed, None).is_err());
        assert!(dpapi::unprotect_with(&sealed, Some(b"other program")).is_err());
        let foreign = dpapi::protect_with(b"secret data", None).unwrap();
        assert!(dpapi::unprotect(&foreign).is_err());
    }

    #[test]
    fn data_dir_moves_from_old_name_once() {
        let base = temp_dir("data-dir");
        fs::create_dir_all(base.join(LEGACY_DATA_DIR_NAME).join("profiles/default")).unwrap();
        fs::write(
            base.join(LEGACY_DATA_DIR_NAME)
                .join("profiles/default/device.key"),
            b"k",
        )
        .unwrap();

        let dir = migrated_data_dir(&base);
        assert_eq!(dir, base.join(DATA_DIR_NAME));
        assert_eq!(
            fs::read(dir.join("profiles/default/device.key")).unwrap(),
            b"k"
        );
        assert!(
            !base.join(LEGACY_DATA_DIR_NAME).exists(),
            "the old folder was renamed"
        );

        // The new folder already exists: an old folder that reappears (an old program version)
        // is left alone and does not replace the new one.
        fs::create_dir_all(base.join(LEGACY_DATA_DIR_NAME)).unwrap();
        assert_eq!(migrated_data_dir(&base), base.join(DATA_DIR_NAME));
        assert!(base.join(LEGACY_DATA_DIR_NAME).exists());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn data_dir_without_old_folder_is_new_name() {
        let base = temp_dir("data-dir-fresh");
        assert_eq!(migrated_data_dir(&base), base.join(DATA_DIR_NAME));
        assert!(
            !base.join(DATA_DIR_NAME).exists(),
            "the folder is created by those who write to it"
        );
    }

    #[test]
    fn rejects_bad_profile_names() {
        assert!(profile_dir("host").is_ok());
        assert!(profile_dir("../evil").is_err());
        assert!(profile_dir("").is_err());
    }
}
