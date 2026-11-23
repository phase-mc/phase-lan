//! Phase's traces in Windows: registry keys in the same places and the same form as the NSIS installer
//! and the Phase window itself (the autostart and deep-link plugins) use.

use std::{io, path::Path};

use crate::{
    APP_EXE, APP_IDENTIFIER, AUTOSTART_FLAG, PRODUCT, PUBLISHER, UNINSTALL_EXE,
    env::SetupEnv,
    paths,
    reg::{RegStore, RegVal},
    tx::Tx,
};

pub const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Phase";
/// The publisher key. The [`LANGUAGE_VALUE`] value is read by the Phase window on first launch.
pub const VENDOR_KEY: &str = r"Software\Phase";
/// The key where NSIS stores the install folder (the default value).
pub const PRODUCT_KEY: &str = r"Software\Phase\Phase";
/// The language chosen in the installer: `ru` or `en`.
pub const LANGUAGE_VALUE: &str = "Language";
/// The language of the NSIS installer window itself.
pub const NSIS_LANGUAGE_VALUE: &str = "Installer Language";
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const STARTUP_APPROVED_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
pub const CLASSES_KEY: &str = r"Software\Classes\phase";
pub const CLASSES_ICON_KEY: &str = r"Software\Classes\phase\DefaultIcon";
pub const CLASSES_COMMAND_KEY: &str = r"Software\Classes\phase\shell\open\command";
/// The "autostart allowed" value in Task Manager — as in the autostart plugin.
const STARTUP_ENABLED: [u8; 12] = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// A registry string without surrounding quotes (NSIS writes `InstallLocation` in quotes).
pub fn unquoted(text: &str) -> &str {
    text.trim().trim_matches('"')
}

/// The autostart command in the autostart plugin's form: `<exe> --autostart`. A path with spaces
/// is quoted: that way Windows doesn't try variants of parsing the string.
pub fn autostart_command(exe: &Path) -> String {
    let path = exe.display().to_string();
    if path.contains(' ') {
        format!("\"{path}\" {AUTOSTART_FLAG}")
    } else {
        format!("{path} {AUTOSTART_FLAG}")
    }
}

/// The autostart entry points to this `exe`.
pub fn autostart_points_to(command: &str, exe: &Path) -> bool {
    let command = command.trim();
    let path = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(""),
        None => command
            .strip_suffix(AUTOSTART_FLAG)
            .unwrap_or(command)
            .trim_end(),
    };
    paths::same_path(Path::new(path), exe)
}

pub fn deep_link_command(exe: &Path) -> String {
    format!("\"{}\" \"%1\"", exe.display())
}

/// Whether autostart of exactly this `exe` is on — what the switch in Phase will show.
pub fn autostart_enabled(reg: &dyn RegStore, exe: &Path) -> bool {
    let registered = reg
        .get_str(RUN_KEY, PRODUCT)
        .is_some_and(|command| autostart_points_to(&command, exe));
    // Task Manager keeps the disable flag in the last eight bytes (as the plugin reads it).
    let allowed = match reg.get(STARTUP_APPROVED_KEY, PRODUCT) {
        Ok(Some(RegVal::Bin(bytes))) if bytes.len() >= 8 => {
            bytes[bytes.len() - 8..].iter().all(|byte| *byte == 0)
        }
        _ => true,
    };
    registered && allowed
}

/// What to write to "Installed apps" and next to it.
pub struct Registration<'a> {
    pub dir: &'a Path,
    pub version: &'a str,
    pub size_bytes: u64,
    pub autostart: bool,
}

/// Writes all values through the journal: a rollback restores the previous ones.
pub fn register(env: &SetupEnv, tx: &mut Tx<'_>, info: &Registration<'_>) -> io::Result<()> {
    let exe = info.dir.join(APP_EXE);
    let uninstaller = info.dir.join(UNINSTALL_EXE);
    let text = |value: String| RegVal::Str(value);
    let entries = [
        ("MainBinaryName", text(APP_EXE.to_string())),
        ("DisplayName", text(PRODUCT.to_string())),
        ("DisplayIcon", text(format!("\"{}\"", exe.display()))),
        ("DisplayVersion", text(info.version.to_string())),
        ("Publisher", text(PUBLISHER.to_string())),
        (
            "InstallLocation",
            text(format!("\"{}\"", info.dir.display())),
        ),
        (
            "UninstallString",
            text(format!("\"{}\" --uninstall", uninstaller.display())),
        ),
        (
            "QuietUninstallString",
            text(format!("\"{}\" --uninstall /S", uninstaller.display())),
        ),
        ("NoModify", RegVal::Dword(1)),
        ("NoRepair", RegVal::Dword(1)),
        (
            "EstimatedSize",
            RegVal::Dword(u32::try_from(info.size_bytes / 1024).unwrap_or(u32::MAX)),
        ),
    ];
    for (name, value) in entries {
        tx.set_value(UNINSTALL_KEY, name, value)?;
    }
    tx.set_value(PRODUCT_KEY, "", text(info.dir.display().to_string()))?;

    // phase:// links — as in the deep-link plugin: the window considers such an entry its own.
    tx.set_value(
        CLASSES_KEY,
        "",
        text(format!("URL:{APP_IDENTIFIER} protocol")),
    )?;
    tx.set_value(CLASSES_KEY, "URL Protocol", text(String::new()))?;
    tx.set_value(CLASSES_ICON_KEY, "", text(format!("{},0", exe.display())))?;
    tx.set_value(CLASSES_COMMAND_KEY, "", text(deep_link_command(&exe)))?;

    let reg = env.reg.clone();
    if info.autostart {
        tx.set_value(RUN_KEY, PRODUCT, text(autostart_command(&exe)))?;
        // Like the plugin: we use the Task Manager key only if it already exists.
        if reg.key_exists(STARTUP_APPROVED_KEY) {
            tx.set_value(
                STARTUP_APPROVED_KEY,
                PRODUCT,
                RegVal::Bin(STARTUP_ENABLED.to_vec()),
            )?;
        }
    } else if reg
        .get_str(RUN_KEY, PRODUCT)
        .is_some_and(|command| autostart_points_to(&command, &exe))
    {
        // We turn off only our own entry; another Phase copy's autostart is left alone.
        tx.delete_value(RUN_KEY, PRODUCT)?;
        tx.delete_value(STARTUP_APPROVED_KEY, PRODUCT)?;
    }
    Ok(())
}

/// Removes the installation's entries from this folder. Others' (another Phase copy) remain.
pub fn unregister(
    env: &SetupEnv,
    tx: &mut Tx<'_>,
    dir: &Path,
    delete_data: bool,
    language_created: bool,
) -> io::Result<()> {
    let reg = env.reg.clone();
    let exe = dir.join(APP_EXE);

    let ours = reg
        .get_str(UNINSTALL_KEY, "InstallLocation")
        .is_some_and(|location| paths::same_path(Path::new(unquoted(&location)), dir));
    if ours {
        for name in reg.value_names(UNINSTALL_KEY)? {
            tx.delete_value(UNINSTALL_KEY, &name)?;
        }
    }
    if reg
        .get_str(PRODUCT_KEY, "")
        .is_some_and(|location| paths::same_path(Path::new(unquoted(&location)), dir))
    {
        tx.delete_value(PRODUCT_KEY, "")?;
    }
    if reg.get_str(CLASSES_COMMAND_KEY, "").as_deref() == Some(&deep_link_command(&exe)) {
        tx.delete_value(CLASSES_COMMAND_KEY, "")?;
        tx.delete_value(CLASSES_ICON_KEY, "")?;
        tx.delete_value(CLASSES_KEY, "")?;
        tx.delete_value(CLASSES_KEY, "URL Protocol")?;
    }
    if reg
        .get_str(RUN_KEY, PRODUCT)
        .is_some_and(|command| autostart_points_to(&command, &exe))
    {
        tx.delete_value(RUN_KEY, PRODUCT)?;
        tx.delete_value(STARTUP_APPROVED_KEY, PRODUCT)?;
    }
    if delete_data {
        tx.delete_value(PRODUCT_KEY, NSIS_LANGUAGE_VALUE)?;
        // We remove the language choice only if the installer created it.
        if language_created {
            tx.delete_value(VENDOR_KEY, LANGUAGE_VALUE)?;
        }
    }
    Ok(())
}

/// After a successful uninstall: empty keys are no longer needed.
pub fn remove_empty_keys(reg: &dyn RegStore) {
    for key in [
        UNINSTALL_KEY,
        CLASSES_COMMAND_KEY,
        r"Software\Classes\phase\shell\open",
        r"Software\Classes\phase\shell",
        CLASSES_ICON_KEY,
        CLASSES_KEY,
        PRODUCT_KEY,
        VENDOR_KEY,
    ] {
        let _ = reg.delete_key_if_empty(key);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn autostart_command_matches_the_app_form() {
        let plain = PathBuf::from(r"C:\Phase\Phase.exe");
        assert_eq!(autostart_command(&plain), r"C:\Phase\Phase.exe --autostart");
        assert!(autostart_points_to(
            r"C:\Phase\Phase.exe --autostart",
            &plain
        ));
        assert!(autostart_points_to(
            r"c:\phase\PHASE.EXE --autostart",
            &plain
        ));
        assert!(!autostart_points_to(
            r"D:\Other\Phase.exe --autostart",
            &plain
        ));

        let spaced = PathBuf::from(r"C:\Users\A B\Phase\Phase.exe");
        let command = autostart_command(&spaced);
        assert_eq!(command, r#""C:\Users\A B\Phase\Phase.exe" --autostart"#);
        assert!(autostart_points_to(&command, &spaced));
        // The plugin's own form: no quotes even with spaces.
        assert!(autostart_points_to(
            r"C:\Users\A B\Phase\Phase.exe --autostart",
            &spaced
        ));
    }

    #[test]
    fn deep_link_matches_the_plugin_form() {
        assert_eq!(
            deep_link_command(Path::new(r"C:\P\Phase.exe")),
            r#""C:\P\Phase.exe" "%1""#
        );
    }
}
