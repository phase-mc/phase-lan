//! Running Java processes and their command lines with argument files expanded.
//!
//! Java accepts parameters from a file: `javaw @C:\…\args`. That is how Legacy
//! Launcher (TLauncher) starts the game: the process command line has a single `@…\Temp\ll…args`, while
//! `--gameDir`, `--version "Forge 1.21.1"`, `--launchTarget` and `--username` live
//! only in that file. Without expanding it Phase sees neither the game folder nor its loader.

use std::{fs, io, path::Path};

use phase_netstat::process::Process;

use crate::nickname::GAME_EXE;

/// We do not read an argument file larger than this (launchers produce tens of kilobytes).
const ARG_FILE_MAX: u64 = 1024 * 1024;

/// Java processes (`javaw.exe`, `java.exe`); in `command_line` argument files are already
/// expanded. The call blocks.
pub(crate) fn game_processes() -> io::Result<Vec<Process>> {
    let mut processes = phase_netstat::process::by_exe_name(GAME_EXE)?;
    for process in &mut processes {
        process.command_line = expand_arg_files(&process.command_line);
    }
    Ok(processes)
}

/// Appends the contents of `@file` argument files (absolute path) to the command line as
/// ordinary parameters. An unreadable or too large file is skipped.
pub(crate) fn expand_arg_files(command_line: &str) -> String {
    let mut expanded = command_line.to_string();
    for token in split_command_line(command_line) {
        // `@@…` is an escaped `@`, not a file.
        let Some(path) = token.strip_prefix('@').filter(|p| !p.starts_with('@')) else {
            continue;
        };
        let path = Path::new(path);
        if !path.is_absolute() {
            continue;
        }
        let Some(text) = read_small(path) else {
            continue;
        };
        for arg in parse_arg_file(&text) {
            expanded.push(' ');
            if arg.is_empty() || arg.contains(char::is_whitespace) {
                expanded.push('"');
                expanded.push_str(&arg);
                expanded.push('"');
            } else {
                expanded.push_str(&arg);
            }
        }
    }
    expanded
}

fn read_small(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > ARG_FILE_MAX {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Windows command line split into words: spaces separate, quotes join.
fn split_command_line(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let (mut quoted, mut started) = (false, false);
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// Java argument file: parameters separated by spaces and line breaks; `"…"` and `'…'`
/// join words, inside quotes `\` escapes (`\\`, `\"`, `\n`…); `#` at the start of a word begins
/// a comment to the end of the line.
fn parse_arg_file(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut arg = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            match c {
                c if c == q => quote = None,
                '\\' => match chars.next() {
                    Some('n') => arg.push('\n'),
                    Some('t') => arg.push('\t'),
                    Some('r') => arg.push('\r'),
                    Some('f') => arg.push('\u{c}'),
                    // Line break inside quotes: continuation, leading spaces are swallowed.
                    Some('\n' | '\r') => {
                        while chars.peek().is_some_and(|c| c.is_whitespace()) {
                            chars.next();
                        }
                    }
                    Some(other) => arg.push(other),
                    None => {}
                },
                c => arg.push(c),
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                started = true;
            }
            '#' if !started => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            c if c.is_whitespace() => {
                if started {
                    args.push(std::mem::take(&mut arg));
                    started = false;
                }
            }
            c => {
                arg.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(arg);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::{flag_value, game_dir_from_command_line};

    #[test]
    fn arg_file_quotes_escapes_and_comments() {
        let text = "-Xms3G\n# comment --gameDir nope\n--version\n\"Forge 1.21.1\"\n--gameDir\n\"C:\\\\Users\\\\me\\\\My Game\"\n--launchTarget forge_client\n'a b'\n";
        assert_eq!(
            parse_arg_file(text),
            [
                "-Xms3G",
                "--version",
                "Forge 1.21.1",
                "--gameDir",
                r"C:\Users\me\My Game",
                "--launchTarget",
                "forge_client",
                "a b",
            ]
        );
    }

    #[test]
    fn expands_legacy_launcher_arg_file() {
        let dir = std::env::temp_dir().join(format!("phase-arg-file-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ll123args");
        fs::write(
            &file,
            "-Xms1G\n--version\n\"Forge 1.21.1\"\n--gameDir\n\"C:\\\\Games\\\\mc game\"\n",
        )
        .unwrap();
        let line = format!(r#""C:\jre\bin\javaw.exe" @{}"#, file.display());
        let expanded = expand_arg_files(&line);
        assert_eq!(
            game_dir_from_command_line(&expanded).as_deref(),
            Some(Path::new(r"C:\Games\mc game"))
        );
        assert_eq!(
            flag_value(&expanded, "--version").as_deref(),
            Some("Forge 1.21.1")
        );

        // No file, relative path, `@@`: the line is unchanged.
        for line in ["javaw @missing", "javaw @@x", r"javaw @C:\nope\args"] {
            assert_eq!(expand_arg_files(line), line);
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
