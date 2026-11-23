//! Command lines of running processes, what the "Command line" column shows
//! in Task Manager. Needed to learn the player's nickname from the running game's
//! arguments (`--username <nick>`).
//!
//! Read-only and without administrator rights: other users' and system processes
//! are simply skipped. On other OSes the list is empty.

use std::io;

/// A running process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    /// Program file name, for example `javaw.exe`.
    pub exe: String,
    /// The full command line, as the process received it.
    pub command_line: String,
}

/// Processes whose program file name is one of `exe_names` (case-insensitive).
/// Processes we cannot access are left out.
#[cfg(windows)]
pub fn by_exe_name(exe_names: &[&str]) -> io::Result<Vec<Process>> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
    };

    // SAFETY: a process list snapshot is a plain call without pointers.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut found = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: `entry` is a struct of the right size (dwSize is set), the snapshot is valid.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more {
        let len = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        let exe = String::from_utf16_lossy(&entry.szExeFile[..len]);
        if exe_names.iter().any(|name| name.eq_ignore_ascii_case(&exe))
            && let Some(command_line) = command_line(entry.th32ProcessID)
        {
            found.push(Process {
                pid: entry.th32ProcessID,
                exe,
                command_line,
            });
        }
        // SAFETY: the same valid snapshot and struct.
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    // SAFETY: closing the handle we obtained above.
    unsafe { CloseHandle(snapshot) };
    Ok(found)
}

#[cfg(not(windows))]
pub fn by_exe_name(_exe_names: &[&str]) -> io::Result<Vec<Process>> {
    Ok(Vec::new())
}

/// Whether at least one process with this file name is running (case-insensitive). Cheaper
/// than `by_exe_name`: command lines are not read and process access is not needed.
#[cfg(windows)]
pub fn is_running(exe_name: &str) -> io::Result<bool> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
    };

    // SAFETY: a process list snapshot is a plain call without pointers.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut found = false;
    // SAFETY: `entry` is a struct of the right size (dwSize is set), the snapshot is valid.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more && !found {
        let len = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        found = String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case(exe_name);
        // SAFETY: the same valid snapshot and struct.
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    // SAFETY: closing the handle we obtained above.
    unsafe { CloseHandle(snapshot) };
    Ok(found)
}

#[cfg(not(windows))]
pub fn is_running(_exe_name: &str) -> io::Result<bool> {
    Ok(false)
}

/// Command line of a process. `None` means no access or the process has already exited.
#[cfg(windows)]
fn command_line(pid: u32) -> Option<String> {
    use windows_sys::{
        Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation},
        Win32::{
            Foundation::{CloseHandle, UNICODE_STRING},
            System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        },
    };

    /// A Windows command line is at most 32,767 UTF-16 units; plus room for the header.
    const BUF_LEN: usize = 72 * 1024;

    // SAFETY: a plain call; a null handle means access denied.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    // Buffer of u64: aligned for UNICODE_STRING (it contains a pointer).
    let mut buf = vec![0u64; BUF_LEN / 8];
    let mut returned = 0u32;
    // SAFETY: the buffer is ours and its size in bytes is passed correctly; the system writes
    // a UNICODE_STRING into it with the text right after it.
    let status = unsafe {
        NtQueryInformationProcess(
            handle,
            ProcessCommandLineInformation,
            buf.as_mut_ptr().cast(),
            BUF_LEN as u32,
            &mut returned,
        )
    };
    // SAFETY: closing the handle we opened ourselves.
    unsafe { CloseHandle(handle) };
    if status < 0 {
        return None;
    }
    // SAFETY: on success the buffer starts with a UNICODE_STRING; the buffer is aligned and larger.
    let text = unsafe { &*buf.as_ptr().cast::<UNICODE_STRING>() };
    let start = text.Buffer as usize;
    let bytes = text.Length as usize;
    let (buf_start, buf_end) = (buf.as_ptr() as usize, buf.as_ptr() as usize + BUF_LEN);
    // The text must lie inside our own buffer, otherwise we do not read it.
    if text.Buffer.is_null() || start < buf_start || start.checked_add(bytes)? > buf_end {
        return None;
    }
    // SAFETY: the range [start, start + bytes) is checked above and lies in `buf`; the PWSTR
    // is 2-byte aligned (the buffer is 8-byte aligned, the system sets the offset).
    let wide = unsafe { std::slice::from_raw_parts(text.Buffer.cast_const(), bytes / 2) };
    Some(String::from_utf16_lossy(wide))
}

/// Working directory of a process: Task Manager does not show it, but the program itself
/// knows it. Needed to find the game folder when the command line has no `--gameDir`
/// (for example, a Minecraft server: its folder is the one it was started from).
///
/// Read-only and without administrator rights. `None` means no access, the process exited
/// or the folder could not be read. Works on 64-bit Windows.
#[cfg(all(windows, target_pointer_width = "64"))]
pub fn working_dir(pid: u32) -> Option<std::path::PathBuf> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};

    use windows_sys::{
        Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation},
        Win32::{
            Foundation::{CloseHandle, HANDLE},
            System::{
                Diagnostics::Debug::ReadProcessMemory,
                Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ},
            },
        },
    };

    /// Offset of the process parameters pointer in its PEB (64-bit).
    const PEB_PROCESS_PARAMETERS: usize = 0x20;
    /// Offset of the "current directory" string in the process parameters (64-bit).
    const PARAMS_CURRENT_DIRECTORY: usize = 0x38;
    /// A Windows path is at most 32,767 UTF-16 units.
    const MAX_PATH_BYTES: usize = 2 * 32 * 1024;

    /// Reads exactly `out.len()` bytes from the process memory.
    fn read(process: HANDLE, addr: usize, out: &mut [u8]) -> Option<()> {
        let mut got = 0usize;
        // SAFETY: we write into our own buffer of its own length; the system checks the foreign
        // address and returns 0 on error.
        let ok = unsafe {
            ReadProcessMemory(
                process,
                addr as *const _,
                out.as_mut_ptr().cast(),
                out.len(),
                &mut got,
            )
        };
        (ok != 0 && got == out.len()).then_some(())
    }

    fn read_usize(process: HANDLE, addr: usize) -> Option<usize> {
        let mut raw = [0u8; size_of::<usize>()];
        read(process, addr, &mut raw)?;
        Some(usize::from_le_bytes(raw))
    }

    fn current_dir(process: HANDLE) -> Option<std::path::PathBuf> {
        // PROCESS_BASIC_INFORMATION: six 8-byte fields, the second is the PEB address.
        let mut info = [0usize; 6];
        // SAFETY: the buffer is ours, the size is passed correctly.
        let status = unsafe {
            NtQueryInformationProcess(
                process,
                ProcessBasicInformation,
                info.as_mut_ptr().cast(),
                size_of_val(&info) as u32,
                std::ptr::null_mut(),
            )
        };
        let peb = info[1];
        if status < 0 || peb == 0 {
            return None;
        }
        let params = read_usize(process, peb.checked_add(PEB_PROCESS_PARAMETERS)?)?;
        // UNICODE_STRING: length in bytes (u16), capacity (u16), padding, pointer.
        let mut text = [0u8; 16];
        read(
            process,
            params.checked_add(PARAMS_CURRENT_DIRECTORY)?,
            &mut text,
        )?;
        let bytes = u16::from_le_bytes([text[0], text[1]]) as usize;
        let buffer = usize::from_le_bytes(text[8..16].try_into().ok()?);
        if bytes == 0 || !bytes.is_multiple_of(2) || bytes > MAX_PATH_BYTES || buffer == 0 {
            return None;
        }
        let mut raw = vec![0u8; bytes];
        read(process, buffer, &mut raw)?;
        let wide: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        Some(OsString::from_wide(&wide).into())
    }

    // SAFETY: a plain call; a null handle means access denied.
    let process =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid) };
    if process.is_null() {
        return None;
    }
    let dir = current_dir(process);
    // SAFETY: closing the handle we opened ourselves.
    unsafe { CloseHandle(process) };
    dir
}

#[cfg(not(all(windows, target_pointer_width = "64")))]
pub fn working_dir(_pid: u32) -> Option<std::path::PathBuf> {
    None
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn reads_working_dir_of_this_process() {
        let expected = std::env::current_dir().unwrap();
        let found = working_dir(std::process::id()).expect("working directory of our own process");
        // The system stores the path with a trailing backslash, so compare by components.
        assert_eq!(
            found.components().collect::<Vec<_>>(),
            expected.components().collect::<Vec<_>>()
        );
        assert_eq!(working_dir(0), None, "the system process is not accessible");
    }

    #[test]
    fn finds_this_test_process_and_reads_its_command_line() {
        let me = std::env::current_exe().unwrap();
        let name = me.file_name().unwrap().to_str().unwrap().to_string();
        let found = by_exe_name(&[&name.to_uppercase()]).unwrap();
        let mine = found
            .iter()
            .find(|p| p.pid == std::process::id())
            .expect("the test process should be found by its file name");
        // The command line starts with the path to the test program.
        let stem = me.file_stem().unwrap().to_str().unwrap();
        assert!(mine.command_line.contains(stem), "{}", mine.command_line);
    }

    #[test]
    fn unknown_name_gives_empty_list() {
        assert!(
            by_exe_name(&["no-such-program-phase.exe"])
                .unwrap()
                .is_empty()
        );
    }
}
