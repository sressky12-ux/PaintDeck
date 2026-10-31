// 繪匣 PaintDeck 2.0：桌面外殼。內建伺服器（server::）在 127.0.0.1 提供網頁和 API，主視窗直接開那個網址，
// 所以瀏覽器版和桌面版是同一套介面。這裡只管：搶 port（順便當單一執行個體）、第一次啟動精靈、開 ComfyUI、主視窗、系統列、關閉行為。
//
// 資料位置記在 %APPDATA%\local.anima.studio\config.json（第一次啟動精靈寫的）。開發、測試時可以用環境變數蓋過：
//   ANIMA_ROOT（資料根目錄）、COMFY_ROOT（ComfyUI 資料夾，ComfyUI_windows_portable\ComfyUI）、ANIMA_WEB（網頁檔）、
//   ANIMA_RES（程式附帶的檔案）、ANIMA_CONFIG（設定檔位置）、ANIMA_PORT（預設 8190）、COMFY_URL（預設 http://127.0.0.1:8188）
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // 正式版不開黑視窗

use anima_app::launcher::ComfyProc;
use anima_app::server;
use anima_app::setup::{self, Config, Setup};
use axum::routing::{get, post};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    webview::NewWindowResponse,
    AppHandle, Emitter, Manager, RunEvent, Url, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};

const TITLE: &str = "繪匣 PaintDeck";

/// 這個程式開的 ComfyUI（結束時要關）
struct Comfy(Mutex<Option<ComfyProc>>);

fn main() {
    // 解除安裝時 NSIS 叫的（nsis\hooks.nsh）：問要不要連資料一起刪，做完就結束，不搶 port、不開視窗
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--uninstall-cleanup") {
        std::process::exit(anima_app::uninstall::run_cli(&args));
    }
    let port: u16 = std::env::var("ANIMA_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8190);
    let (listener, port) = take_port(port);
    let home: Url = format!("http://127.0.0.1:{port}/")
        .parse()
        .expect("網址格式");

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![hide_window, quit_app, open_assistant])
        .setup(move |app| {
            app.manage(Comfy(Mutex::new(None)));
            let web = env_path("ANIMA_WEB")
                .map(Ok)
                .unwrap_or_else(|| bundled(app, "ui"))
                .unwrap_or_else(|e| fail(&e));
            let res = env_path("ANIMA_RES")
                .map(Ok)
                .unwrap_or_else(|| bundled(app, "resources"))
                .unwrap_or_else(|e| fail(&e));
            let cfg_path = env_path("ANIMA_CONFIG").map(Ok).unwrap_or_else(|| {
                app.path()
                    .app_config_dir()
                    .map(|d| d.join("config.json"))
                    .map_err(|e| format!("找不到設定資料夾：{e}"))
            });
            let wizard = Setup::new(&cfg_path.unwrap_or_else(|e| fail(&e)), &res);
            let handle = app.handle().clone();
            match env_config()
                .or_else(|| wizard.config())
                .filter(|c| c.problem().is_none())
            {
                Some(cfg) => {
                    tauri::async_runtime::block_on(start_main(handle, cfg, wizard, web, listener))
                }
                None => start_wizard(handle, wizard, web, listener, port),
            }

            let local = home.clone();
            let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(home.clone()))
                .title(TITLE)
                .inner_size(1440.0, 900.0)
                .min_inner_size(1000.0, 640.0)
                .decorations(false)
                .center()
                // 內建伺服器以外的網址（Civitai、ComfyUI 節點介面…）交給預設瀏覽器，不在這個視窗裡換頁
                .on_navigation(move |u| {
                    let mine = u.origin() == local.origin();
                    if !mine {
                        open_external(u);
                    }
                    mine
                })
                .on_new_window(|u, _| {
                    open_external(&u);
                    NewWindowResponse::Deny
                })
                .build()?;
            fit_window(&win);

            let show = MenuItem::with_id(app, "show", "開啟主視窗", true, None::<&str>)?;
            let browser = MenuItem::with_id(app, "browser", "用瀏覽器開啟", true, None::<&str>)?;
            let repair = MenuItem::with_id(app, "setup", "安裝與修復…", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "結束", true, None::<&str>)?;
            let sep = || PredefinedMenuItem::separator(app);
            let menu = Menu::with_items(app, &[&show, &browser, &sep()?, &repair, &sep()?, &quit])?;
            let url = home.clone();
            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().cloned().ok_or("找不到程式圖示")?)
                .tooltip(TITLE)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, ev| match ev.id.as_ref() {
                    "show" => show_main(app),
                    "browser" => open_external(&url),
                    "setup" => {
                        if let (Some(w), Ok(u)) =
                            (app.get_webview_window("main"), url.join("setup"))
                        {
                            let _ = w.navigate(u);
                        }
                        show_main(app);
                    }
                    "quit" => confirm_quit(app),
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = ev
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;
            Ok(())
        })
        // 按關閉（含 Alt+F4）先不關，交給網頁問要縮到系統列還是結束（ui\js\deck.js）
        .on_window_event(|w, ev| {
            if let WindowEvent::CloseRequested { api, .. } = ev {
                if w.label() == "main" {
                    api.prevent_close();
                    let _ = w.emit("anima-close", ());
                }
            }
        })
        .build(tauri::generate_context!())
        .unwrap_or_else(|e| fail(&format!("{TITLE}啟動失敗：{e}")));
    app.run(|app, ev| {
        if let RunEvent::Exit = ev {
            if let Some(p) = app
                .state::<Comfy>()
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            {
                p.stop();
            }
        }
    });
}

#[tauri::command]
fn hide_window(window: tauri::WebviewWindow) {
    let _ = window.hide();
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

/// 頂列的「✦ 助手」：生圖助手開在自己的視窗（同一個內建伺服器的網頁加 ?solo=agent，只顯示助手頁）。
/// 已經開著就叫到前面，不開第二個。
/// 一定要 async：Windows 上在同步指令裡建 WebviewWindow 會卡死（2026-10-26 實測新視窗全白、關不掉），Tauri 文件也這樣寫
#[tauri::command]
async fn open_assistant(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("assistant") {
        let _ = w.unminimize();
        let _ = w.show();
        return w.set_focus().map_err(|e| e.to_string());
    }
    let main = app.get_webview_window("main").ok_or("找不到主視窗")?;
    let home = main.url().map_err(|e| e.to_string())?;
    let url = home.join("/?solo=agent#agent").map_err(|e| e.to_string())?;
    let local = url.clone();
    WebviewWindowBuilder::new(&app, "assistant", WebviewUrl::External(url))
        .title(format!("{TITLE}｜助手"))
        .inner_size(560.0, 860.0)
        .min_inner_size(420.0, 560.0)
        .on_navigation(move |u| {
            let mine = u.origin() == local.origin();
            if !mine {
                open_external(u);
            }
            mine
        })
        .on_new_window(|u, _| {
            open_external(&u);
            NewWindowResponse::Deny
        })
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// 系統列的「結束」：先用 Windows 的對話框確認一次（容易不小心關掉）。
/// 對話框會卡住呼叫它的執行緒，所以開新的執行緒問；已經在問了就不再開第二個
fn confirm_quit(app: &AppHandle) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static ASKING: AtomicBool = AtomicBool::new(false);
    if ASKING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let yes = ask_yes_no("確定要結束生圖站嗎？\n\n這個程式幫你開的 ComfyUI 也會一起關掉，排隊中的圖不會畫完。");
        ASKING.store(false, Ordering::SeqCst);
        if yes {
            app.exit(0);
        }
    });
}

/// 是／否對話框，預設按鈕是「否」
fn ask_yes_no(msg: &str) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            MessageBoxW, IDYES, MB_DEFBUTTON2, MB_ICONQUESTION, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
        };
        let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (text, title) = (w(msg), w(TITLE));
        let r = unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2 | MB_TOPMOST | MB_SETFOREGROUND,
            )
        };
        r == IDYES
    }
    #[cfg(not(windows))]
    {
        let _ = msg;
        true
    }
}

/// 1440×900 放不下的螢幕（小筆電、放大 125%／150% 的）：縮成螢幕的九成左右再置中，最小尺寸也跟著放寬
fn fit_window(w: &tauri::WebviewWindow) {
    let Ok(Some(m)) = w.current_monitor() else { return };
    let size = m.size().to_logical::<f64>(m.scale_factor());
    // 工作列大約占 48 像素
    let (sw, sh) = (size.width, (size.height - 48.0).max(400.0));
    let (ww, wh) = ((sw * 0.94).min(1440.0), (sh * 0.94).min(900.0));
    if ww < 1440.0 || wh < 900.0 {
        let _ = w.set_min_size(Some(tauri::LogicalSize::new(ww.min(1000.0), wh.min(640.0))));
        let _ = w.set_size(tauri::LogicalSize::new(ww, wh));
        let _ = w.center();
    }
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

// ---------------------------------------------------------------- 兩種模式
/// 正式畫面：開內建伺服器（含精靈的 API，給「安裝與修復」用），ComfyUI 沒開就在背景開
async fn start_main(
    handle: AppHandle,
    cfg: Config,
    wizard: Arc<Setup>,
    web: PathBuf,
    listener: std::net::TcpListener,
) {
    let paths = cfg.paths(&web);
    let comfy_url = std::env::var("COMFY_URL").unwrap_or_else(|_| {
        let prefs = anima_app::gpu::prefs();
        if prefs.port != 0 {
            return format!("http://127.0.0.1:{}", prefs.port);
        }
        // 8188 沒人用、但同一份 ComfyUI 已經被別的啟動器開在別的 port（官方桌面版 8000…）：直接用它，不再開第二個。
        // 8188 被「確定是別份」的 ComfyUI 占著（例如另一套整合包）：生圖站讀寫的是自己這份的 input／output，連過去圖庫會收不到圖，
        // 所以自己這份開在別的空 port。「我自己開」的不換。用另一條執行緒問（這裡在 async 裡，不能直接用 blocking 的 HTTP client）
        let dir = cfg.comfy_dir.clone();
        let found = std::thread::spawn(move || {
            use anima_app::launcher::{find_running, other_install_on, port_open};
            if !port_open(8188) {
                find_running(&dir, &[8000, 8189, 8288])
            } else if prefs.external || !other_install_on(&dir, 8188) {
                None
            } else {
                eprintln!("8188 是另一份 ComfyUI，生圖站自己這份改開在別的連接埠");
                find_running(&dir, &[8189, 8288, 8388]).or_else(|| [8189, 8288, 8388, 8488].into_iter().find(|p| !port_open(*p)))
            }
        })
        .join()
        .ok()
        .flatten();
        match found {
            Some(p) => {
                eprintln!("ComfyUI 用 {p}");
                format!("http://127.0.0.1:{p}")
            }
            None => "http://127.0.0.1:8188".into(),
        }
    });
    let shared = server::start(paths.clone(), &comfy_url)
        .await
        .unwrap_or_else(|e| fail(&e));
    handle.manage(shared.updates.clone());
    let h = handle.clone();
    shared.updates.bind(handle.clone(), move || {
        if let Some(p) = h.state::<Comfy>().0.lock().unwrap_or_else(|e| e.into_inner()).take() { p.stop(); }
    });
    anima_app::server::attach_setup(&shared, wizard.clone());
    let weak = Arc::downgrade(&shared);
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(8)).await;
        loop {
            let Some(a) = weak.upgrade() else { break };
            if a.updates.auto_check() { a.updates.check().await; }
            drop(a);
            tokio::time::sleep(Duration::from_secs(12 * 60 * 60)).await;
        }
    });
    let h = handle.clone();
    let _ = shared.show_window.set(Box::new(move || show_main(&h)));
    // 精靈完成：位置沒換＝回主畫面；換了＝重開程式（伺服器要用新的資料夾）
    let (h, old) = (handle.clone(), cfg.clone());
    let restarting = std::sync::atomic::AtomicBool::new(false);
    wizard.set_on_finish(move |new: &Config| {
        if *new == old {
            return "reload";
        }
        if !restarting.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let h = h.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(500)); // 先讓網頁收到回應
                std::env::set_var("ANIMA_RESTART", "1"); // 新的程式要等舊的放掉 port
                h.restart();
            });
        }
        "restart"
    });
    // ComfyUI：用 COMFY_URL 的 port 判斷有沒有在跑
    let comfy_port = comfy_url
        .parse::<Url>()
        .ok()
        .and_then(|u| u.port())
        .unwrap_or(8188);
    let (dir, logs) = (cfg.comfy_dir.clone(), paths.data.join("logs"));
    // 設定頁「重開 ComfyUI」（外部模型資料夾改了要重開才讀得到）：關掉自己開的那個，等 port 放掉再開
    let (h, d, l) = (handle.clone(), dir.clone(), logs.clone());
    let restart_diagnostics=shared.diagnostics.clone();
    let _ = shared.restart_comfy.set(Box::new(move || {
        let old = h
            .state::<Comfy>()
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        match old {
            Some(old) => old.stop(),
            // 沒有在跑（上次沒開起來、或剛把「我自己開 ComfyUI」關掉）：直接開
            None if !anima_app::launcher::port_open(comfy_port) => {}
            None => return Err("這個 ComfyUI 不是生圖站開的（或還沒開好）：請自己把 ComfyUI 關掉再開一次".into()),
        }
        let (h, d, l) = (h.clone(), d.clone(), l.clone());
        let diagnostics=restart_diagnostics.clone();
        std::thread::spawn(move || {
            for _ in 0..60 {
                if !anima_app::launcher::port_open(comfy_port) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            match ComfyProc::start_if_needed(&d, &l, comfy_port) {
                Ok(Some(p)) => {
                    p.attach_diagnostics(diagnostics);
                    *h.state::<Comfy>().0.lock().unwrap_or_else(|e| e.into_inner()) = Some(p)
                }
                Ok(None) => eprintln!("重開 ComfyUI：已經有別的 ComfyUI 開著"),
                Err(e) => {
                    eprintln!("重開 ComfyUI 失敗：{e}");
                    anima_app::launcher::set_note(Some(format!("重開 ComfyUI 失敗：{e}")));
                }
            }
        });
        Ok(())
    }));
    // 安裝與修復換 A 卡的 PyTorch 時：先關掉生圖站開的 ComfyUI（DLL 不能被占用），裝完再開
    let (h1, h2, d, l) = (handle.clone(), handle.clone(), dir.clone(), logs.clone());
    let hook_diagnostics=shared.diagnostics.clone();
    wizard.set_comfy_hooks(
        move || {
            if let Some(p) = h1.state::<Comfy>().0.lock().unwrap_or_else(|e| e.into_inner()).take() {
                p.stop();
            }
        },
        move || match ComfyProc::start_if_needed(&d, &l, comfy_port) {
            Ok(Some(p)) => {p.attach_diagnostics(hook_diagnostics.clone()); *h2.state::<Comfy>().0.lock().unwrap_or_else(|e| e.into_inner()) = Some(p)},
            Ok(None) => {}
            Err(e) => anima_app::launcher::set_note(Some(format!("ComfyUI 沒開起來：{e}"))),
        },
    );
    let setup_router = setup::router(wizard, &web)
        .layer(axum::middleware::from_fn_with_state(shared.clone(), server::update_gate));
    let diagnostics=shared.diagnostics.clone();
    let router = server::router(shared).merge(setup_router);
    let listener = tokio::net::TcpListener::from_std(listener)
        .unwrap_or_else(|e| fail(&format!("內建伺服器啟動失敗：{e}")));
    tauri::async_runtime::spawn(async move {
        if let Err(e) = server::serve_on(router, listener).await {
            fail(&format!("內建伺服器停止：{e}"));
        }
    });
    let res = paths.res.clone();
    std::thread::spawn(move || {
        // 官方免安裝版還比 COMFY_VERSION 舊：開 ComfyUI 之前自動升級（2026-10-31 使用者：「要確保他們升級後都能很直接地簡單的，
        // 能享受到跟我一樣」）。ComfyUI 已經開著、「我自己開 ComfyUI」的不動；失敗（例如沒網路）照舊用舊版開
        if !anima_app::launcher::port_open(comfy_port) && !anima_app::gpu::prefs().external {
            if anima_app::setup::comfy_upgradable(&dir) {
                anima_app::launcher::set_note(Some(format!("正在把 ComfyUI 升級到 {}（約 1～3 分鐘，只有這一次），好了會自己開", anima_app::setup::COMFY_VERSION)));
            }
            match anima_app::setup::auto_upgrade_comfy(&dir, &res) {
                None => {}
                Some(Ok(())) => {
                    eprintln!("ComfyUI 自動升級到 {} 了", anima_app::setup::COMFY_VERSION);
                    anima_app::launcher::set_note(None);
                }
                Some(Err(e)) => {
                    eprintln!("ComfyUI 自動升級沒成功：{e}");
                    anima_app::launcher::set_note(None);
                }
            }
        }
        // 程式附帶、已經裝進 ComfyUI 的東西（AI 改寫節點、工作流程、MOSS 補丁…）先換成這一版的，再開 ComfyUI
        let updated = anima_app::setup::sync_bundled(&dir, &res);
        match ComfyProc::start_if_needed(&dir, &logs, comfy_port) {
            Ok(Some(p)) => {
                p.attach_diagnostics(diagnostics);
                *handle
                    .state::<Comfy>()
                    .0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(p)
            }
            Ok(None) => {
                eprintln!("ComfyUI 已經開著，直接使用");
                // 開著的 ComfyUI 還是舊的程式：生成頁提醒重開
                anima_app::setup::set_updated(updated);
            }
            Err(e) => {
                eprintln!("{e}");
                // 視窗程式看不到 eprintln：記下來，/api/state 在 ComfyUI 離線時會帶給網頁
                anima_app::launcher::set_note(Some(format!("ComfyUI 沒開起來：{e}")));
            }
        }
    });
}

/// 第一次啟動：只開精靈（同一個 port）。完成後關掉精靈的伺服器、換成正式畫面
fn start_wizard(
    handle: AppHandle,
    wizard: Arc<Setup>,
    web: PathBuf,
    listener: std::net::TcpListener,
    port: u16,
) {
    let done = Arc::new(tokio::sync::Notify::new());
    let d = done.clone();
    wizard.set_on_finish(move |_| {
        d.notify_one();
        "reload"
    });
    let page = web.join("setup.html");
    let h = handle.clone();
    let router = setup::router(wizard.clone(), &web)
        .route("/", get(move || setup::setup_page(page.clone())))
        .route(
            "/api/app/show",
            post(move || {
                show_main(&h);
                async { axum::Json(serde_json::json!({"ok": true, "app": server::APP_ID})) }
            }),
        )
        .nest_service(
            server::static_prefix(),
            tower_http::services::ServeDir::new(&web),
        )
        .nest_service("/static", tower_http::services::ServeDir::new(&web));
    tauri::async_runtime::spawn(async move {
        let l = tokio::net::TcpListener::from_std(listener)
            .unwrap_or_else(|e| fail(&format!("內建伺服器啟動失敗：{e}")));
        eprintln!("第一次啟動精靈：http://127.0.0.1:{port}/");
        let wait = done.clone();
        if let Err(e) = axum::serve(l, router)
            .with_graceful_shutdown(async move { wait.notified().await })
            .await
        {
            fail(&format!("內建伺服器停止：{e}"));
        }
        let Some(cfg) = wizard.config() else {
            fail("精靈完成了，但讀不到設定檔")
        };
        let l = rebind(port).unwrap_or_else(|| fail(&format!("重新開啟 127.0.0.1:{port} 失敗")));
        start_main(handle, cfg, wizard, web, l).await;
    });
}

// ---------------------------------------------------------------- 位置
fn env_path(k: &str) -> Option<PathBuf> {
    std::env::var_os(k)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// 開發、測試：ANIMA_ROOT（＋COMFY_ROOT）直接指定，不用設定檔
fn env_config() -> Option<Config> {
    let root = env_path("ANIMA_ROOT")?;
    let comfy_dir = match env_path("COMFY_ROOT") {
        Some(c) => c.parent().map(Path::to_path_buf).unwrap_or(c),
        None => root
            .parent()
            .unwrap_or(&root)
            .join("ComfyUI_windows_portable"),
    };
    Some(Config {
        data_root: root,
        comfy_dir,
    })
}

/// 程式附帶的資料夾：開發時在 anima_app\（ui、resources），正式版在安裝資料夾裡
fn bundled(app: &tauri::App, name: &str) -> Result<PathBuf, String> {
    if cfg!(debug_assertions) {
        return Ok(Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(name));
    }
    Ok(app
        .path()
        .resource_dir()
        .map_err(|e| format!("找不到程式資料夾：{e}"))?
        .join(name))
}

// ---------------------------------------------------------------- port
/// 搶 port：搶不到時，是自己已經在跑就叫它出來後結束，不是（多半是舊版生圖站）就說明後結束。
/// 「安裝與修復」換位置後重開的那一次（ANIMA_RESTART）要等舊的程式放掉 port
/// 回傳 (listener, 實際用的 port)。port 被別的程式（不是生圖站）占走時，往後找一個空的（8191～8199，跳過 ComfyUI 常用的 8188、8189），
/// 網頁的網址跟著換（網頁存在瀏覽器裡的小設定會是新的一份，不影響資料）
fn take_port(port: u16) -> (std::net::TcpListener, u16) {
    let restarting = std::env::var_os("ANIMA_RESTART").is_some();
    if restarting {
        std::env::remove_var("ANIMA_RESTART");
        if let Some(l) = rebind(port) {
            return (l, port);
        }
    }
    let ready = |l: std::net::TcpListener| {
        l.set_nonblocking(true)
            .unwrap_or_else(|e| fail(&format!("內建伺服器啟動失敗：{e}")));
        l
    };
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => (ready(l), port),
        Err(_) => {
            if wake_running(port) {
                std::process::exit(0);
            }
            let spare = (port.saturating_add(1)..=port.saturating_add(9))
                .filter(|p| ![8188, 8189].contains(p))
                .find_map(|p| std::net::TcpListener::bind(("127.0.0.1", p)).ok().map(|l| (l, p)));
            match spare {
                Some((l, p)) => {
                    eprintln!("127.0.0.1:{port} 被別的程式占用，改用 {p}");
                    (ready(l), p)
                }
                None => fail(&format!("127.0.0.1:{port} 和後面幾個連接埠都被別的程式占用。\n請關掉占用的程式，再開 {TITLE}。")),
            }
        }
    }
}

/// 同一個 port 再搶一次（最多等 15 秒）
fn rebind(port: u16) -> Option<std::net::TcpListener> {
    for _ in 0..60 {
        if let Ok(l) = std::net::TcpListener::bind(("127.0.0.1", port)) {
            l.set_nonblocking(true).ok()?;
            return Some(l);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    None
}

/// port 上是不是已經有一個自己在跑；是的話它會把視窗叫出來
fn wake_running(port: u16) -> bool {
    use std::io::{Read, Write};
    let addr = ([127, 0, 0, 1], port).into();
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    let req = format!("POST /api/app/show HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = Vec::new();
    let _ = s.take(64 * 1024).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).contains(&format!("\"app\":\"{}\"", server::APP_ID))
}

// ---------------------------------------------------------------- Windows
/// 用預設瀏覽器開（只開 http／https，其他網址一律不理）
fn open_external(u: &Url) {
    if !matches!(u.scheme(), "http" | "https") {
        return;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (op, url) = (w("open"), w(u.as_str()));
        unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                op.as_ptr(),
                url.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            );
        }
    }
}

/// 跳出錯誤視窗後結束（正式版沒有黑視窗，eprintln 看不到）
fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (text, title) = (w(msg), w(TITLE));
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    std::process::exit(1)
}
