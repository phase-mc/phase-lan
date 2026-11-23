//! Command-line parsing.
//!
//! ```text
//! Phase-Setup.exe                     install window
//! Phase-Setup.exe /S [/D=<folder>]    silent install or update
//! uninstall.exe [--uninstall]         uninstall window
//! uninstall.exe --uninstall /S        silent uninstall, data stays
//! ```
//!
//! `/D=` — as in NSIS: the last argument, no quotes needed, spaces allowed.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Install {
        silent: bool,
        dir: Option<PathBuf>,
    },
    Uninstall {
        silent: bool,
        delete_data: bool,
    },
    /// Verify the embedded program's integrity and exit. Writes nothing.
    VerifyPayload,
    /// A service step after the window or uninstaller exits (see `uninstall::finish`).
    Finish {
        wait_pid: u32,
        dir: Option<PathBuf>,
        webview: Option<PathBuf>,
    },
}

/// Parses the arguments (without the program name). `exe_name` — the name of the running file:
/// `uninstall.exe` without flags uninstalls, like the NSIS uninstaller of the same name.
pub fn parse(args: &[String], exe_name: &str) -> Result<Command, String> {
    let mut silent = false;
    let mut uninstall = exe_name.eq_ignore_ascii_case(crate::UNINSTALL_EXE);
    let mut delete_data = false;
    let mut dir = None;
    let mut finish = false;
    let mut wait_pid = None;
    let mut finish_dir = None;
    let mut webview = None;

    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        index += 1;
        let upper = arg.to_ascii_uppercase();
        if upper == "/S" {
            silent = true;
        } else if upper.starts_with("/D=") {
            // Everything to the end of the line is the path, even if the shell split it on spaces.
            let mut path = arg[3..].to_string();
            for rest in &args[index..] {
                path.push(' ');
                path.push_str(rest);
            }
            index = args.len();
            let path = path.trim().trim_matches('"').to_string();
            if path.is_empty() {
                return Err("/D= без пути".into());
            }
            dir = Some(PathBuf::from(path));
        } else {
            let mut value = || {
                let next = args.get(index).cloned();
                index += 1;
                next.ok_or_else(|| format!("{arg}: нет значения"))
            };
            match arg.as_str() {
                "--uninstall" => uninstall = true,
                "--delete-data" => delete_data = true,
                "--verify-payload" => return Ok(Command::VerifyPayload),
                "--finish" => finish = true,
                "--wait-pid" => {
                    wait_pid = Some(value()?.parse().map_err(|_| "--wait-pid: не число")?);
                }
                "--dir" => finish_dir = Some(PathBuf::from(value()?)),
                "--webview" => webview = Some(PathBuf::from(value()?)),
                other => return Err(format!("неизвестный аргумент: {other}")),
            }
        }
    }

    if finish {
        let wait_pid = wait_pid.ok_or("--finish: нужен --wait-pid")?;
        return Ok(Command::Finish {
            wait_pid,
            dir: finish_dir,
            webview,
        });
    }
    if uninstall {
        if dir.is_some() {
            return Err("/D= не применяется при удалении".into());
        }
        if delete_data && !silent {
            return Err("--delete-data только вместе с /S".into());
        }
        return Ok(Command::Uninstall {
            silent,
            delete_data,
        });
    }
    if delete_data {
        return Err("--delete-data только при удалении".into());
    }
    if dir.is_some() && !silent {
        return Err("/D= только вместе с /S".into());
    }
    Ok(Command::Install { silent, dir })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Command, String> {
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        parse(&args, "Phase-Setup-1.0.0.exe")
    }

    #[test]
    fn silent_flags() {
        assert_eq!(
            run(&[]).unwrap(),
            Command::Install {
                silent: false,
                dir: None
            }
        );
        assert_eq!(
            run(&["/S", "/D=C:\\Games\\My", "Phase"]).unwrap(),
            Command::Install {
                silent: true,
                dir: Some(PathBuf::from("C:\\Games\\My Phase"))
            }
        );
        assert_eq!(
            run(&["/s"]).unwrap(),
            Command::Install {
                silent: true,
                dir: None
            }
        );
        assert!(run(&["/D=C:\\x"]).is_err(), "/D только с /S");
        assert!(run(&["--what"]).is_err());
    }

    #[test]
    fn uninstall_by_flag_or_by_file_name() {
        assert_eq!(
            run(&["--uninstall", "/S"]).unwrap(),
            Command::Uninstall {
                silent: true,
                delete_data: false
            }
        );
        assert_eq!(
            parse(&["/S".to_string()], "Uninstall.EXE").unwrap(),
            Command::Uninstall {
                silent: true,
                delete_data: false
            }
        );
        assert!(
            run(&["--uninstall", "--delete-data"]).is_err(),
            "только явно и тихо"
        );
        assert!(run(&["--uninstall", "/S", "/D=C:\\x"]).is_err());
    }

    #[test]
    fn finish_step() {
        assert_eq!(
            run(&["--finish", "--wait-pid", "42", "--dir", "C:\\P"]).unwrap(),
            Command::Finish {
                wait_pid: 42,
                dir: Some(PathBuf::from("C:\\P")),
                webview: None
            }
        );
        assert!(run(&["--finish"]).is_err());
    }
}
