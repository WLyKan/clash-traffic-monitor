#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use monitor_runtime::{
    backend::{DashboardRequest, DashboardSnapshot, RankRow, Runtime, Settings, TargetRequest},
    cli::{Options, HELP},
    instance::{focus_existing, InstanceServer},
    paths::resolve_data_dir,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::Duration,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, State,
};
use tauri_plugin_notification::NotificationExt;

struct FrontendStatus {
    mode: &'static str,
    has_index: bool,
    has_script: bool,
}

// App::run exits the process without dropping managed state. Keep explicit,
// idempotent shutdown independent of that state (and its AppHandle callbacks).
struct DesktopWorkers {
    exiting: Arc<AtomicBool>,
    focus: Mutex<Option<InstanceServer>>,
    alerts: Mutex<Option<JoinHandle<()>>>,
}
impl DesktopWorkers {
    fn new(exiting: Arc<AtomicBool>) -> Self {
        Self {
            exiting,
            focus: Mutex::new(None),
            alerts: Mutex::new(None),
        }
    }

    fn shutdown(&self) {
        self.exiting.store(true, Ordering::Release);
        // Dropping the server joins its callback thread and removes its own
        // endpoint. Release the state mutex before invoking that Drop.
        let focus = self.focus.lock().unwrap().take();
        drop(focus);
        let alerts = self.alerts.lock().unwrap().take();
        if let Some(handle) = alerts {
            let _ = handle.join();
        }
    }
}

#[tauri::command]
async fn get_dashboard(
    runtime: State<'_, Arc<Runtime>>,
    request: DashboardRequest,
) -> Result<DashboardSnapshot, String> {
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.dashboard(request))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn get_targets(
    runtime: State<'_, Arc<Runtime>>,
    request: TargetRequest,
) -> Result<Vec<RankRow>, String> {
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.targets(request))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn get_settings(runtime: State<'_, Arc<Runtime>>) -> Result<Settings, String> {
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.get_settings())
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn save_settings(runtime: State<'_, Arc<Runtime>>, settings: Settings) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || runtime.save_settings(settings))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
fn get_diagnostics(
    runtime: State<'_, Arc<Runtime>>,
    frontend: State<'_, FrontendStatus>,
) -> serde_json::Value {
    let mut report = runtime.diagnostics();
    report["frontendMode"] = serde_json::json!(frontend.mode);
    report["embeddedIndex"] = serde_json::json!(frontend.has_index);
    report["embeddedScript"] = serde_json::json!(frontend.has_script);
    report
}

fn frontend_mode(
    dev: bool,
    dist: Option<&tauri::utils::config::FrontendDist>,
    has_index: bool,
    has_script: bool,
) -> &'static str {
    if dev {
        "development-server"
    } else if matches!(dist, Some(tauri::utils::config::FrontendDist::Url(_))) {
        "external-url"
    } else if !has_index || !has_script {
        "missing-assets"
    } else {
        "embedded"
    }
}

fn show_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}
fn open_alert(app: &tauri::AppHandle, peak_at: f64) {
    show_window(app);
    let _ = app.emit("open-alert", serde_json::json!({"peakAt":peak_at}));
}

fn run() -> Result<i32, String> {
    let options = Options::parse(std::env::args_os().skip(1))?;
    if options.help {
        output(HELP, false);
        return Ok(0);
    }
    if options.version {
        output(
            &format!(
                "Clash 流量监控 {} ({}, dirty={})",
                env!("CARGO_PKG_VERSION"),
                env!("CLASH_BUILD_COMMIT"),
                env!("CLASH_BUILD_DIRTY")
            ),
            false,
        );
        return Ok(0);
    }
    let directory = resolve_data_dir(options.data_dir.as_deref())?;
    let context = tauri::generate_context!();
    let has_index = context.assets().get(&"index.html".into()).is_some();
    let has_script = context.assets().iter().any(|(key, _)| key.ends_with(".js"));
    let frontend = FrontendStatus {
        mode: frontend_mode(
            cfg!(dev),
            context.config().build.frontend_dist.as_ref(),
            has_index,
            has_script,
        ),
        has_index,
        has_script,
    };
    if options.diagnostics {
        let mut report = monitor_runtime::cli::diagnostics(&directory)?;
        report["build_commit"] = serde_json::json!(env!("CLASH_BUILD_COMMIT"));
        report["build_dirty"] = serde_json::json!(env!("CLASH_BUILD_DIRTY") == "true");
        report["frontend_mode"] = serde_json::json!(frontend.mode);
        report["embedded_index"] = serde_json::json!(frontend.has_index);
        report["embedded_script"] = serde_json::json!(frontend.has_script);
        output(
            &serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?,
            false,
        );
        return Ok(if report.get("database_error").is_some() {
            1
        } else {
            0
        });
    }
    if options.check {
        let snapshot = monitor_core::MihomoSource::from_env()?.sample()?;
        let proxy: Vec<_> = snapshot
            .connections
            .iter()
            .filter(|c| c.route == monitor_core::Route::Proxy)
            .collect();
        output(
            &format!(
                "Mihomo 已连接。活跃连接 {} 条，代理 {} 条，已识别应用的代理连接 {} 条。",
                snapshot.connections.len(),
                proxy.len(),
                proxy.iter().filter(|c| c.app.is_some()).count()
            ),
            false,
        );
        return Ok(0);
    }
    let runtime = match Runtime::start(directory.clone()) {
        Ok(runtime) => runtime,
        Err(error) => {
            if error.contains("已有采集实例") {
                // A second launch while the first WebView is still starting may
                // arrive before its focus endpoint; never bypass the writer lock.
                let mut focused = false;
                for _ in 0..10 {
                    if focus_existing(&directory) {
                        focused = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(150));
                }
                if !focused {
                    output("流量监控已在运行，请从 Windows 通知区域打开。", false);
                }
                return Ok(0);
            }
            return Err(error);
        }
    };
    let exiting = Arc::new(AtomicBool::new(false));
    let desktop_workers = Arc::new(DesktopWorkers::new(exiting.clone()));
    let setup_workers = desktop_workers.clone();
    let exit_flag = exiting.clone();
    let setup_runtime = runtime.clone();
    let application = match tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .manage(runtime.clone())
        .manage(desktop_workers.clone())
        .manage(frontend)
        .invoke_handler(tauri::generate_handler![
            get_dashboard,
            get_targets,
            get_settings,
            save_settings,
            get_diagnostics
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(move |app| {
            // Tauri invokes setup during Ready, after Builder::build returns.
            // Clean partial setup here before Tauri handles the returned error.
            let result: Result<(), Box<dyn std::error::Error>> = (|| {
                let config = app
                    .config()
                    .app
                    .windows
                    .iter()
                    .find(|window| window.label == "main")
                    .ok_or_else(|| std::io::Error::other("缺少主窗口配置"))?;
                tauri::WebviewWindowBuilder::from_config(app, config)?
                    .data_directory(setup_runtime.directory.join("webview"))
                    .build()?;
                let show = MenuItem::with_id(app, "show", "打开面板", true, None::<&str>)?;
                let latest =
                    MenuItem::with_id(app, "latest", "查看最近流量提醒", true, None::<&str>)?;
                let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&show, &latest, &quit])?;
                let mut tray = TrayIconBuilder::with_id("monitor")
                    .tooltip("Clash 流量监控")
                    .menu(&menu)
                    .show_menu_on_left_click(false)
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "show" => show_window(app),
                        "latest" => {
                            let runtime = app.state::<Arc<Runtime>>();
                            if let Some(alert) = runtime.latest_alert() {
                                open_alert(app, alert.peak_at)
                            } else {
                                show_window(app)
                            }
                        }
                        "quit" => app.exit(0),
                        _ => {}
                    })
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            show_window(tray.app_handle());
                        }
                    });
                if let Some(icon) = app.default_window_icon() {
                    tray = tray.icon(icon.clone());
                }
                tray.build(app)?;
                let handle = app.handle().clone();
                let focus =
                    InstanceServer::start(&setup_runtime.directory, move || show_window(&handle))
                        .map_err(std::io::Error::other)?;
                *app.state::<Arc<DesktopWorkers>>().focus.lock().unwrap() = Some(focus);
                let action_app = app.handle().clone();
                app.notification().on_action(move |event| {
                    if let Some(peak) = event
                        .notification()
                        .and_then(|n| n.extra().get("peakAt"))
                        .and_then(|v| v.as_f64())
                    {
                        open_alert(&action_app, peak);
                    }
                })?;
                let handle = app.handle().clone();
                let background = setup_runtime.clone();
                let stopping = exit_flag.clone();
                let alerts = std::thread::Builder::new()
                    .name("desktop-alerts".into())
                    .spawn(move || {
                        let mut last_revision = 0;
                        let mut last_event = std::time::Instant::now();
                        while !stopping.load(Ordering::Acquire) {
                            let revision = background.revision();
                            if revision != last_revision
                                && last_event.elapsed() >= Duration::from_secs(2)
                            {
                                let _ = handle
                                    .emit("data_updated", serde_json::json!({"revision":revision}));
                                last_revision = revision;
                                last_event = std::time::Instant::now();
                            }
                            for alert in background.take_alerts() {
                                let body = monitor_runtime::backend::alert_body(&alert);
                                let _ = handle
                                    .notification()
                                    .builder()
                                    .title("代理流量提醒")
                                    .body(body)
                                    .extra("peakAt", alert.peak_at)
                                    .show();
                            }
                            std::thread::sleep(Duration::from_millis(400));
                        }
                    })?;
                *app.state::<Arc<DesktopWorkers>>().alerts.lock().unwrap() = Some(alerts);
                if !options.start_hidden {
                    show_window(app.handle());
                }
                Ok(())
            })();
            if result.is_err() {
                setup_workers.shutdown();
                setup_runtime.shutdown();
            }
            result
        })
        .build(context)
    {
        Ok(application) => application,
        Err(error) => {
            desktop_workers.shutdown();
            runtime.shutdown();
            return Err(error.to_string());
        }
    };
    let on_exit = runtime.clone();
    application.run(move |app, event| {
        if let tauri::RunEvent::Exit = event {
            exiting.store(true, Ordering::Release);
            app.state::<Arc<DesktopWorkers>>().shutdown();
            on_exit.shutdown();
        }
    });
    // App::run never returns; all worker/transaction cleanup is in Exit above.
    Ok(0)
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            output(&error, true);
            std::process::exit(1)
        }
    }
}

fn output(message: &str, error: bool) {
    #[cfg(windows)]
    unsafe {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetStdHandle(kind: u32) -> isize;
            fn WriteFile(
                file: isize,
                data: *const u8,
                size: u32,
                written: *mut u32,
                overlapped: *mut std::ffi::c_void,
            ) -> i32;
        }
        #[link(name = "user32")]
        unsafe extern "system" {
            fn MessageBoxW(window: isize, text: *const u16, title: *const u16, flags: u32) -> i32;
        }
        let bytes = format!("{message}\n");
        let handle = GetStdHandle(if error {
            (-12i32) as u32
        } else {
            (-11i32) as u32
        });
        if handle != 0 && handle != -1 {
            let mut written = 0;
            if WriteFile(
                handle,
                bytes.as_ptr(),
                bytes.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            ) != 0
                && written == bytes.len() as u32
            {
                return;
            }
        }
        let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
        let title: Vec<u16> = "Clash 流量监控".encode_utf16().chain(Some(0)).collect();
        MessageBoxW(
            0,
            text.as_ptr(),
            title.as_ptr(),
            if error { 0x10 } else { 0x40 },
        );
    }
    #[cfg(not(windows))]
    {
        if error {
            eprintln!("{message}")
        } else {
            println!("{message}")
        }
    }
}

#[cfg(test)]
mod frontend_asset_tests {
    use super::*;
    use tauri::utils::config::FrontendDist;

    #[test]
    fn windows_drive_url_is_never_reported_as_embedded() {
        let dist: FrontendDist =
            serde_json::from_value(serde_json::json!(r"D:\probe\web")).unwrap();
        assert!(matches!(&dist, FrontendDist::Url(url) if url.scheme() == "d"));
        assert_eq!(
            frontend_mode(false, Some(&dist), false, false),
            "external-url"
        );
    }

    #[test]
    fn windows_directory_array_requires_actual_index_and_script_assets() {
        let dist: FrontendDist =
            serde_json::from_value(serde_json::json!([r"D:\probe\web"])).unwrap();
        assert!(
            matches!(&dist, FrontendDist::Files(paths) if paths == &[std::path::PathBuf::from(r"D:\probe\web")])
        );
        assert_eq!(frontend_mode(false, Some(&dist), true, true), "embedded");
        for (index, script) in [(false, false), (true, false), (false, true)] {
            assert_eq!(
                frontend_mode(false, Some(&dist), index, script),
                "missing-assets"
            );
        }
    }

    #[test]
    fn development_server_and_missing_distribution_are_not_embedded() {
        assert_eq!(
            frontend_mode(true, None, false, false),
            "development-server"
        );
        assert_eq!(frontend_mode(false, None, false, false), "missing-assets");
    }
}

#[cfg(test)]
mod desktop_lifecycle_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Lifetime(Arc<AtomicBool>);
    impl Lifetime {
        fn observe(&self) {
            assert!(!self.0.load(Ordering::Acquire));
        }
    }
    impl Drop for Lifetime {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[test]
    fn explicit_shutdown_breaks_managed_callback_cycle_and_cleans_endpoint() {
        let directory = tempfile::tempdir().unwrap();
        let owner = Arc::new(DesktopWorkers::new(Arc::new(AtomicBool::new(false))));
        let weak = Arc::downgrade(&owner);
        let focus_exited = Arc::new(AtomicBool::new(false));
        let alerts_exited = Arc::new(AtomicBool::new(false));
        let focus_calls = Arc::new(AtomicUsize::new(0));

        // The callback holds its managed owner strongly, just as AppHandle
        // holds Tauri's manager and the manager owns DesktopWorkers.
        let callback_owner = owner.clone();
        let lifetime = Lifetime(focus_exited.clone());
        let calls = focus_calls.clone();
        let focus = InstanceServer::start(directory.path(), move || {
            lifetime.observe();
            assert!(!callback_owner.exiting.load(Ordering::Acquire));
            calls.fetch_add(1, Ordering::AcqRel);
        })
        .unwrap();
        *owner.focus.lock().unwrap() = Some(focus);

        let alerts_owner = owner.clone();
        let finished = alerts_exited.clone();
        *owner.alerts.lock().unwrap() = Some(std::thread::spawn(move || {
            while !alerts_owner.exiting.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(5));
            }
            finished.store(true, Ordering::Release);
        }));

        assert!(focus_existing(directory.path()));
        assert_eq!(focus_calls.load(Ordering::Acquire), 1);
        assert!(directory.path().join("instance.json").exists());
        drop(owner);
        let retained = weak.upgrade().expect("callbacks keep managed owner alive");
        retained.shutdown();
        assert!(
            !directory.path().join("instance.json").exists(),
            "explicit shutdown must remove endpoint before process exit"
        );
        assert!(!focus_existing(directory.path()));
        assert!(
            focus_exited.load(Ordering::Acquire),
            "focus callback/thread must be dropped and joined"
        );
        assert!(
            alerts_exited.load(Ordering::Acquire),
            "alert thread must be joined"
        );
        retained.shutdown();
        assert!(retained.focus.lock().unwrap().is_none());
        assert!(retained.alerts.lock().unwrap().is_none());
        drop(retained);
        assert!(
            weak.upgrade().is_none(),
            "explicit cleanup must break callback ownership cycle"
        );
    }

    #[test]
    fn shutdown_is_safe_before_workers_exist() {
        let workers = DesktopWorkers::new(Arc::new(AtomicBool::new(false)));
        workers.shutdown();
        workers.shutdown();
        assert!(workers.exiting.load(Ordering::Acquire));
    }
}
