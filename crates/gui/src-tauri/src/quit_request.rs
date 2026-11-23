//! A polite request to quit — from the Phase installer before an update.
//!
//! The window's close button only hides Phase to the tray, so the installer has no ordinary way
//! to close the program, and killing it is not allowed: the room would be cut off. So
//! the window holds a named event `Local\PhaseQuitRequest-<pid>`; the installer finds the
//! process and sets the event. After that the program itself decides:
//!
//! - no room — a normal exit (like "Quit" in the tray menu);
//! - a room is open — the window is shown and asks for confirmation. "No" leaves
//!   everything as it was, and the installer offers to retry once its wait runs out.

#[cfg(windows)]
pub use imp::spawn;

#[cfg(not(windows))]
pub fn spawn(_app: tauri::AppHandle) {}

#[cfg(windows)]
mod imp {
    use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
    use tauri::{AppHandle, Manager};
    use windows_sys::Win32::{
        Foundation::{HANDLE, WAIT_OBJECT_0},
        System::{
            Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW},
            Threading::{CreateEventW, INFINITE, WaitForSingleObject},
        },
    };

    use crate::{links::show_main_window, state::AppState, tray};

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    /// Starts waiting for the request. If the event can't be created, the installer will ask
    /// the user to close Phase manually, and the program works as usual.
    pub fn spawn(app: AppHandle) {
        let name = wide(&format!("Local\\PhaseQuitRequest-{}", std::process::id()));
        // SAFETY: the name is NUL-terminated; the auto-reset event lives until the process ends.
        let event: HANDLE = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
        if event.is_null() {
            tracing::warn!("событие для установщика не создано");
            return;
        }
        // The handle is an address; it is passed between threads as a number.
        let event = event as usize;
        std::thread::spawn(move || {
            loop {
                // SAFETY: the handle is valid for the whole lifetime of the process.
                let signal = unsafe { WaitForSingleObject(event as HANDLE, INFINITE) };
                if signal != WAIT_OBJECT_0 {
                    return;
                }
                on_request(&app);
            }
        });
    }

    fn on_request(app: &AppHandle) {
        let status = tauri::async_runtime::block_on(app.state::<AppState>().status());
        let room_open = status.is_some_and(|status| !status.phase.is_finished());
        tracing::info!("установщик просит закрыть Phase (комната открыта: {room_open})");
        if room_open && !confirm_close_room(app) {
            return;
        }
        tray::quit_app(app);
    }

    fn confirm_close_room(app: &AppHandle) -> bool {
        show_main_window(app);
        let (title, text, close, keep) = if saved_language_is_english() {
            (
                "Update Phase?",
                "The installer needs to close Phase, and your room is open. \
                 Closing Phase will end the room for everyone in it.",
                "Close room and quit",
                "Keep playing",
            )
        } else {
            (
                "Обновить Phase?",
                "Установщику нужно закрыть Phase, а у вас открыта комната. \
                 Если закрыть программу, комната завершится для всех участников.",
                "Закрыть комнату и выйти",
                "Остаться",
            )
        };
        let answer = MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title(title)
            .set_description(text)
            .set_buttons(MessageButtons::OkCancelCustom(close.into(), keep.into()))
            .show();
        matches!(answer, MessageDialogResult::Custom(choice) if choice == close)
    }

    /// Language chosen in the installer: `HKCU\Software\Phase`, value `Language`.
    fn saved_language_is_english() -> bool {
        let key = wide(r"Software\Phase");
        let value = wide("Language");
        let mut buffer = [0u16; 8];
        let mut size = std::mem::size_of_val(&buffer) as u32;
        // SAFETY: the strings are NUL-terminated, the buffer size is passed in bytes.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        status == 0 && String::from_utf16_lossy(&buffer).trim_end_matches('\0') == "en"
    }
}
