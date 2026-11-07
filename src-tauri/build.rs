fn main() {
    // 無頭版（沒有 desktop 功能）不用 Tauri 的資源檔、權限設定
    if std::env::var_os("CARGO_FEATURE_DESKTOP").is_none() {
        return;
    }
    // updater 的 Tauri 錯誤對話框會連到 TaskDialogIndirect。lib/整合測試也需要
    // Common Controls v6 activation manifest；tauri-build 的 resource.lib 原本只連 bins。
    // 主視窗的網頁來自內建伺服器（外部網址），自訂指令要登記成權限，capabilities\main.json 才能開放
    let app = tauri_build::AppManifest::new().commands(&["hide_window", "quit_app", "open_assistant"]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(app))
        .expect("tauri-build 失敗");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'");
    }
}
