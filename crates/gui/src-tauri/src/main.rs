//! Phase — the app window (Tauri 2 + Svelte).
//!
//! Bridge between the core and the window:
//!
//! ```text
//!  window (JS)  ──invoke──►  commands.rs ──► AppState ──► phase-core::Session
//!     ▲                                                     │ watch / broadcast
//!     └──── core://status, core://event, core://log ◄───────┘ (forwarder tasks)
//! ```

// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autoroom;
#[cfg(feature = "client")]
mod client;
mod codes;
mod commands;
mod harden;
mod language;
mod links;
mod migrate;
mod personal;
mod quit_request;
mod state;
mod tray;

use phase_core::{EventBus, logs::LogLayer};
use tauri::{Emitter, Manager, RunEvent};
use tauri_plugin_deep_link::DeepLinkExt;
use tokio::sync::broadcast::error::RecvError;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::state::{AppState, EVENT_CHANNEL, EXIT_STOP_TIMEOUT, LOG_CHANNEL, LogBuffer};

fn main() {
    // Log: to the window (via LogLayer) and to the developer console (stderr).
    let log_layer = LogLayer::new();
    let mut log_rx = log_layer.subscribe();
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("phase_tunnel=info,phase_core=info,phase_gui=info,warn")
    });
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_target(false)
                .with_writer(std::io::stderr),
        )
        .with(log_layer)
        .init();

    // Language: set before the tray and auto-room; the window refines it later with its own setting.
    language::init();
    let bus = EventBus::new();
    let logs = LogBuffer::default();

    // The Client window (`--client`) opens instead of Phase LAN: the main window is not created.
    #[cfg(feature = "client")]
    let client_mode = client::requested();
    // Phase LAN build: no Client window at all.
    #[cfg(not(feature = "client"))]
    let client_mode = false;
    let mut context = tauri::generate_context!();
    if client_mode {
        context.config_mut().app.windows.clear();
    }
    // LanBridge window settings: before the window is created, since WebView2 opens its folder immediately.
    migrate::webview_data(&context.config().identifier);

    let mut builder = tauri::Builder::default();
    // The single-instance plugin goes first: a phase:// link on Windows launches a second
    // instance, and the plugin passes its arguments to the open window and closes the second one.
    // A repeat launch without a link just shows the open window.
    if !client_mode && !second_instance_allowed() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            links::show_main_window(app)
        }));
    }
    #[cfg(feature = "client")]
    {
        builder = builder.manage(client::ClientGame::default());
    }
    let app = builder
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![tray::AUTOSTART_FLAG]),
        ))
        .manage(links::LinkInbox::default())
        .manage(autoroom::AutoRoom::default())
        .manage(codes::RoomCodes::default())
        .on_window_event(move |window, event| {
            // Phase LAN hides to the tray; the Client window closes like a normal program.
            if !client_mode {
                tray::on_window_event(window, event)
            }
        })
        .manage(AppState::new(bus.clone(), logs.clone()))
        .setup(move |app| {
            #[cfg(feature = "client")]
            if client_mode {
                harden::apply(&client::open_window(app.handle())?);
                return Ok(());
            }
            // The window is a program, not a browser: WebView2 menus and shortcuts are off.
            for window in app.webview_windows().values() {
                harden::apply(window);
            }
            // The window is created hidden (tauri.conf.json): when launched with Windows it
            // stays in the tray, otherwise it is shown immediately.
            tray::install(app.handle())?;
            if !tray::started_by_autostart() {
                links::show_main_window(app.handle());
            }
            autoroom::spawn(app.handle().clone());
            // The installer asks to close before an update (see quit_request.rs).
            quit_request::spawn(app.handle().clone());
            // phase:// links: scheme registration (portable build), links while running,
            // and the link the window was launched with.
            #[cfg(windows)]
            links::ensure_registered(app.handle());
            let links_handle = app.handle().clone();
            app.deep_link()
                .on_open_url(move |event| links::open(&links_handle, event.urls()));
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                links::open(app.handle(), urls);
            }

            let handle = app.handle().clone();
            // Core events → window.
            let mut events = bus.subscribe();
            let events_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match events.recv().await {
                        Ok(event) => {
                            tray::on_core_event(&events_handle, &event);
                            let _ = events_handle.emit(EVENT_CHANNEL, &event);
                        }
                        Err(RecvError::Lagged(n)) => {
                            tracing::warn!("окно не успевает: пропущено событий {n}")
                        }
                        Err(RecvError::Closed) => break,
                    }
                }
            });
            // Log → buffer + window.
            tauri::async_runtime::spawn(async move {
                loop {
                    match log_rx.recv().await {
                        Ok(line) => {
                            logs.push(line.clone());
                            let _ = handle.emit(LOG_CHANNEL, &line);
                        }
                        Err(RecvError::Lagged(_)) => {}
                        Err(RecvError::Closed) => break,
                    }
                }
            });
            tracing::info!("Phase {} запущен", phase_core::APP_VERSION);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::core_info,
            commands::game_running,
            commands::recent_worlds,
            commands::install_companion,
            commands::list_profiles,
            commands::ensure_profile,
            commands::set_profile_nickname,
            commands::room_look,
            commands::set_room_look,
            commands::set_player_style,
            commands::set_own_glow,
            commands::start_host,
            commands::join,
            commands::stop_session,
            commands::session_status,
            commands::answer_approval,
            commands::pending_approvals,
            commands::recent_logs,
            commands::allowed_devices,
            commands::forget_device,
            commands::mods_overview,
            commands::pick_mods_dir,
            commands::set_host_mods,
            commands::mod_command,
            commands::restore_mods_backup,
            personal::save_background,
            personal::load_background,
            personal::delete_background,
            commands::take_link,
            #[cfg(feature = "client")]
            client::client_status,
            #[cfg(feature = "client")]
            client::client_install,
            #[cfg(feature = "client")]
            client::client_launch,
            #[cfg(feature = "client")]
            client::client_stop,
            language::language_info,
            language::set_language,
            autoroom::set_auto_room,
            codes::room_code,
            tray::autostart_enabled,
            tray::set_autostart,
            harden::open_devtools,
        ])
        .build(context)
        .expect("не удалось запустить окно Phase");

    app.run(|app, event| {
        // Closing the program: stop the session cleanly first, so friends get
        // "connection closed" rather than waiting for a timeout; the report is saved.
        if let RunEvent::Exit = event {
            let state = app.state::<AppState>();
            tauri::async_runtime::block_on(async {
                let _ = tokio::time::timeout(EXIT_STOP_TIMEOUT, state.stop()).await;
            });
        }
    });
}

/// A second instance next to the first: debug builds only and on explicit request
/// (`PHASE_SECOND_INSTANCE=1`) — for developer checks while the working copy is open.
fn second_instance_allowed() -> bool {
    cfg!(debug_assertions) && std::env::var_os("PHASE_SECOND_INSTANCE").is_some_and(|v| v == "1")
}
