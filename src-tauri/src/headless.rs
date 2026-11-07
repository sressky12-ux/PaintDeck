//! 無頭版（2026-11-07）：不開視窗、不連 Tauri，給 Linux 純命令列的伺服器用（省下桌面環境和 WebKit 的記憶體）。
//! 使用者原話（轉述 Linux 使用者）：「主要是用 linux 纯命令行能多少腾出点内存」。
//!
//! 跟桌面版（main.rs）一樣的開站流程，只是沒有視窗和系統匣：
//! - 有設定檔（跟桌面版同一份：~/.config/local.anima.studio/config.json）而且能用 → 正式站：自己開 ComfyUI（跟桌面版同一套
//!   launcher，含開站前自動升級、同步附帶節點、別張卡的工作節點），網頁在 http://127.0.0.1:<port>/
//! - 沒有設定檔 → 先跑網頁版安裝精靈（同一個 port），完成後直接換成正式站
//! - 精靈改了資料或 ComfyUI 的位置 → 關掉自己開的 ComfyUI，原地重新執行自己（exec，用新的位置）
//! - Ctrl+C、SIGTERM（systemctl stop）→ 關掉自己開的 ComfyUI 再結束
//!
//! 連線：預設只聽 127.0.0.1，別台電腦用 SSH 通道連（ssh -L 8190:127.0.0.1:8190 使用者@伺服器）。
//! 要在區網直接連：--host 0.0.0.0 --allow-host <這台的 IP 或名字>（伺服器只收列出來的網址，擋 DNS rebinding）；沒有密碼，只在信任的網路用。
use crate::launcher::{self, ComfyProc};
use crate::server;
use crate::setup::{self, Config, Setup};
use axum::routing::get;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct Options {
    /// 要聽的位址（預設 127.0.0.1）
    pub host: String,
    pub port: u16,
    /// 設定檔（資料、ComfyUI 的位置）
    pub config: PathBuf,
    pub web: PathBuf,
    pub res: PathBuf,
    /// 除了 127.0.0.1／localhost 以外，網址可以用的主機名稱（區網 IP、主機名）
    pub allow_hosts: Vec<String>,
}

const USAGE: &str = "繪匣 PaintDeck 無頭版（沒有視窗，用瀏覽器操作）

用法：anima-server [--host 位址] [--port 連接埠] [--allow-host 名稱]… [--config 設定檔]

  --host        要聽的位址，預設 127.0.0.1（只有這台自己連得到）。0.0.0.0＝所有網路卡
  --port        網頁的連接埠，預設 8190
  --allow-host  區網直接連時，網址裡用的 IP 或主機名稱（可以給好幾次）。沒有密碼保護，只在信任的網路用
  --config      設定檔位置，預設 ~/.config/local.anima.studio/config.json（跟桌面版同一份）

別台電腦要用的話，最安全是 SSH 通道：
  ssh -N -L 8190:127.0.0.1:8190 使用者@這台伺服器
然後在自己電腦的瀏覽器開 http://127.0.0.1:8190/

環境變數（給開發和測試用）：ANIMA_WEB、ANIMA_RES、COMFY_URL；設了 ANIMA_ROOT 就是舊的開發模式（不開 ComfyUI、不跑精靈）";

/// 命令列 → Options。--help＝印說明（回傳 Err 裡放說明文字，呼叫的人印出來就結束）
pub fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut o = Options {
        host: std::env::var("ANIMA_HOST").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "127.0.0.1".into()),
        port: std::env::var("ANIMA_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8190),
        config: env_path("ANIMA_CONFIG").unwrap_or_else(default_config),
        web: env_path("ANIMA_WEB").unwrap_or_else(default_web),
        res: crate::paths::default_res(),
        allow_hosts: std::env::var("ANIMA_ALLOW_HOSTS")
            .ok()
            .map(|s| s.split(',').map(|h| h.trim().to_string()).filter(|h| !h.is_empty()).collect())
            .unwrap_or_default(),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} 後面要接值\n\n{USAGE}"));
        match a.as_str() {
            "-h" | "--help" => return Err(USAGE.into()),
            "--host" => o.host = val("--host")?,
            "--port" => o.port = val("--port")?.parse().map_err(|_| "--port 要是數字".to_string())?,
            "--allow-host" => o.allow_hosts.push(val("--allow-host")?),
            "--config" => o.config = PathBuf::from(val("--config")?),
            other => return Err(format!("不認得的參數：{other}\n\n{USAGE}")),
        }
    }
    Ok(o)
}

fn env_path(k: &str) -> Option<PathBuf> {
    std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// 桌面版（Tauri 的 app_config_dir）用的同一份設定檔：Linux ~/.config/local.anima.studio，Windows %APPDATA%\local.anima.studio
pub fn default_config() -> PathBuf {
    let base = if cfg!(windows) {
        env_path("APPDATA")
    } else {
        env_path("XDG_CONFIG_HOME").or_else(|| env_path("HOME").map(|h| h.join(".config")))
    };
    base.unwrap_or_else(|| PathBuf::from(".")).join("local.anima.studio").join("config.json")
}

/// 網頁檔：程式旁邊的 ui（無頭版的壓縮檔、Windows 安裝版）→ Linux 安裝版的 /usr/lib/<產品名>/ui → 開發時的 anima_app/ui
pub fn default_web() -> PathBuf {
    let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("ui")));
    beside
        .into_iter()
        .chain(crate::paths::linux_lib_dir().map(|d| d.join("ui")))
        .find(|d| d.join("index.html").is_file())
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("ui"))
}

/// 自己開的 ComfyUI（主節點＋別張卡）
#[derive(Default)]
struct Procs {
    main: Mutex<Option<ComfyProc>>,
    extras: Mutex<Vec<ComfyProc>>,
}

impl Procs {
    fn stop_main(&self) {
        if let Some(p) = self.main.lock().unwrap_or_else(|e| e.into_inner()).take() {
            p.stop();
        }
    }
    fn stop_extras(&self) {
        for p in std::mem::take(&mut *self.extras.lock().unwrap_or_else(|e| e.into_inner())) {
            p.stop();
        }
    }
    fn stop_all(&self) {
        self.stop_extras();
        self.stop_main();
    }
    /// 設定 → 工作節點裡啟用的本機別張卡（同 main.rs 的 start_extras）
    fn start_extras(&self, dir: &Path, logs: &Path, data: &Path, main_port: u16) {
        let cfg = crate::nodes::Cfg::load(data);
        for n in cfg.nodes.iter().filter(|n| n.enabled && n.kind == "local") {
            let Some(gpu) = n.gpu else { continue };
            let port = crate::nodes::port_of(&n.url).unwrap_or_else(|| crate::nodes::local_port(main_port, gpu));
            match ComfyProc::start_extra(dir, logs, port, gpu, &n.args) {
                Ok(Some(p)) => self.extras.lock().unwrap_or_else(|e| e.into_inner()).push(p),
                Ok(None) => eprintln!("{}：連接埠 {port} 已經有 ComfyUI，直接用", n.name),
                Err(e) => {
                    eprintln!("{} 沒開起來：{e}", n.name);
                    launcher::set_note(Some(format!("「{}」（第 {gpu} 號卡）沒開起來：{e}", n.name)));
                }
            }
        }
    }
}

fn has_local_extras(data: &Path) -> bool {
    crate::nodes::Cfg::load(data).nodes.iter().any(|n| n.enabled && n.kind == "local" && n.gpu.is_some())
}

/// 用新的設定重新執行自己（精靈換了位置）：Linux 用 exec 原地換掉（port 跟著放掉），Windows 開新的再結束
fn restart_self(procs: &Procs) -> ! {
    procs.stop_all();
    std::env::set_var("ANIMA_RESTART", "1");
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("anima-server"));
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let e = std::process::Command::new(&exe).args(&args).exec();
        eprintln!("重新執行 {} 失敗：{e}；請自己再開一次", exe.display());
        std::process::exit(1);
    }
    #[cfg(not(unix))]
    {
        let _ = std::process::Command::new(&exe).args(&args).spawn();
        std::process::exit(0);
    }
}

/// 綁 port：剛重新執行（ANIMA_RESTART）時舊的可能還沒放掉，等一下
async fn bind(host: &str, port: u16) -> Result<tokio::net::TcpListener, String> {
    let restarting = std::env::var_os("ANIMA_RESTART").is_some();
    std::env::remove_var("ANIMA_RESTART");
    let tries = if restarting { 40 } else { 1 };
    let mut last = String::new();
    for i in 0..tries {
        match tokio::net::TcpListener::bind((host, port)).await {
            Ok(l) => return Ok(l),
            Err(e) => last = e.to_string(),
        }
        if i + 1 < tries {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    Err(format!("佔不到 {host}:{port}：{last}（是不是已經開著一個了？換一個用 --port）"))
}

/// Ctrl+C 或 SIGTERM
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn print_urls(o: &Options, wizard: bool) {
    let what = if wizard { "安裝精靈" } else { "繪匣 PaintDeck" };
    eprintln!("{what}：http://127.0.0.1:{}/", o.port);
    for h in &o.allow_hosts {
        eprintln!("        http://{h}:{}/", o.port);
    }
    if matches!(o.host.as_str(), "127.0.0.1" | "localhost" | "::1") {
        eprintln!("別台電腦要用：ssh -N -L {p}:127.0.0.1:{p} 使用者@這台，再開 http://127.0.0.1:{p}/", p = o.port);
    } else if o.allow_hosts.is_empty() {
        eprintln!("注意：聽的是 {}，但沒有用 --allow-host 列出網址，別台電腦用 IP 連會被擋（只收 127.0.0.1／localhost）", o.host);
    } else {
        eprintln!("注意：區網裡的人都能用這個生圖站（沒有密碼），只在信任的網路這樣開");
    }
}

pub async fn run(o: Options) -> Result<(), String> {
    server::allow_hosts(o.allow_hosts.clone());
    let listener = bind(&o.host, o.port).await?;
    let wizard = Setup::new(&o.config, &o.res);
    let procs = Arc::new(Procs::default());
    {
        let p = procs.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            eprintln!("結束：關掉自己開的 ComfyUI…");
            let _ = tokio::task::spawn_blocking(move || p.stop_all()).await;
            std::process::exit(0);
        });
    }
    let cfg = match wizard.config().filter(|c| c.problem().is_none()) {
        Some(c) => c,
        None => {
            if let Some(c) = wizard.config() {
                eprintln!("設定檔 {} 的位置不能用（{}），先開安裝精靈", o.config.display(), c.problem().unwrap_or_default());
            }
            run_wizard(&o, wizard.clone(), listener).await?;
            let c = wizard.config().ok_or("精靈完成了，但讀不到設定檔")?;
            // 精靈的伺服器剛關掉：同一個 port 再開一次
            let l = bind(&o.host, o.port).await?;
            return run_main(o, c, wizard, l, procs).await;
        }
    };
    run_main(o, cfg, wizard, listener, procs).await
}

/// 第一次：只有精靈（同 main.rs 的 start_wizard），完成後回來換正式站
async fn run_wizard(o: &Options, wizard: Arc<Setup>, listener: tokio::net::TcpListener) -> Result<(), String> {
    let done = Arc::new(tokio::sync::Notify::new());
    let d = done.clone();
    wizard.set_on_finish(move |_| {
        d.notify_one();
        "reload"
    });
    let page = o.web.join("setup.html");
    let router = setup::router(wizard, &o.web)
        .route("/", get(move || setup::setup_page(page.clone())))
        .nest_service(server::static_prefix(), tower_http::services::ServeDir::new(&o.web))
        .nest_service("/static", tower_http::services::ServeDir::new(&o.web));
    print_urls(o, true);
    axum::serve(listener, router)
        .with_graceful_shutdown(async move { done.notified().await })
        .await
        .map_err(|e| format!("精靈的伺服器停止：{e}"))
}

/// 決定 ComfyUI 的網址（同 main.rs start_main 開頭那段）
fn comfy_url(cfg: &Config) -> String {
    if let Ok(u) = std::env::var("COMFY_URL") {
        return u;
    }
    let prefs = crate::gpu::prefs();
    if prefs.port != 0 {
        return format!("http://127.0.0.1:{}", prefs.port);
    }
    let dir = &cfg.comfy_dir;
    let found = if !launcher::port_open(8188) {
        launcher::find_running(dir, &[8000, 8189, 8288])
    } else if prefs.external || !launcher::other_install_on(dir, 8188) {
        None
    } else {
        eprintln!("8188 是另一份 ComfyUI，生圖站自己這份改開在別的連接埠");
        launcher::find_running(dir, &[8189, 8288, 8388]).or_else(|| [8189, 8288, 8388, 8488].into_iter().find(|p| !launcher::port_open(*p)))
    };
    match found {
        Some(p) => {
            eprintln!("ComfyUI 用 {p}");
            format!("http://127.0.0.1:{p}")
        }
        None => "http://127.0.0.1:8188".into(),
    }
}

async fn run_main(o: Options, cfg: Config, wizard: Arc<Setup>, listener: tokio::net::TcpListener, procs: Arc<Procs>) -> Result<(), String> {
    let paths = cfg.paths(&o.web);
    let c = cfg.clone();
    let comfy_url = tokio::task::spawn_blocking(move || comfy_url(&c)).await.map_err(|e| e.to_string())?;
    let shared = server::start(paths.clone(), &comfy_url).await?;
    server::attach_setup(&shared, wizard.clone());
    // 精靈完成：位置沒換＝回主畫面；換了＝重新執行自己（伺服器要用新的資料夾）
    let (old, p) = (cfg.clone(), procs.clone());
    wizard.set_on_finish(move |new: &Config| {
        if *new == old {
            return "reload";
        }
        let p = p.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500)); // 先讓網頁收到回應
            restart_self(&p);
        });
        "restart"
    });
    let comfy_port = comfy_url.parse::<reqwest::Url>().ok().and_then(|u| u.port()).unwrap_or(8188);
    let (dir, logs, data) = (cfg.comfy_dir.clone(), paths.data.join("logs"), paths.data.clone());
    // 設定頁「重開 ComfyUI」
    {
        let (p, d, l, data, diag) = (procs.clone(), dir.clone(), logs.clone(), data.clone(), shared.diagnostics.clone());
        let _ = shared.restart_comfy.set(Box::new(move || {
            p.stop_extras();
            let had = p.main.lock().unwrap_or_else(|e| e.into_inner()).take();
            match had {
                Some(old) => old.stop(),
                None if !launcher::port_open(comfy_port) => {}
                None => return Err("這個 ComfyUI 不是生圖站開的（或還沒開好）：請自己把 ComfyUI 關掉再開一次".into()),
            }
            let (p, d, l, data, diag) = (p.clone(), d.clone(), l.clone(), data.clone(), diag.clone());
            std::thread::spawn(move || {
                launcher::set_pci_order(has_local_extras(&data));
                for _ in 0..60 {
                    if !launcher::port_open(comfy_port) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                match ComfyProc::start_if_needed(&d, &l, comfy_port) {
                    Ok(Some(n)) => {
                        n.attach_diagnostics(diag);
                        *p.main.lock().unwrap_or_else(|e| e.into_inner()) = Some(n);
                    }
                    Ok(None) => eprintln!("重開 ComfyUI：已經有別的 ComfyUI 開著"),
                    Err(e) => {
                        eprintln!("重開 ComfyUI 失敗：{e}");
                        launcher::set_note(Some(format!("重開 ComfyUI 失敗：{e}")));
                    }
                }
                std::thread::sleep(Duration::from_secs(2));
                p.start_extras(&d, &l, &data, comfy_port);
            });
            Ok(())
        }));
    }
    // 安裝與修復要換 PyTorch 時：先關掉自己開的 ComfyUI，裝完再開
    {
        let (p1, p2, d, l, data, diag) = (procs.clone(), procs.clone(), dir.clone(), logs.clone(), data.clone(), shared.diagnostics.clone());
        wizard.set_comfy_hooks(
            move || p1.stop_all(),
            move || {
                match ComfyProc::start_if_needed(&d, &l, comfy_port) {
                    Ok(Some(n)) => {
                        n.attach_diagnostics(diag.clone());
                        *p2.main.lock().unwrap_or_else(|e| e.into_inner()) = Some(n);
                    }
                    Ok(None) => {}
                    Err(e) => launcher::set_note(Some(format!("ComfyUI 沒開起來：{e}"))),
                }
                p2.start_extras(&d, &l, &data, comfy_port);
            },
        );
    }
    let setup_router = setup::router(wizard, &o.web).layer(axum::middleware::from_fn_with_state(shared.clone(), server::update_gate));
    let diagnostics = shared.diagnostics.clone();
    let router = server::router(shared).merge(setup_router);
    // ComfyUI：開站前同 main.rs（自動升級、A 卡 PyTorch 檢查、同步附帶的節點），再開
    {
        let (p, res) = (procs.clone(), paths.res.clone());
        std::thread::spawn(move || {
            launcher::set_pci_order(has_local_extras(&data));
            if !launcher::port_open(comfy_port) && !crate::gpu::prefs().external {
                if setup::comfy_upgradable(&dir) {
                    launcher::set_note(Some(format!("正在把 ComfyUI 升級到 {}（約 1～3 分鐘，只有這一次），好了會自己開", setup::COMFY_VERSION)));
                }
                match setup::auto_upgrade_comfy(&dir, &res) {
                    None => {}
                    Some(Ok(())) => {
                        eprintln!("ComfyUI 自動升級到 {} 了", setup::COMFY_VERSION);
                        launcher::set_note(None);
                    }
                    Some(Err(e)) => {
                        eprintln!("ComfyUI 自動升級沒成功：{e}");
                        launcher::set_note(None);
                    }
                }
                if let Some(r) = setup::auto_fix_rocm10(&dir) {
                    let m = r.unwrap_or_else(|e| e);
                    eprintln!("A 卡 PyTorch 檢查：{m}");
                    launcher::log_note(format!("A 卡 PyTorch 檢查：{m}"));
                }
            }
            let updated = setup::sync_bundled(&dir, &res);
            match ComfyProc::start_if_needed(&dir, &logs, comfy_port) {
                Ok(Some(n)) => {
                    n.attach_diagnostics(diagnostics);
                    eprintln!("ComfyUI 開在 {comfy_port}（log：{}）", logs.join("comfyui.log").display());
                    *p.main.lock().unwrap_or_else(|e| e.into_inner()) = Some(n);
                }
                Ok(None) => {
                    eprintln!("ComfyUI 已經開著，直接使用");
                    setup::set_updated(updated);
                }
                Err(e) => {
                    eprintln!("{e}");
                    launcher::set_note(Some(format!("ComfyUI 沒開起來：{e}")));
                }
            }
            p.start_extras(&dir, &logs, &data, comfy_port);
        });
    }
    print_urls(&o, false);
    server::serve_on(router, listener).await.map_err(|e| format!("伺服器停止：{e}"))
}
