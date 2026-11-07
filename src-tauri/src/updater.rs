//! 本體更新：私人版永遠手動；公開版只安裝官方 updater 驗過簽章的版本。
//! 不開放外掛的原生 IPC，所有操作經本機 API 的頻道、佇列與更新鎖檢查。
//! 沒有 desktop 功能（Linux 純命令列的無頭版，不連 Tauri）：永遠手動更新，畫面只給 Releases 的連結。
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
#[cfg(feature = "desktop")]
use std::sync::atomic::Ordering;
use std::sync::{atomic::AtomicBool, Mutex, OnceLock};
#[cfg(feature = "desktop")]
use std::time::Duration;
#[cfg(feature = "desktop")]
use tauri_plugin_updater::{Update, Updater, UpdaterExt};

/// 無頭版沒有更新外掛：放一個同名的空殼（永遠不會有值）
#[cfg(not(feature = "desktop"))]
#[allow(dead_code)]
pub struct Update {
    pub version: String,
    pub body: Option<String>,
}

// 匯出公開版時只改這個常數；不能用環境變數或使用者設定繞過。
pub const CHANNEL: &str = "public";
pub const ENDPOINT: &str = "https://github.com/sressky12-ux/PaintDeck/releases/latest/download/latest.json";
pub const RELEASES: &str = "https://github.com/sressky12-ux/PaintDeck/releases";

fn allowed(channel: &str, desktop: bool) -> bool { channel == "public" && desktop }

pub fn newer(current: &str, remote: &str) -> bool {
    match (semver::Version::parse(current), semver::Version::parse(remote.trim_start_matches('v'))) {
        (Ok(c), Ok(r)) => r > c && r.pre.is_empty(),
        _ => false,
    }
}

struct Status {
    auto: bool,
    phase: &'static str,
    pending: Option<Update>,
    done: u64,
    total: Option<u64>,
}

pub struct Service {
    file: PathBuf,
    status: Mutex<Status>,
    pub installing: AtomicBool,
    /// 安裝取得寫鎖；一般修改 API 在整個 handler 期間持讀鎖，避免檢查和送工之間的競態。
    pub gate: tokio::sync::RwLock<()>,
    operation: tokio::sync::Mutex<()>,
    #[cfg(feature = "desktop")]
    app: OnceLock<tauri::AppHandle>,
    pub setup_busy: OnceLock<Box<dyn Fn() -> bool + Send + Sync>>,
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    before_exit: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl Service {
    pub fn new(data: &Path) -> Self {
        let file = data.join("updater.json");
        let auto = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v["auto_check"].as_bool()).unwrap_or(true);
        Self {
            file, status: Mutex::new(Status { auto, phase: "idle", pending: None, done: 0, total: None }),
            installing: AtomicBool::new(false), gate: tokio::sync::RwLock::new(()),
            operation: tokio::sync::Mutex::new(()),
            #[cfg(feature = "desktop")]
            app: OnceLock::new(),
            setup_busy: OnceLock::new(), before_exit: OnceLock::new(),
        }
    }

    #[cfg(feature = "desktop")]
    pub fn bind(&self, app: tauri::AppHandle, before_exit: impl Fn() + Send + Sync + 'static) {
        let _ = self.app.set(app);
        let _ = self.before_exit.set(Box::new(before_exit));
    }

    /// Linux 版先不開 App 內更新：Release 的 latest.json 還沒有 linux-x86_64 的簽章檔，打開只會一直報「找不到這個平台」。
    /// 畫面照私人版一樣只給 Releases 的連結，自己下載新的 AppImage／deb
    #[cfg(feature = "desktop")]
    pub fn enabled(&self) -> bool { cfg!(windows) && allowed(CHANNEL, self.app.get().is_some()) }
    #[cfg(not(feature = "desktop"))]
    pub fn enabled(&self) -> bool { allowed(CHANNEL, false) }

    pub fn auto_check(&self) -> bool {
        self.enabled() && self.status.lock().unwrap_or_else(|e| e.into_inner()).auto
    }

    pub fn view(&self) -> Value {
        let s = self.status.lock().unwrap_or_else(|e| e.into_inner());
        json!({"current_version": env!("CARGO_PKG_VERSION"), "channel": CHANNEL,
            "enabled": self.enabled(), "auto_check": s.auto, "phase": s.phase,
            "done": s.done, "total": s.total, "releases_url": RELEASES,
            "update": s.pending.as_ref().map(|u| json!({"version": u.version, "notes": u.body}))})
    }

    pub fn set_auto(&self, on: bool) -> Result<(), String> {
        let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = self.file.parent() { std::fs::create_dir_all(p).map_err(|_| "無法儲存更新設定")?; }
        let tmp = self.file.with_extension("tmp");
        std::fs::write(&tmp, json!({"auto_check": on}).to_string()).map_err(|_| "無法儲存更新設定")?;
        std::fs::rename(tmp, &self.file).map_err(|_| "無法儲存更新設定")?;
        s.auto = on;
        Ok(())
    }

    #[cfg(feature = "desktop")]
    fn updater(&self) -> Result<Updater, String> {
        if !self.enabled() { return Err("這是私人版或瀏覽器測試站，請手動安裝更新".into()); }
        let app = self.app.get().ok_or("請用桌面版更新")?;
        app.updater_builder()
            .version_comparator(|c, r| newer(&c.to_string(), &r.version.to_string()))
            .timeout(Duration::from_secs(20))
            // 檢查與下載（含重新導向）共用既有網路層；不讓外掛另讀系統代理。
            .configure_client(|c| c.no_proxy().proxy(crate::net::proxy()).pool_max_idle_per_host(0)
                .connect_timeout(Duration::from_secs(20)).read_timeout(Duration::from_secs(90)))
            .on_before_exit({
                let app = app.clone();
                move || {
                    // Windows updater 直接 process::exit，不能只靠 RunEvent::Exit 清理。
                    if let Some(service) = app.try_state::<std::sync::Arc<Service>>() {
                        if let Some(f) = service.before_exit.get() { f(); }
                    }
                }
            })
            .build().map_err(|_| "無法啟用更新，請手動下載安裝檔".into())
    }

    pub async fn check(&self) -> Value {
        if !self.enabled() { return self.view(); }
        let Ok(_op) = self.operation.try_lock() else { return self.view() };
        self.phase("checking");
        #[cfg(feature = "desktop")]
        let result = match self.updater() {
            Ok(u) => u.check().await.ok(),
            Err(_) => None,
        };
        #[cfg(not(feature = "desktop"))]
        let result = None;
        self.checked(result);
        self.view()
    }

    fn checked(&self, result: Option<Option<Update>>) {
        let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
        match result {
            Some(update) => {
                s.phase = if update.is_some() { "available" } else { "current" };
                s.pending = update;
            }
            // 離線不彈窗；保留先前查到的新版，安裝時仍會重新檢查。
            None => s.phase = "offline",
        }
    }

    pub fn phase(&self, phase: &'static str) {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).phase = phase;
    }

    #[cfg(not(feature = "desktop"))]
    pub async fn install(&self, _app: &crate::server::Shared, _version: &str) -> Result<(), String> {
        Err(format!("無頭版請到 {RELEASES} 下載新版，手動換掉"))
    }

    #[cfg(feature = "desktop")]
    pub async fn install(&self, app: &crate::server::Shared, version: &str) -> Result<(), String> {
        // 私人版在伺服器擋住，不接受指定 endpoint、key 或頻道。
        let updater = self.updater()?;
        let _op = self.operation.try_lock().map_err(|_| "正在檢查或下載更新，請稍候")?;
        if self.installing.swap(true, Ordering::SeqCst) { return Err("正在更新".into()); }
        struct Reset<'a>(&'a Service);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.installing.store(false, Ordering::SeqCst);
                self.0.phase("idle");
            }
        }
        let _reset = Reset(self);
        let _gate = self.gate.write().await;
        crate::server::update_idle(app).await?;
        let update = updater.check().await.map_err(|_| "暫時無法連到更新來源，請稍後再試")?
            .ok_or("目前沒有新版")?;
        if update.version != version { return Err("新版資訊已變更，請重新檢查更新並確認".into()); }
        self.phase("downloading");
        {
            let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
            s.done = 0; s.total = None;
        }
        // download() 先驗簽，失敗時絕不啟動安裝程式。
        let bytes = update.download(|n, total| {
            let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
            s.done += n as u64; s.total = total;
        }, || {}).await.map_err(|_| "更新下載或簽章驗證失敗，請稍後重試或手動下載")?;
        crate::server::update_idle(app).await?;
        self.phase("installing");
        // install 再驗一次簽章，Windows 成功啟動安裝程式後會退出，/P /R 會裝回原位置並重開。
        update.install(bytes).map_err(|_| "無法啟動更新安裝程式，請手動下載安裝".into())
    }
}

#[cfg(feature = "desktop")]
use tauri::Manager;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_and_channel() {
        assert!(newer("26.10.24", "26.1024.1"));
        assert!(newer("26.1024.1", "v26.1024.2"));
        assert!(!newer("26.1024.2", "26.1024.1"));
        assert!(!newer("26.1024.1", "26.1024.1"));
        assert!(!newer("26.1024.1", "27.1.1-beta"));
        assert!(!newer("26.1024.1", "broken"));
        let s = Service::new(&std::env::temp_dir().join("anima_update_no_file"));
        assert!(!s.enabled()); // 無桌面綁定；不會自行下載或安裝
        assert!(!allowed("private", true));
        assert!(allowed("public", true));
        assert!(!allowed("public", false));
        s.checked(None);
        assert_eq!(s.view()["phase"], "offline");
    }
}
