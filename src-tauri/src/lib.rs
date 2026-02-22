pub mod cli;
mod notification;
pub mod pipe;
pub mod remote;
pub mod setup;
pub mod sound;
mod updater;
pub mod win32;

use log::LevelFilter;
use simplelog::{ColorChoice, CombinedLogger, Config, TermLogger, TerminalMode, WriteLogger};
use std::fs::OpenOptions;
use std::sync::{Arc, Mutex};

use cli::NotifyRequest;
use notification::{
    close_notification, get_notification_for_window, on_foreground_changed, show_notification,
    NotificationData, NotificationManagerState,
};

/// Managed state for the remote notification and SSH tunnel subsystem.
pub struct RemoteState {
    pub tunnel_status: Arc<Mutex<remote::TunnelStatus>>,
    pub ssh_tunnel: Arc<Mutex<Option<remote::SshTunnel>>>,
    /// True when the user explicitly disconnected the tunnel (suppress auto-reconnect).
    pub user_disconnected: Arc<Mutex<bool>>,
    /// Port on which the HTTP notification server is currently listening (0 = not started).
    pub http_server_port: Arc<Mutex<u16>>,
}

use tauri::image::Image;
use tauri::menu::{MenuBuilder, MenuItem, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// Holds tray menu items so we can update their text at runtime.
pub struct TrayMenuState {
    pub settings_item: MenuItem<tauri::Wry>,
    pub quit_item: MenuItem<tauri::Wry>,
}

/// Generate a 16×16 RGBA status indicator icon for the system tray.
fn generate_tray_status_icon(status: &str) -> Vec<u8> {
    let size: usize = 16;
    let mut rgba = vec![0u8; size * size * 4];

    let (r, g, b) = match status {
        "Connected" => (34u8, 197, 94),   // green-500
        "Connecting" => (245, 158, 11),   // amber-500
        _ => (239, 68, 68),               // red-500 (error)
    };

    let center = (size as f32 - 1.0) / 2.0;
    let inner_r = 5.5f32;
    let outer_r = 7.0f32;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            let idx = (y * size + x) * 4;

            if dist <= inner_r {
                let alpha = if dist > inner_r - 1.0 {
                    ((inner_r - dist) * 255.0).min(255.0) as u8
                } else {
                    255
                };
                rgba[idx] = r;
                rgba[idx + 1] = g;
                rgba[idx + 2] = b;
                rgba[idx + 3] = alpha;
            } else if dist <= outer_r {
                let alpha = if dist > outer_r - 1.0 {
                    ((outer_r - dist) * 180.0).min(180.0) as u8
                } else {
                    180
                };
                rgba[idx] = 255;
                rgba[idx + 1] = 255;
                rgba[idx + 2] = 255;
                rgba[idx + 3] = alpha;
            }
        }
    }
    rgba
}

/// Update system tray icon and tooltip based on SSH tunnel status.
pub fn update_tray_status(app: &AppHandle, status: &str) {
    if let Some(tray) = app.tray_by_id("main") {
        let tooltip = match status {
            "Connected" => "Agent Toast - SSH Connected",
            "Connecting" => "Agent Toast - SSH Connecting...",
            "Disconnected" => "Agent Toast",
            _ => "Agent Toast - SSH Error",
        };
        let _ = tray.set_tooltip(Some(tooltip));

        if status == "Disconnected" {
            let icon_bytes = include_bytes!("../icons/tray.ico");
            if let Ok(icon) = Image::from_bytes(icon_bytes) {
                let _ = tray.set_icon(Some(icon));
            }
        } else {
            let rgba = generate_tray_status_icon(status);
            let icon = Image::new_owned(rgba, 16, 16);
            let _ = tray.set_icon(Some(icon));
        }
    }
}

/// Update tray menu text to match the current locale.
pub fn update_tray_locale(app: &AppHandle) {
    let locale = setup::read_locale();
    let (label_settings, label_quit) = match locale.as_str() {
        "en" => ("Settings", "Quit"),
        _ => ("설정", "종료"),
    };
    if let Some(state) = app.try_state::<TrayMenuState>() {
        let _ = state.settings_item.set_text(label_settings);
        let _ = state.quit_item.set_text(label_quit);
    }
}

#[tauri::command]
fn get_locale() -> String {
    setup::read_locale()
}

#[tauri::command]
fn is_dev_mode() -> bool {
    cfg!(debug_assertions)
}

#[tauri::command(rename_all = "snake_case")]
fn play_sound(sound_name: String) {
    crate::sound::play_notification_sound(&sound_name);
}

#[tauri::command]
fn is_portable() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return true;
    };
    let Some(dir) = exe.parent() else {
        return true;
    };
    !dir.join("uninstall.exe").exists()
}

#[tauri::command]
fn get_monitor_list() -> Vec<win32::MonitorInfo> {
    win32::get_monitor_list()
}

#[tauri::command]
fn get_notification_data(window: WebviewWindow) -> Option<NotificationData> {
    let state = window.app_handle().state::<NotificationManagerState>();
    get_notification_for_window(&state, window.label())
}

#[tauri::command]
fn close_notify(id: String, app: AppHandle) {
    let state = app.state::<NotificationManagerState>();
    close_notification(&app, &state, &id);
}

#[tauri::command]
fn activate_source(hwnd: isize, id: String, app: AppHandle) {
    log::debug!("activate_source called: hwnd={hwnd}, id={id}");
    win32::activate_window(hwnd);
    let state = app.state::<NotificationManagerState>();
    close_notification(&app, &state, &id);
}

#[tauri::command]
fn test_notification(app: AppHandle) {
    log::debug!("[TEST] test_notification command called");
    let state = app.state::<NotificationManagerState>().inner().clone();
    let locale = setup::read_locale();

    // Pick random event type using current time
    let events = ["task_complete", "user_input_required", "error"];
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let event = events[(nanos as usize) % events.len()];

    let (test_msg, test_title) = match locale.as_str() {
        "en" => ("This is a test notification", "Test"),
        _ => ("테스트 알림입니다", "테스트"),
    };
    let req = NotifyRequest {
        pid: 0,
        event: event.to_string(),
        message: Some(test_msg.to_string()),
        title_hint: Some(test_title.to_string()),
        process_tree: Some(vec![]),
        source: "claude".into(),
        remote_host: None,
    };
    log::debug!("[TEST] Spawning notification thread for event={event}");
    std::thread::spawn(move || {
        log::debug!("[TEST] Thread started, calling show_notification");
        show_notification(&app, &state, req);
        log::debug!("[TEST] show_notification returned");
    });
}

#[tauri::command]
async fn open_settings(app: AppHandle, tab: Option<String>) {
    let app_clone = app.clone();
    let _ = app.run_on_main_thread(move || {
        open_setup_window_with_tab(&app_clone, tab.as_deref());
    });
}

pub fn open_setup_window(app: &AppHandle) {
    open_setup_window_with_tab(app, None);
}

pub fn open_setup_window_with_tab(app: &AppHandle, tab: Option<&str>) {
    // If setup window already exists, focus it (and optionally navigate to tab)
    if let Some(win) = app.get_webview_window("setup") {
        if let Some(t) = tab {
            let _ = win.eval(format!("window.location.hash = '{t}';"));
        }
        let _ = win.set_focus();
        return;
    }

    let locale = setup::read_locale();
    let setup_title = match locale.as_str() {
        "en" => "Agent Toast Settings",
        _ => "Agent Toast 설정",
    };

    let url = match tab {
        Some(t) => format!("index.html#{t}"),
        None => "index.html".to_string(),
    };

    let _ = WebviewWindowBuilder::new(app, "setup", WebviewUrl::App(url.into()))
        .title(setup_title)
        .inner_size(560.0, 720.0)
        .resizable(true)
        .center()
        .build();
}

pub fn run_app(initial_request: Option<NotifyRequest>, open_setup: bool) {
    // Initialize logging to temp file + terminal
    let log_path = std::env::temp_dir().join("agent-toast.log");

    // Keep only the last half (by line count) if the log exceeds 1 MB
    const MAX_LOG_SIZE: u64 = 1024 * 1024;
    if let Ok(meta) = std::fs::metadata(&log_path) {
        if meta.len() > MAX_LOG_SIZE {
            if let Ok(content) = std::fs::read_to_string(&log_path) {
                let lines: Vec<&str> = content.lines().collect();
                let kept = lines[lines.len() / 2..].join("\n");
                let _ = std::fs::write(&log_path, kept + "\n");
            }
        }
    }

    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok();

    let mut loggers: Vec<Box<dyn simplelog::SharedLogger>> = vec![TermLogger::new(
        LevelFilter::Debug,
        Config::default(),
        TerminalMode::Stderr,
        ColorChoice::Auto,
    )];
    if let Some(file) = log_file {
        loggers.push(WriteLogger::new(
            LevelFilter::Debug,
            Config::default(),
            file,
        ));
    }
    let _ = CombinedLogger::init(loggers);

    // Capture panics from any thread into the log file before aborting
    let panic_log_path = log_path.clone();
    std::panic::set_hook(Box::new(move |info| {
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown".to_string()
        };
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".to_string());
        let bt = std::backtrace::Backtrace::force_capture();
        let msg = format!(
            "[PANIC] {payload}\n  at {location}\n  thread: {:?}\n{bt}",
            std::thread::current().name().unwrap_or("unnamed")
        );
        log::error!("{msg}");
        // Also write directly to file in case the logger is broken
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .append(true)
            .open(&panic_log_path)
        {
            use std::io::Write;
            let _ = writeln!(f, "{msg}");
        }
    }));

    log::info!("=== Agent Toast Started === (log: {})", log_path.display());

    let mgr_state = notification::create_manager();

    // Set up RemoteState with default disconnected status
    let remote_state = RemoteState {
        tunnel_status: Arc::new(Mutex::new(remote::TunnelStatus::Disconnected)),
        ssh_tunnel: Arc::new(Mutex::new(None)),
        user_disconnected: Arc::new(Mutex::new(false)),
        http_server_port: Arc::new(Mutex::new(0)),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(mgr_state.clone())
        .manage(remote_state)
        .invoke_handler(tauri::generate_handler![
            close_notify,
            activate_source,
            get_notification_data,
            test_notification,
            get_locale,
            is_dev_mode,
            is_portable,
            play_sound,
            open_settings,
            setup::get_hook_config,
            setup::save_hook_config,
            setup::get_exe_path,
            setup::get_saved_exe_path,
            setup::open_settings_file,
            setup::is_hook_config_saved,
            setup::get_codex_installed,
            get_monitor_list,
            updater::mark_update_pending,
            setup::save_remote_config,
            remote::connect_ssh_tunnel,
            remote::disconnect_ssh_tunnel,
            remote::get_tunnel_status,
            remote::test_remote_connection,
            remote::generate_remote_token,
            remote::get_ssh_log_path,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let state = mgr_state.clone();

            // System tray
            let tray_handle = handle.clone();
            let locale = setup::read_locale();
            let (label_settings, label_quit) = match locale.as_str() {
                "en" => ("Settings", "Quit"),
                _ => ("설정", "종료"),
            };
            let settings_item = MenuItemBuilder::with_id("settings", label_settings).build(app)?;
            let quit_item = MenuItemBuilder::with_id("quit", label_quit).build(app)?;
            app.manage(TrayMenuState {
                settings_item: settings_item.clone(),
                quit_item: quit_item.clone(),
            });
            let menu = MenuBuilder::new(app)
                .item(&settings_item)
                .item(&quit_item)
                .build()?;
            // TODO: Tauri ICO 파싱 버그로 인해 트레이 아이콘 별도 로드 필요
            // - Tauri가 ICO의 첫 번째 엔트리만 사용 (entries()[0])
            // - icon.ico는 작업표시줄용 (큰 사이즈 먼저), tray.ico는 트레이용 (작은 사이즈)
            // - 제목표시줄도 icon.ico 사용해서 해상도 깨짐 (Tauri 수정 필요)
            // - 관련 이슈: https://github.com/tauri-apps/tauri/issues/14596
            let tray_icon_bytes = include_bytes!("../icons/tray.ico");
            let tray_icon = Image::from_bytes(tray_icon_bytes).expect("failed to load tray icon");
            TrayIconBuilder::with_id("main")
                .icon(tray_icon)
                .menu(&menu)
                .tooltip("Agent Toast")
                .on_menu_event(move |app, event| match event.id().as_ref() {
                    "settings" => open_setup_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(&tray_handle)?;

            // Start Named Pipe server for subsequent calls
            let pipe_handle = handle.clone();
            let pipe_state = state.clone();
            pipe::start_server(move |req| {
                show_notification(&pipe_handle, &pipe_state, req);
            });

            // FR-3: Event-based foreground change detection via SetWinEventHook
            let focus_handle = handle.clone();
            let focus_state = state.clone();
            win32::start_foreground_listener(move |hwnd| {
                on_foreground_changed(&focus_handle, &focus_state, hwnd);
            });

            // Open setup window if requested
            if open_setup {
                open_setup_window(&handle);
            }

            // Show initial notification if provided
            if let Some(req) = initial_request {
                let init_handle = handle.clone();
                let init_state = state.clone();
                // Delay slightly to ensure app is ready
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    show_notification(&init_handle, &init_state, req);
                });
            }

            // Check if update was just completed
            updater::check_update_completed(&handle, &state);

            // Check for updates in background
            updater::check_for_updates(&handle, &state);

            // Start remote notification HTTP server if enabled
            {
                let hook_config = setup::get_hook_config();
                if hook_config.remote_enabled {
                    let remote_st = handle.state::<RemoteState>();
                    let port = hook_config.remote_port;
                    let token = hook_config.remote_token.clone();
                    let http_app = handle.clone();
                    let http_state = state.clone();
                    remote::start_http_server(
                        port,
                        token,
                        http_app,
                        http_state,
                        hook_config.ssh_host.clone(),
                    );
                    *remote_st.http_server_port.lock().unwrap() = port;

                    // Auto-connect SSH tunnel if enabled and host is configured
                    if hook_config.ssh_auto_connect && !hook_config.ssh_host.is_empty() {
                        let ssh_config = remote::SshConfig {
                            host: hook_config.ssh_host.clone(),
                            port: hook_config.ssh_port,
                            user: hook_config.ssh_user.clone(),
                            key_path: remote::resolve_home_dir(&hook_config.ssh_key_path),
                            remote_port: hook_config.ssh_remote_port,
                            local_port: hook_config.remote_port,
                        };
                        let tunnel = remote::SshTunnel::new(ssh_config);
                        *remote_st.ssh_tunnel.lock().unwrap() = Some(tunnel);

                        let tunnel_arc = remote_st.ssh_tunnel.clone();
                        let status_arc = remote_st.tunnel_status.clone();
                        if let Some(ref mut t) = *remote_st.ssh_tunnel.lock().unwrap() {
                            if let Err(e) = t.connect(status_arc.clone()) {
                                log::error!("[SSH] Auto-connect failed: {e}");
                            }
                        }

                        // Start watchdog for auto-reconnect
                        let user_disc = remote_st.user_disconnected.clone();
                        remote::start_watchdog(tunnel_arc, status_arc, true, user_disc);
                    }

                    // Always start tray status polling (user might connect manually later)
                    let poll_status = remote_st.tunnel_status.clone();
                    let poll_app = handle.clone();
                    std::thread::spawn(move || {
                        let mut last = String::new();
                        loop {
                            std::thread::sleep(std::time::Duration::from_secs(3));
                            let current = {
                                let s = poll_status.lock().unwrap();
                                match &*s {
                                    remote::TunnelStatus::Connected => "Connected",
                                    remote::TunnelStatus::Connecting => "Connecting",
                                    remote::TunnelStatus::Disconnected => "Disconnected",
                                    remote::TunnelStatus::Error(_) => "Error",
                                }
                                .to_string()
                            };
                            if current != last {
                                update_tray_status(&poll_app, &current);
                                last = current;
                            }
                        }
                    });

                    // Set initial tray status
                    let init_status = {
                        let s = remote_st.tunnel_status.lock().unwrap();
                        match &*s {
                            remote::TunnelStatus::Connected => "Connected",
                            remote::TunnelStatus::Connecting => "Connecting",
                            remote::TunnelStatus::Disconnected => "Disconnected",
                            remote::TunnelStatus::Error(_) => "Error",
                        }
                        .to_string()
                    };
                    update_tray_status(&handle, &init_status);
                }
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| match event {
            RunEvent::ExitRequested { api, code, .. } => {
                log::warn!("[EXIT] ExitRequested: code={code:?}");
                if code.is_none() {
                    api.prevent_exit();
                } else {
                    log::error!("[EXIT] App exiting with code={code:?}");
                }
            }
            RunEvent::Exit => {
                log::warn!("[EXIT] App is shutting down (RunEvent::Exit)");
                // Kill SSH tunnel process on exit
                if let Some(remote_st) = app.try_state::<RemoteState>() {
                    // Prevent watchdog from auto-reconnecting
                    *remote_st.user_disconnected.lock().unwrap() = true;

                    let status = remote_st.tunnel_status.clone();
                    let mut guard = remote_st.ssh_tunnel.lock().unwrap();
                    if let Some(ref mut tunnel) = *guard {
                        log::info!("[EXIT] Disconnecting SSH tunnel before shutdown");
                        tunnel.disconnect(status);
                    }
                    // Remove tunnel entirely so watchdog cannot reconnect
                    *guard = None;
                }
            }
            _ => {}
        });
}
