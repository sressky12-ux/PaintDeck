//! 外掛寫法的 API（內容與切段在 skills.rs）：列出、看目錄、從 GitHub 裝、綁到寫法、移除（搬到 待刪除\外掛寫法\）、打開資料夾。
//! 程式不帶任何 skill：都是使用者自己裝的，只放在 <data>\skills\。
use super::{bad, blocking, internal, not_found, ok, ApiResult, Body, Shared};
use crate::{settings, skills};
use axum::extract::{Path as UrlPath, State};
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::time::Duration;

fn text(body: &Value, k: &str) -> String {
    body.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

fn now() -> String {
    chrono::Local::now().format("%Y%m%d_%H%M%S").to_string()
}

pub async fn list(State(app): State<Shared>) -> ApiResult {
    let dir = skills::root(&app.paths.data);
    let d = dir.clone();
    let all = blocking(move || skills::list(&d).iter().map(skills::Skill::summary).collect::<Vec<_>>()).await;
    let bind = settings::load(&app.paths.settings).get(skills::BIND_KEY).cloned().unwrap_or_else(|| json!({}));
    let guides: Vec<Value> = crate::agent::GUIDES.iter().map(|g| json!({"name": g.0, "zh": g.2})).collect();
    // 挑段方式：裝了 Qwen3-Embedding 照意思挑，沒有照關鍵字（embed.rs）
    let a = app.clone();
    let pick = blocking(move || a.pe.embed.status()).await;
    ok(json!({"dir": dir.display().to_string(), "skills": all, "bind": bind, "guides": guides, "pick": pick}))
}

/// 一份的細節：目錄（給模型看的那份）＋每段的標題、大小
pub async fn get_one(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult {
    let root = skills::root(&app.paths.data);
    let v = blocking(move || {
        skills::find(&root, &id).map(|s| {
            let mut v = s.summary();
            v["toc"] = json!(s.toc());
            v["part_list"] = json!(s.parts.iter().enumerate().map(|(i, p)| json!({"n": i + 1, "file": p.file, "title": p.title, "tokens": p.tokens})).collect::<Vec<_>>());
            v
        })
    })
    .await
    .ok_or_else(|| not_found("沒有這個外掛寫法"))?;
    ok(v)
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .proxy(crate::net::proxy())
        .user_agent("PaintDeck skills")
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())
}

async fn fetch(http: &reqwest::Client, url: &str) -> Result<bytes::Bytes, String> {
    let r = http.get(url).header("Accept", "application/vnd.github+json").send().await.map_err(|e| crate::net::explain(&e, url))?;
    match r.status().as_u16() {
        200 => r.bytes().await.map_err(|e| crate::net::explain(&e, url)),
        403 | 429 => Err("GitHub 暫時擋下來了（沒登入每小時只能查 60 次），過一陣子再試".into()),
        404 => Err("GitHub 找不到這個庫、分支或檔案（私人庫裝不了）".into()),
        s => Err(format!("GitHub 回了 {s}：{url}")),
    }
}

async fn fetch_json(http: &reqwest::Client, url: &str) -> Result<Value, String> {
    serde_json::from_slice(&fetch(http, url).await?).map_err(|e| format!("GitHub 回的資料讀不懂：{e}"))
}

/// 網址的一段路徑：英數和 -._~ 以外照 UTF-8 轉成 %XX
fn enc(seg: &str) -> String {
    seg.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn enc_path(p: &str) -> String {
    p.split('/').map(enc).collect::<Vec<_>>().join("/")
}

/// 庫裡的相對路徑 → 本機路徑（不准 ..、磁碟代號）
fn local(rel: &str) -> Result<PathBuf, String> {
    let segs: Vec<&str> = rel.split('/').collect();
    if segs.iter().any(|s| s.is_empty() || *s == ".." || *s == "." || s.contains([':', '\\'])) {
        return Err(format!("檔名不安全，沒有下載：{rel}"));
    }
    Ok(segs.iter().collect())
}

/// 從 GitHub 裝：{url}（庫首頁，或指到 skill 資料夾的 …/tree/分支/資料夾）。同名的舊版先搬到待刪除
pub async fn install(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let url = text(&body, "url");
    let g = skills::parse_github(&url).ok_or_else(|| bad("請貼 GitHub 網址：github.com/作者/庫名，或指到 skill 資料夾的 …/tree/分支/資料夾"))?;
    let http = client().map_err(internal)?;
    let api = format!("https://api.github.com/repos/{}/{}", g.owner, g.repo);
    let repo = fetch_json(&http, &api).await.map_err(bad)?;
    let branch = g.branch.clone().or_else(|| repo["default_branch"].as_str().map(String::from)).unwrap_or_else(|| "main".into());
    let tree = fetch_json(&http, &format!("{api}/git/trees/{}?recursive=1", enc(&branch))).await.map_err(bad)?;
    let blobs: Vec<(String, u64)> = tree["tree"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["type"] == "blob")
        .filter_map(|e| Some((e["path"].as_str()?.to_string(), e["size"].as_u64().unwrap_or(0))))
        .collect();
    let paths: Vec<String> = blobs.iter().map(|(p, _)| p.clone()).collect();
    let dir = skills::choose_skill(&paths, &g.path).map_err(bad)?;
    let mut files = skills::wanted_files(&blobs, &dir).map_err(bad)?;
    // skill 在子資料夾、裡面沒附授權：抓庫根目錄的授權檔一起放
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    if !dir.is_empty() && !files.iter().any(|(p, _)| skills::license_file(&base(p))) {
        if let Some(l) = blobs.iter().find(|(p, _)| !p.contains('/') && skills::license_file(p)) {
            files.push(l.clone());
        }
    }
    let root = skills::root(&app.paths.data);
    let tmp = root.join(format!(".installing_{}_{}", now(), std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(internal)?;
    let prefix = format!("{dir}/");
    let mut got = Ok(());
    for (p, _) in &files {
        let rel = if dir.is_empty() { p.clone() } else { p.strip_prefix(&prefix).map(String::from).unwrap_or_else(|| base(p)) };
        let raw = format!("https://raw.githubusercontent.com/{}/{}/{}/{}", g.owner, g.repo, enc_path(&branch), enc_path(p));
        let step = async {
            let dest = tmp.join(local(&rel)?);
            let bytes = fetch(&http, &raw).await?;
            if let Some(d) = dest.parent() {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            std::fs::write(&dest, &bytes).map_err(|e| e.to_string())
        };
        if let Err(e) = step.await {
            got = Err(e);
            break;
        }
    }
    // 下載到一半失敗：暫存資料夾是這次自己建的，直接清掉
    if let Err(e) = got {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(bad(e));
    }
    let skill_md = std::fs::read_dir(&tmp).ok().into_iter().flatten().flatten().map(|e| e.path()).find(|p| p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("SKILL.md")));
    let raw_md = skill_md.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    let (name, _, _) = skills::front_matter(&raw_md);
    let id = skills::safe_id(if name.trim().is_empty() { &g.repo } else { &name });
    let license = &repo["license"];
    let source = json!({"url": url, "repo": format!("{}/{}", g.owner, g.repo), "branch": branch, "path": dir,
                        "license": license["spdx_id"], "license_name": license["name"], "installed": chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()});
    std::fs::write(tmp.join("source.json"), serde_json::to_vec_pretty(&source).unwrap_or_default()).map_err(internal)?;
    let dest = root.join(&id);
    let trash = app.paths.root.join("待刪除").join("外掛寫法").join(format!("{id}_{}", now()));
    let replaced = dest.exists();
    let (t, d) = (tmp.clone(), dest.clone());
    blocking(move || {
        if d.exists() {
            crate::maint::move_path(&d, &trash)?;
        }
        std::fs::rename(&t, &d)
    })
    .await
    .map_err(internal)?;
    let r = root.clone();
    let i = id.clone();
    let sk = blocking(move || skills::find(&r, &i).map(|s| s.summary())).await.ok_or_else(|| internal("裝好了卻讀不到 SKILL.md"))?;
    // 先把每段的意思座標算好（背景，沒裝模型就什麼都不做）
    app.pe.embed.index_later(root.join(&id));
    ok(json!({"ok": true, "skill": sk, "replaced": replaced}))
}

/// 綁到寫法：{guide, skill}（skill 空字串＝拿掉）
pub async fn bind(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let (guide, skill) = (text(&body, "guide"), text(&body, "skill"));
    if !crate::agent::GUIDES.iter().any(|g| g.0 == guide) {
        return Err(bad(format!("沒有「{guide}」這份寫法")));
    }
    if !skill.is_empty() {
        let root = skills::root(&app.paths.data);
        let s = skill.clone();
        let r = root.clone();
        if !blocking(move || skills::find(&r, &s).is_some()).await {
            return Err(not_found("沒有這個外掛寫法"));
        }
        // 自己放進資料夾的 skill 沒經過安裝：綁的時候先在背景算意思座標
        app.pe.embed.index_later(root.join(&skill));
    }
    let _g = app.settings_lock.lock().await;
    let mut s = settings::load(&app.paths.settings);
    let mut b: Map<String, Value> = s.get(skills::BIND_KEY).and_then(Value::as_object).cloned().unwrap_or_default();
    if skill.is_empty() {
        b.remove(&guide);
    } else {
        b.insert(guide, json!(skill));
    }
    s.insert(skills::BIND_KEY.into(), Value::Object(b.clone()));
    settings::save(&app.paths.settings, &s).map_err(internal)?;
    ok(json!({"ok": true, "bind": b}))
}

/// 移除：整個資料夾搬到 待刪除\外掛寫法\，綁著它的寫法一起拿掉
pub async fn remove(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let id = text(&body, "id");
    if !skills::valid_id(&id) {
        return Err(bad("名稱不對"));
    }
    let dir = skills::root(&app.paths.data).join(&id);
    if !dir.is_dir() {
        return Err(not_found("沒有這個外掛寫法"));
    }
    let to = app.paths.root.join("待刪除").join("外掛寫法").join(format!("{id}_{}", now()));
    let t = to.clone();
    blocking(move || crate::maint::move_path(&dir, &t)).await.map_err(internal)?;
    let _g = app.settings_lock.lock().await;
    let mut s = settings::load(&app.paths.settings);
    if let Some(Value::Object(b)) = s.get_mut(skills::BIND_KEY) {
        b.retain(|_, v| v.as_str() != Some(id.as_str()));
    }
    settings::save(&app.paths.settings, &s).map_err(internal)?;
    ok(json!({"ok": true, "moved_to": to.display().to_string()}))
}

/// 檔案總管打開 skills 資料夾（自己放資料夾進去也可以）
pub async fn open(State(app): State<Shared>) -> ApiResult {
    let dir = skills::root(&app.paths.data);
    std::fs::create_dir_all(&dir).map_err(internal)?;
    #[cfg(windows)]
    std::process::Command::new("explorer.exe").arg(&dir).spawn().map_err(|e| bad(format!("打不開檔案總管：{e}")))?;
    ok(json!({"ok": true, "dir": dir.display().to_string()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_encoded_and_checked() {
        assert_eq!(enc_path("references/鏡頭 a.md"), "references/%E9%8F%A1%E9%A0%AD%20a.md");
        assert_eq!(local("references/a.md").unwrap(), PathBuf::from("references").join("a.md"));
        assert!(local("../x.md").is_err() && local("C:/x.md").is_err() && local("a//b.md").is_err());
    }
}
