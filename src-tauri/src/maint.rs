//! 維護工具：丟進 Windows 資源回收筒、一鍵恢復原廠（舊資料整包移到「待刪除」，可以救回）。對照舊版 maint.py。
//!
//! 恢復原廠的每一次都存成  <root>\待刪除\恢復原廠_<時間>\
//!   manifest.json      這次清了哪些部分、每個檔案原本在哪
//!   anima.db           清之前的資料庫完整複本（救回時從這裡把資料表抄回去）
//!   files\...          搬走的檔案（生成的圖、縮圖、上傳暫存）
//! 不刪除任何使用者檔案：只搬家；備份的 settings.json 會拿掉 Civitai API key。
use crate::db::Db;
use crate::py::now;
use crate::settings;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

/// 可以選擇清掉的部分：key → 說明（網頁上顯示）
pub const PARTS: &[(&str, &str)] = &[
    ("gallery", "圖庫：所有生成的圖、圖庫紀錄、任務紀錄、縮圖"),
    (
        "uploads",
        "上傳暫存：放進圖生圖、局部重繪、多圖參考的輸入圖（ComfyUI\\input 裡 aw_ 開頭的檔）",
    ),
    ("presets", "提示詞庫（整套設定、片段）和舊的預設組"),
    (
        "lora_meta",
        "LoRA 的觸發詞、預設強度、備註、收藏（從 Civitai 下載時自動存的也會一起清掉）",
    ),
    (
        "settings",
        "設定（負面提示詞、AI 改寫選項…）回到預設；Civitai API key 保留",
    ),
];

fn db_tables(part: &str) -> &'static [&'static str] {
    match part {
        "gallery" => &["images", "jobs"],
        "presets" => &["presets", "prompt_lib"],
        "lora_meta" => &["lora_meta"],
        _ => &[],
    }
}

pub fn parts_json() -> Value {
    Value::Object(
        PARTS
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect(),
    )
}

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

// ---------------------------------------------------------------- Windows 資源回收筒
/// 把檔案丟進資源回收筒（可以從回收筒還原）。回傳成功丟掉的路徑。
#[cfg(windows)]
pub fn to_recycle_bin(paths: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    use windows_sys::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE,
        SHFILEOPSTRUCTW,
    };
    let paths: Vec<PathBuf> = paths
        .iter()
        .filter(|p| p.exists())
        .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
        .collect();
    let mut done = vec![];
    for batch in paths.chunks(200) {
        // 一次給太多路徑字串會超長；分批。格式：路徑\0路徑\0\0
        let mut from: Vec<u16> = vec![];
        for p in batch {
            from.extend(p.as_os_str().encode_wide_compat());
            from.push(0);
        }
        from.push(0);
        let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
        op.wFunc = FO_DELETE;
        op.pFrom = from.as_ptr();
        op.fFlags = (FOF_SILENT | FOF_NOCONFIRMATION | FOF_ALLOWUNDO | FOF_NOERRORUI) as u16;
        let rc = unsafe { SHFileOperationW(&mut op) };
        done.extend(batch.iter().filter(|p| !p.exists()).cloned());
        if rc != 0 && done.is_empty() {
            return Err(err(format!("丟進資源回收筒失敗（代碼 {rc}）")));
        }
    }
    Ok(done)
}

/// Linux：丟進桌面的垃圾桶（freedesktop 規格，用 GNOME 的 gio trash；KDE、Xfce 也讀同一個垃圾桶）。沒有 gio＝錯誤
#[cfg(not(windows))]
pub fn to_recycle_bin(paths: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    let paths: Vec<&PathBuf> = paths.iter().filter(|p| p.exists()).collect();
    let mut done = vec![];
    for batch in paths.chunks(200) {
        let st = std::process::Command::new("gio")
            .arg("trash")
            .arg("--")
            .args(batch.iter().map(|p| p.as_os_str()))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        done.extend(batch.iter().filter(|p| !p.exists()).map(|p| (*p).clone()));
        if let Err(e) = st {
            if done.is_empty() {
                return Err(err(format!("丟進垃圾桶失敗（找不到 gio：{e}）")));
            }
        }
    }
    if done.is_empty() && !paths.is_empty() {
        return Err(err("丟進垃圾桶失敗"));
    }
    Ok(done)
}

/// 某個路徑所在磁碟的（可用, 總共）位元組
#[cfg(windows)]
pub fn disk_usage(path: &Path) -> Option<(u64, u64)> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let w: Vec<u16> = path
        .as_os_str()
        .encode_wide_compat()
        .into_iter()
        .chain([0])
        .collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) };
    (ok != 0).then_some((free, total))
}
/// Linux：statvfs（可用給一般使用者的 f_bavail，跟 df 的 Avail 一樣）
#[cfg(not(windows))]
pub fn disk_usage(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    // 路徑還不存在（要建的資料夾）就往上找存在的那層
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let bs = st.f_frsize as u64;
    Some((st.f_bavail as u64 * bs, st.f_blocks as u64 * bs))
}

#[cfg_attr(not(windows), allow(dead_code))] // 只有 Windows 用（Linux 沒有這個機制）
trait EncodeWide {
    fn encode_wide_compat(&self) -> Vec<u16>;
}
impl EncodeWide for std::ffi::OsStr {
    #[cfg(windows)]
    fn encode_wide_compat(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().collect()
    }
    #[cfg(not(windows))]
    fn encode_wide_compat(&self) -> Vec<u16> {
        self.to_string_lossy().encode_utf16().collect()
    }
}

// ---------------------------------------------------------------- 搬家
/// 搬檔案或資料夾（同一顆硬碟是改名；跨硬碟就複製完再移除來源，跟 shutil.move 一樣）
pub fn move_path(src: &Path, dst: &Path) -> io::Result<()> {
    if let Some(d) = dst.parent() {
        std::fs::create_dir_all(d)?;
    }
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    copy_tree(src, dst)?;
    if src.is_dir() {
        std::fs::remove_dir_all(src)
    } else {
        std::fs::remove_file(src)
    }
}

pub fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_tree(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dst).map(|_| ())
    }
}

/// os.path.splitext
fn splitext(p: &Path) -> (String, String) {
    let s = p.to_string_lossy().to_string();
    let name_start = s.rfind(['\\', '/']).map(|i| i + 1).unwrap_or(0);
    match s[name_start..].rfind('.') {
        Some(i) if i > 0 && !s[name_start..name_start + i].chars().all(|c| c == '.') => {
            let at = name_start + i;
            (s[..at].to_string(), s[at..].to_string())
        }
        _ => (s, String::new()),
    }
}

/// 搬過去；目的地已經有同名的就加編號，不覆蓋。記錄 {from, to}
fn move_logged(src: &Path, dst: &Path, log: &mut Vec<Value>) -> io::Result<()> {
    let (base, ext) = splitext(dst);
    let mut dst = dst.to_path_buf();
    let mut n = 1;
    while dst.exists() {
        dst = PathBuf::from(format!("{base}_{n}{ext}"));
        n += 1;
    }
    move_path(src, &dst)?;
    log.push(json!({"from": src.to_string_lossy(), "to": dst.to_string_lossy()}));
    Ok(())
}

fn sorted_entries(dir: &Path) -> io::Result<Vec<std::fs::DirEntry>> {
    let mut v: Vec<_> = std::fs::read_dir(dir)?.flatten().collect();
    v.sort_by_key(|e| e.file_name());
    Ok(v)
}

fn write_json(path: &Path, v: &Value) -> io::Result<()> {
    std::fs::write(path, settings::to_text(v))
}

// ---------------------------------------------------------------- 恢復原廠
pub struct ResetTarget<'a> {
    pub root: &'a Path,
    pub output: &'a Path,
    pub input: &'a Path,
    pub thumbs: &'a Path,
    pub settings: &'a Path,
}

/// 依選的部分清掉資料，舊的全部搬進 待刪除\恢復原廠_<時間>\
pub fn reset(parts: &[String], t: &ResetTarget, db: &Db) -> io::Result<Value> {
    let parts: Vec<String> = parts
        .iter()
        .filter(|p| PARTS.iter().any(|(k, _)| k == p))
        .cloned()
        .collect();
    if parts.is_empty() {
        return Err(err("沒有選要清掉哪些部分"));
    }
    let has = |k: &str| parts.iter().any(|p| p == k);
    let name = format!("恢復原廠_{}", chrono::Local::now().format("%Y%m%d_%H%M%S"));
    let bx = t.root.join("待刪除").join(&name);
    let files = bx.join("files");
    if bx.exists() {
        return Err(err(format!("{} 已經存在", bx.display())));
    }
    std::fs::create_dir_all(&files)?;
    let mut moved = vec![];
    // 先留一份完整的資料庫複本（救回時從這裡抄回去）
    let backup = bx.join("anima.db");
    db.with_conn(|c| c.execute("VACUUM INTO ?1", [backup.to_string_lossy()]))
        .map_err(|e| err(format!("備份資料庫失敗：{e}")))?;
    if has("gallery") {
        // ComfyUI\output 裡全部（含直接在 ComfyUI 產生的圖、_trash）
        for e in sorted_entries(t.output)? {
            move_logged(
                &e.path(),
                &files.join("output").join(e.file_name()),
                &mut moved,
            )?;
        }
        if t.thumbs.is_dir() {
            for e in sorted_entries(t.thumbs)? {
                move_logged(
                    &e.path(),
                    &files.join("thumbs").join(e.file_name()),
                    &mut moved,
                )?;
            }
        }
    }
    if has("uploads") && t.input.is_dir() {
        for e in sorted_entries(t.input)? {
            if e.file_name().to_string_lossy().starts_with("aw_") && e.path().is_file() {
                move_logged(
                    &e.path(),
                    &files.join("input").join(e.file_name()),
                    &mut moved,
                )?;
            }
        }
    }
    db.with_conn(|c| -> rusqlite::Result<()> {
        for p in &parts {
            for tb in db_tables(p) {
                c.execute(&format!("DELETE FROM {tb}"), [])?;
            }
        }
        Ok(())
    })
    .map_err(|e| err(format!("清資料表失敗：{e}")))?;
    if has("settings") && t.settings.exists() {
        // 備份那份拿掉金鑰；現在的設定檔回到預設、保留金鑰
        let cur = settings::load(t.settings);
        write_json(
            &bx.join("settings.json"),
            &Value::Object(
                file_settings(t.settings)
                    .map(|s| settings::without_key(&s))
                    .unwrap_or_default(),
            ),
        )?;
        let mut fresh = settings::without_key(&settings::defaults());
        fresh.insert(settings::KEY.into(), json!(settings::civitai_key(&cur)));
        settings::save(t.settings, &fresh)?;
    }
    let manifest =
        json!({"name": name, "time": now(), "parts": parts, "moved": moved, "restored": false});
    write_json(&bx.join("manifest.json"), &manifest)?;
    Ok(json!({"name": name, "moved": moved.len(), "parts": parts}))
}

/// 設定檔本身的內容（不含預設值）
fn file_settings(path: &Path) -> Option<Map<String, Value>> {
    match serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

pub fn list_resets(root: &Path) -> Vec<Value> {
    let base = root.join("待刪除");
    let Ok(mut names) = std::fs::read_dir(&base).map(|rd| {
        rd.flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>()
    }) else {
        return vec![];
    };
    names.sort();
    names.reverse();
    let mut out = vec![];
    for n in names {
        let mp = base.join(&n).join("manifest.json");
        if !n.starts_with("恢復原廠_") {
            continue;
        }
        let Some(m) = std::fs::read_to_string(&mp)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            continue;
        };
        out.push(json!({"name": n, "time": m["time"], "parts": m["parts"], "files": m["moved"].as_array().map(Vec::len).unwrap_or(0),
                        "restored": m.get("restored").cloned().unwrap_or(json!(false))}));
    }
    out
}

/// os.path.normcase（Windows）：小寫、斜線統一成反斜線
fn normcase(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").to_lowercase()
}

/// 搬回原位。資料夾已存在（例如恢復原廠後又生了新圖）就逐一合併；同名檔案改名，記在 renamed（原路徑 → 新路徑）
fn merge_back(src: &Path, dst: &Path, renamed: &mut HashMap<String, String>) -> io::Result<usize> {
    if src.is_dir() && dst.is_dir() {
        for e in sorted_entries(src)? {
            merge_back(&e.path(), &dst.join(e.file_name()), renamed)?;
        }
        if std::fs::read_dir(src)?.next().is_none() {
            std::fs::remove_dir(src)?; // 只移除已經搬空的資料夾
        }
        return Ok(1);
    }
    if dst.exists() {
        let mut log = vec![];
        move_logged(src, dst, &mut log)?;
        renamed.insert(
            normcase(dst),
            log[0]["to"].as_str().unwrap_or_default().to_string(),
        );
        return Ok(1);
    }
    move_path(src, dst)?;
    Ok(1)
}

/// 救回某一次恢復原廠：檔案搬回原位（撞名的加編號），資料表的列抄回去（已存在的不動）
pub fn undo(
    name: &str,
    root: &Path,
    output: &Path,
    db: &Db,
    settings_path: &Path,
) -> io::Result<Value> {
    let name = Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let bx = root.join("待刪除").join(&name);
    let mp = bx.join("manifest.json");
    let mut m: Value = std::fs::read_to_string(&mp)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| err("找不到這次的紀錄"))?;
    if crate::py::truthy(m.get("restored")) {
        return Err(err("這次已經救回過了"));
    }
    let (mut back, mut missing, mut renamed) = (0, 0, HashMap::new());
    for mv in m["moved"].as_array().cloned().unwrap_or_default() {
        let (from, to) = (
            PathBuf::from(mv["from"].as_str().unwrap_or("")),
            PathBuf::from(mv["to"].as_str().unwrap_or("")),
        );
        if to.exists() {
            back += merge_back(&to, &from, &mut renamed)?;
        } else {
            missing += 1;
        }
    }
    let parts: Vec<String> = m["parts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_str().map(str::to_owned))
        .collect();
    let tables: Vec<&str> = parts
        .iter()
        .flat_map(|p| db_tables(p).iter().copied())
        .collect();
    if !tables.is_empty() {
        let old = bx.join("anima.db");
        db.with_conn(|c| -> rusqlite::Result<()> {
            c.execute("ATTACH DATABASE ?1 AS old", [old.to_string_lossy()])?;
            let r = (|| -> rusqlite::Result<()> {
                for t in &tables {
                    let cols_of = |schema: &str| -> rusqlite::Result<Vec<String>> {
                        let mut st = c.prepare(&format!("PRAGMA {schema}.table_info({t})"))?;
                        let v = st.query_map([], |r| r.get::<_, String>(1))?.collect();
                        v
                    };
                    let have = cols_of("main")?;
                    let cols: Vec<String> = cols_of("old")?
                        .into_iter()
                        .filter(|c| have.contains(c))
                        .collect();
                    if cols.is_empty() {
                        // 舊版的備份還沒有這個表：提示詞庫改從備份裡的預設組抄
                        if *t == "prompt_lib" {
                            c.execute(&crate::db::lib_from_presets_sql("old"), [now()])?;
                        }
                        continue;
                    }
                    let list = cols.join(",");
                    if *t != "images" || renamed.is_empty() {
                        c.execute(
                            &format!(
                                "INSERT OR IGNORE INTO main.{t}({list}) SELECT {list} FROM old.{t}"
                            ),
                            [],
                        )?;
                        continue;
                    }
                    // 圖庫：檔案撞名改過名的，紀錄裡的檔名也跟著改
                    let fi = cols.iter().position(|c| c == "filename").unwrap_or(0);
                    let si = cols.iter().position(|c| c == "subfolder").unwrap_or(0);
                    let mut st = c.prepare(&format!("SELECT {list} FROM old.images"))?;
                    let rows: Vec<Vec<rusqlite::types::Value>> = st
                        .query_map([], |r| {
                            (0..cols.len())
                                .map(|i| r.get::<_, rusqlite::types::Value>(i))
                                .collect()
                        })?
                        .collect::<rusqlite::Result<_>>()?;
                    let ins = format!(
                        "INSERT OR IGNORE INTO main.images({list}) VALUES({})",
                        vec!["?"; cols.len()].join(",")
                    );
                    for mut row in rows {
                        let txt = |v: &rusqlite::types::Value| match v {
                            rusqlite::types::Value::Text(s) => s.clone(),
                            _ => String::new(),
                        };
                        let orig = output.join(txt(&row[si])).join(txt(&row[fi]));
                        if let Some(new) = renamed.get(&normcase(&orig)) {
                            let base = Path::new(new)
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default();
                            row[fi] = rusqlite::types::Value::Text(base);
                        }
                        c.execute(&ins, rusqlite::params_from_iter(row))?;
                    }
                }
                Ok(())
            })();
            c.execute("DETACH DATABASE old", [])?;
            r
        })
        .map_err(|e| err(format!("抄回資料表失敗：{e}")))?;
    }
    let sp = bx.join("settings.json");
    if parts.iter().any(|p| p == "settings") && sp.exists() {
        let mut old = file_settings(&sp).unwrap_or_default();
        // 現在的金鑰要用 settings::load 讀（Windows 的設定檔只存加密過的 civitai_key_dpapi，直接讀檔拿不到 civitai_key）：
        // 2026-11-10 以前這裡直接讀檔，救回設定後 Civitai API key 就不見了
        let cur = settings::load(settings_path);
        old.insert(settings::KEY.into(), json!(settings::civitai_key(&cur)));
        settings::save(settings_path, &old)?;
    }
    m["restored"] = json!(now());
    write_json(&mp, &m)?;
    Ok(json!({"files": back, "renamed": renamed.len(), "missing": missing}))
}

#[cfg(test)]
mod tests {
    use super::splitext;
    use std::path::Path;

    #[test]
    fn splitext_like_python() {
        assert_eq!(
            splitext(Path::new(r"C:\a\b.png")),
            (r"C:\a\b".into(), ".png".into())
        );
        assert_eq!(
            splitext(Path::new(r"C:\a.d\b")),
            (r"C:\a.d\b".into(), "".into())
        );
        assert_eq!(
            splitext(Path::new(r"C:\a\.hidden")),
            (r"C:\a\.hidden".into(), "".into())
        );
        assert_eq!(
            splitext(Path::new(r"C:\a\x.tar.gz")),
            (r"C:\a\x.tar".into(), ".gz".into())
        );
    }
}
