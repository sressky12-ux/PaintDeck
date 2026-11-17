//! RefMod（crate::refmod）：本機裝外掛、檢查狀態，拍片前把角色的 RefMod 檔放到本機 ComfyUI。
//! 角色做 RefMod 的 API 在 api_assets::make_refmod；雲端上傳在 api_cloud::upload_refmods。
use super::api_assets::assets_dir;
use super::{bad, blocking, ok, ApiError, ApiResult, Shared};
use crate::refmod as rm;
use axum::extract::State;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

fn node_dir(app: &Shared) -> PathBuf {
    app.paths.comfy_root.join("custom_nodes").join(rm::DIR)
}

/// 本機 ComfyUI 裡這個 RefMod 的檔案位置（models\refmods\<名字>.safetensors）
pub fn local_path(app: &Shared, name: &str) -> PathBuf {
    let mut p = app.paths.comfy_root.join("models").join("refmods");
    for part in name.split('/') {
        p.push(part);
    }
    p.set_extension("safetensors");
    p
}

/// 名字 → 資產庫收著的那份檔（paintdeck/pd_char_<id> → 那個角色 data.refmod.file）
pub fn asset_file(app: &Shared, name: &str) -> Result<PathBuf, String> {
    let id = name
        .strip_prefix(&format!("{}/pd_char_", rm::SUB))
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| format!("{name} 不是資產庫做的 RefMod"))?;
    let row = app.db.lib_item(id).map_err(|e| e.to_string())?.ok_or_else(|| format!("找不到 RefMod {name} 的角色（可能刪掉了）"))?;
    let data = row.get("data").cloned().unwrap_or(Value::Null);
    if data["refmod"]["name"] != name {
        return Err(format!("角色「{}」的 RefMod 換過了：拍攝單重新加一次這個角色", row.get("name").and_then(Value::as_str).unwrap_or("")));
    }
    let f = assets_dir(&app.paths).join(super::basename(data["refmod"]["file"].as_str().unwrap_or("")));
    if !f.is_file() {
        return Err(format!("RefMod {name} 的檔案不見了：到資產庫的角色卡重做一次"));
    }
    Ok(f)
}

/// 本機 ComfyUI 有沒有載入 RefMod 節點
pub async fn require_nodes(app: &Shared) -> Result<(), ApiError> {
    if !app.comfy.live().online {
        return Err(bad("本機的 ComfyUI 沒有連線"));
    }
    if app.comfy.object_info("MiniMaxH3RefModExtract").await.is_err() {
        return Err(bad("本機 ComfyUI 還沒有 RefMod 節點：到資產庫的角色卡按「安裝 RefMod 外掛」，裝好重開 ComfyUI"));
    }
    Ok(())
}

/// 本機拍片前：把資產庫收著的 RefMod 檔複製到 ComfyUI 的 models\refmods。
/// 每次都蓋（一個才幾百 KB）：同解析度的圖重做出來檔案大小一模一樣，比大小分不出新舊
pub async fn ensure_local(app: &Shared, names: &[String]) -> Result<(), String> {
    if names.is_empty() {
        return Ok(());
    }
    require_nodes(app).await.map_err(|e| e.1)?;
    for n in names {
        let (from, to) = (asset_file(app, n)?, local_path(app, n));
        blocking(move || -> std::io::Result<()> {
            std::fs::create_dir_all(to.parent().unwrap_or(Path::new(".")))?;
            std::fs::copy(&from, &to)?;
            Ok(())
        })
        .await
        .map_err(|e| format!("RefMod 檔放不進 ComfyUI：{e}"))?;
    }
    Ok(())
}

/// 雲端拍片前要上傳的：(名字, 資產庫裡的檔)
pub fn asset_files(app: &Shared, names: &[String]) -> Result<Vec<(String, PathBuf)>, String> {
    names.iter().map(|n| Ok((n.clone(), asset_file(app, n)?))).collect()
}

pub(super) fn git(args: &[&str], dir: Option<&Path>) -> Result<String, String> {
    let mut c = Command::new("git");
    c.args(args);
    if let Some(d) = dir {
        c.current_dir(d);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x08000000);
    }
    let out = c.output().map_err(|_| "找不到 git：先安裝 Git（https://git-scm.com），再按一次".to_string())?;
    if !out.status.success() {
        return Err(format!("git {} 失敗：{}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 已經裝的那份是不是釘的 commit（不是 git 資料夾、讀不到都算不是）
pub(super) fn installed_commit(dir: &Path) -> Option<String> {
    dir.join(".git").exists().then(|| git(&["rev-parse", "HEAD"], Some(dir)).ok()).flatten()
}

/// 把外掛 git clone 到 dest（ComfyUI 的 custom_nodes\<名字>），釘在 commit。回「這次真的裝了」（已經是這版＝false）。
/// 先抓到 <名字>-installing.disabled（ComfyUI 不載 .disabled 結尾的），切好版本才改名，中途失敗不會留半套；
/// dest 已經有別的版本（或不是 git 資料夾）就不動它，回錯誤。RefMod 和 Wardrobe（api_wardrobe.rs）共用
pub(super) fn clone_pinned(dest: &Path, repo: &str, commit: &str, dir: &str) -> Result<bool, String> {
    if dest.exists() {
        return match installed_commit(dest) {
            Some(h) if h == commit => Ok(false),
            _ => Err(format!("{} 已經有別的版本：要換成繪匣審過的版本，先把那個資料夾移走再按一次", dest.display())),
        };
    }
    let parent = dest.parent().ok_or("ComfyUI 資料夾不對")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let tmp = parent.join(format!("{dir}-installing.disabled"));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).map_err(|e| format!("上次沒裝完的暫存資料夾刪不掉：{e}"))?;
    }
    let tmp_s = tmp.to_string_lossy().to_string();
    let fetched = git(&["clone", "--quiet", "--no-checkout", repo, &tmp_s], None)
        .and_then(|_| git(&["checkout", "--quiet", "--detach", commit], Some(&tmp)))
        .and_then(|_| git(&["rev-parse", "HEAD"], Some(&tmp)));
    match fetched {
        Ok(h) if h == commit => {}
        Ok(h) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(format!("抓到的版本不對（{h}）：沒有裝"));
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
    }
    std::fs::rename(&tmp, dest).map_err(|e| format!("裝到 custom_nodes 失敗：{e}"))?;
    Ok(true)
}

/// GET → {installed, same, loaded, online, repo, commit}
pub async fn status(State(app): State<Shared>) -> ApiResult {
    let dir = node_dir(&app);
    let (installed, head) = blocking(move || (dir.join("__init__.py").is_file(), installed_commit(&dir))).await;
    let online = app.comfy.live().online;
    let loaded = online && app.comfy.object_info("MiniMaxH3RefModExtract").await.is_ok();
    ok(json!({"installed": installed, "same": head.as_deref() == Some(rm::COMMIT), "loaded": loaded, "online": online,
              "repo": rm::REPO, "commit": rm::COMMIT}))
}

/// POST → 把外掛 git clone 到本機 ComfyUI 的 custom_nodes（釘在審過的 commit），回 {installed, restart}。
/// 先抓到 custom_nodes\<名字>-installing.disabled（ComfyUI 不載 .disabled 結尾的），切好版本才改名，中途失敗不會留半套
pub async fn install(State(app): State<Shared>) -> ApiResult {
    // 連按兩次：第二次等第一次裝完，再看到「已經裝好」
    static INSTALLING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _g = INSTALLING.lock().await;
    let dest = node_dir(&app);
    let r = blocking(move || clone_pinned(&dest, rm::REPO, rm::COMMIT, rm::DIR)).await.map_err(bad)?;
    ok(json!({"installed": true, "fresh": r, "restart": r || !app.comfy.object_info("MiniMaxH3RefModExtract").await.is_ok()}))
}
