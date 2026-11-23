//! The window is a program, not a browser tab: everything in WebView2 itself that gives away a
//! browser is turned off.
//!
//! - the standard context menu ("Back", "Reload", "Save as", "Print");
//! - browser shortcuts: `Ctrl+R`, `F5`, `Ctrl+P`, `Ctrl+S`, `Ctrl+U`, `Ctrl+F`, `Ctrl+G`,
//!   `F12`, zoom, "Back/Forward". Text editing (`Ctrl+C/V/X/A/Z`) and arrows stay;
//! - the status bar, zoom by wheel and gesture, swipe navigation.
//!
//! The second line of defense is `src/lib/guard.ts` in the window: side mouse buttons, dragging,
//! selection. DevTools exist only in debug builds: [`open_devtools`] is called by the
//! window on `F12` / `Ctrl+Shift+I`, and does nothing in release builds (developer tools
//! are turned off in WebView2 itself there).

use tauri::WebviewWindow;

/// Applies the settings to the window. An error doesn't block startup: the protection inside the window remains.
pub fn apply(window: &WebviewWindow) {
    #[cfg(windows)]
    if let Err(err) = window.with_webview(|webview| {
        if let Err(err) = webview2_settings(&webview) {
            tracing::warn!("настройки WebView2 не применены: {err}");
        }
    }) {
        tracing::warn!("настройки WebView2 не применены: {err}");
    }
    #[cfg(not(windows))]
    let _ = window;
}

#[cfg(windows)]
fn webview2_settings(webview: &tauri::webview::PlatformWebview) -> windows_core::Result<()> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2Settings3, ICoreWebView2Settings5, ICoreWebView2Settings6,
    };
    use windows_core::Interface;

    // SAFETY: COM calls run on the window thread (guaranteed by `with_webview`), the objects
    // are alive as long as `webview` is.
    unsafe {
        let settings = webview.controller().CoreWebView2()?.Settings()?;
        settings.SetAreDefaultContextMenusEnabled(false)?;
        settings.SetIsStatusBarEnabled(false)?;
        settings.SetIsZoomControlEnabled(false)?;
        settings.SetIsBuiltInErrorPageEnabled(false)?;
        // The following settings appeared in newer WebView2 versions: an old runtime
        // doesn't have them — then only the protection in the window works.
        if let Ok(settings) = settings.cast::<ICoreWebView2Settings3>() {
            settings.SetAreBrowserAcceleratorKeysEnabled(false)?;
        }
        if let Ok(settings) = settings.cast::<ICoreWebView2Settings5>() {
            settings.SetIsPinchZoomEnabled(false)?;
        }
        if let Ok(settings) = settings.cast::<ICoreWebView2Settings6>() {
            settings.SetIsSwipeNavigationEnabled(false)?;
        }
    }
    Ok(())
}

/// Developer tools — debug builds only.
#[tauri::command]
pub fn open_devtools(window: WebviewWindow) {
    #[cfg(debug_assertions)]
    window.open_devtools();
    #[cfg(not(debug_assertions))]
    let _ = window;
}
