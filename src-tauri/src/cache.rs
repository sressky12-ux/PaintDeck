//! 僅清理程式明確擁有、可重建的磁碟快取；不接受網頁提供的路徑，不追蹤連結。
use crate::{paths::Paths, setup::Config};
use serde::Serialize;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;

#[derive(Default)]
pub struct Control {
    pub gate: std::sync::Arc<tokio::sync::RwLock<()>>,
    pub cleaning: AtomicBool,
}

#[derive(Serialize)]
pub struct Group {
    pub id: &'static str,
    pub label: &'static str,
    pub bytes: u64,
    pub files: usize,
    pub skipped: usize,
    #[serde(skip)]
    targets: Vec<(PathBuf, PathBuf)>,
}
impl Group {
    pub(crate) fn tree(anchor: &Path, path: &Path, filter: fn(&Path) -> bool) -> Self {
        let mut group = Self::new("frames", "拆幀檔案");
        group.add(anchor, path, filter);
        group
    }

    fn new(id: &'static str, label: &'static str) -> Self {
        Self { id, label, bytes: 0, files: 0, skipped: 0, targets: vec![] }
    }
    fn add(&mut self, anchor: &Path, path: &Path, filter: fn(&Path) -> bool) {
        self.add_inner(anchor, path, filter, 0);
    }
    fn add_inner(&mut self, anchor: &Path, path: &Path, filter: fn(&Path) -> bool, depth: usize) {
        if depth > 64 { self.skipped += 1; return; }
        if !path.exists() && std::fs::symlink_metadata(path).is_err() { return; }
        if !safe_path(anchor, path) { self.skipped += 1; return; }
        let Ok(meta) = std::fs::symlink_metadata(path) else { self.skipped += 1; return; };
        if meta.is_dir() {
            match std::fs::read_dir(path) {
                Ok(entries) => for entry in entries {
                    match entry {
                        Ok(e) => self.add_inner(anchor, &e.path(), filter, depth + 1),
                        Err(_) => self.skipped += 1,
                    }
                },
                Err(_) => self.skipped += 1,
            }
        } else if meta.is_file() && filter(path) {
            self.bytes = self.bytes.saturating_add(meta.len());
            self.files += 1;
            self.targets.push((anchor.to_path_buf(), path.to_path_buf()));
        }
    }
}

pub(crate) fn linked(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 { return true; }
    }
    meta.file_type().is_symlink()
}

/// 每一層都檢查，防止快取資料夾或其子目錄被 junction／symlink 導到模型或原圖。
pub fn safe_path(anchor: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(anchor) else { return false; };
    let mut current = anchor.to_path_buf();
    let okay = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| !linked(&m));
    if !okay(&current) { return false; }
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) { return false; }
        current.push(component);
        if !okay(&current) { return false; }
    }
    true
}

fn all(_: &Path) -> bool { true }
fn thumbnail(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("webp"))
        || p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(".webp.tmp"))
}
fn comfy_archive(p: &Path) -> bool {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    [crate::setup::COMFY_URL, crate::setup::COMFY_AMD_URL, crate::setup::COMFY_INTEL_URL, crate::setup::COMFY_CU126_URL]
        .iter().filter_map(|u| u.rsplit('/').next())
        .any(|n| name == n || name == format!("{n}.part"))
}

pub fn inventory(p: &Paths, config: Option<&Config>) -> Vec<Group> {
    let mut thumbs = Group::new("thumbnails", "圖庫縮圖（瀏覽時自動重建）");
    thumbs.add(&p.root, &p.thumbs, thumbnail);
    let mut pe = Group::new("rewrite", "AI 改寫結果快取");
    for name in ["cache.sqlite", "cache.sqlite-wal", "cache.sqlite-shm"] {
        pe.add(&p.comfy_root, &p.pe_node.join(name), all);
    }
    let mut downloads = Group::new("downloads", "安裝下載暫存（未完成的下載需重新下載）");
    downloads.add(&p.root, &p.data.join("_dl"), all);
    let mut archives = Group::new("archives", "安裝完成後留下的壓縮檔");
    let trash = p.root.join("待刪除");
    if safe_path(&p.root, &trash) {
        if let Ok(entries) = std::fs::read_dir(&trash) {
            for e in entries.flatten() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                if name.strip_prefix("安裝用的壓縮檔_")
                    .is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '_')) {
                    archives.add(&p.root, &e.path(), all);
                }
            }
        }
    }
    let mut unpack = Group::new("unpack", "ComfyUI 安裝與解壓暫存");
    if let Some(c) = config.filter(|c| c.data_root == p.root) {
        if let Some(parent) = c.comfy_dir.parent() {
            unpack.add(parent, &parent.join("_anima_dl"), comfy_archive);
            let record = std::fs::read(p.data.join("installer_cache.json")).ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
            #[allow(unused_mut)]
            let mut locations = vec![(parent.to_path_buf(), parent.join("_anima_u"))];
            #[cfg(windows)]
            if let Some(root) = c.comfy_dir.ancestors().last().filter(|r| r.to_string_lossy().ends_with(":\\")) {
                let target = root.join("_anima_u");
                if !locations.iter().any(|(_, p)| p == &target) { locations.push((root.to_path_buf(), target)); }
            }
            for (anchor, dir) in locations {
                // 解壓暫存位置會共用；先確認這份資料的歸屬紀錄，再讀該目錄。
                let owned = record.as_ref().is_some_and(|v|
                    v["comfy_dir"].as_str().is_some_and(|s| Path::new(s) == c.comfy_dir)
                    && v["unpack"].as_str().is_some_and(|s| Path::new(s) == dir));
                if !owned { continue; }
                // 與安裝器相同的歸屬條件：只有 ComfyUI_windows_portable。
                if safe_path(&anchor, &dir) && std::fs::read_dir(&dir).is_ok_and(|mut rd|
                    rd.all(|e| e.is_ok_and(|e| e.file_name() == "ComfyUI_windows_portable"))) {
                    unpack.add(&anchor, &dir, all);
                }
            }
        }
    }
    let mut studio = Group::new("studio", "剪輯台的影格與代理預覽快取");
    for name in ["frames", "proxies"] {
        studio.add(&p.root, &p.data.join("studio/analysis").join(name), all);
    }
    vec![thumbs, pe, downloads, archives, unpack, studio]
}

/// 呼叫者先持獨占鎖並確認閒置；不刪目錄，逐檔重查歸屬與連結。
pub fn clear_files(group: &Group) -> (u64, usize, usize) {
    let (mut bytes, mut files, mut skipped) = (0u64, 0, group.skipped);
    for (anchor, path) in &group.targets {
        if !safe_path(anchor, path) { skipped += 1; continue; }
        let Ok(meta) = std::fs::symlink_metadata(path) else { skipped += 1; continue; };
        if !meta.is_file() || linked(&meta) { skipped += 1; continue; }
        match std::fs::remove_file(path) {
            Ok(()) => { bytes = bytes.saturating_add(meta.len()); files += 1; }
            Err(_) => skipped += 1,
        }
    }
    (bytes, files, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root(tag: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../待刪除").join(format!("cache-{tag}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap(); dir.canonicalize().unwrap()
    }
    fn put(path: &Path) { std::fs::create_dir_all(path.parent().unwrap()).unwrap(); std::fs::write(path, b"fixture bytes").unwrap(); }
    #[test]
    fn only_owned_rebuildable_files_are_removed() {
        let base = root("scope");
        let p = Paths::new(&base, &base.join("web"), Some(base.join("comfy")));
        let removed = [p.thumbs.join("1.webp"), p.thumbs.join("2.webp.tmp"), p.data.join("_dl/rocm10/fixture.whl"),
            base.join("待刪除/安裝用的壓縮檔_20261024_120000/tool.zip"), p.data.join("studio/analysis/frames/a.jpg")];
        let kept = [p.output.join("original.png"), p.input.join("aw_reference.png"), p.previews.join("lora.png"),
            p.data.join("models/tagger/model.onnx"), p.settings.clone(), p.data.join("studio/project.json"),
            base.join("待刪除/恢復原廠_20261024/original.png"), p.thumbs.join("user-note.txt")];
        for path in removed.iter().chain(kept.iter()) { put(path); }
        let groups = inventory(&p, None);
        assert_eq!(groups.iter().map(|g| g.files).sum::<usize>(), removed.len());
        let bytes: u64 = groups.iter().map(|g| clear_files(g).0).sum();
        assert_eq!(bytes, removed.len() as u64 * 13);
        assert!(removed.iter().all(|p| !p.exists()));
        assert!(kept.iter().all(|p| std::fs::read(p).unwrap() == b"fixture bytes"));
    }
    #[test]
    fn snapshot_does_not_authorize_a_replaced_directory_link() {
        let base = root("junction");
        let p = Paths::new(&base.join("station"), &base.join("web"), Some(base.join("comfy")));
        let cache = p.thumbs.join("1.webp"); put(&cache);
        let group = inventory(&p, None).remove(0);
        let outside = base.join("unrelated"); put(&outside.join("1.webp"));
        std::fs::rename(&p.thumbs, p.data.join("old-thumbs")).unwrap();
        #[cfg(windows)] {
            use std::os::windows::process::CommandExt;
            let status = std::process::Command::new("cmd").args(["/c", "mklink", "/J"])
                .arg(&p.thumbs).arg(&outside).creation_flags(0x08000000).output().unwrap();
            assert!(status.status.success(), "junction fixture: {} {}", String::from_utf8_lossy(&status.stdout), String::from_utf8_lossy(&status.stderr));
        }
        #[cfg(unix)] std::os::unix::fs::symlink(&outside, &p.thumbs).unwrap();
        let (bytes, files, skipped) = clear_files(&group);
        assert_eq!((bytes, files, skipped), (0, 0, 1));
        assert!(outside.join("1.webp").exists());
        assert_eq!(inventory(&p, None)[0].files, 0);
    }
    #[test]
    fn installer_archives_are_filtered_and_unknown_unpack_folders_survive() {
        let base = root("installer");
        let cfg = Config {data_root:base.join("station"), comfy_dir:base.join("install/ComfyUI_windows_portable")};
        std::fs::create_dir_all(&cfg.data_root).unwrap();
        let p = Paths::new(&cfg.data_root, &base.join("web"), Some(cfg.comfy_dir.join("ComfyUI")));
        let parent = cfg.comfy_dir.parent().unwrap();
        let archive = parent.join("_anima_dl/ComfyUI_windows_portable_nvidia.7z.part");
        let note = parent.join("_anima_dl/my-file.txt");
        let unpack = parent.join("_anima_u/private-file.txt");
        for path in [&archive, &note, &unpack] { put(path); }
        let groups = inventory(&p, Some(&cfg));
        let group = groups.iter().find(|g| g.id == "unpack").unwrap();
        assert_eq!(group.files, 1);
        clear_files(group);
        assert!(!archive.exists());
        assert!(note.exists() && unpack.exists());
    }
    #[test]
    fn extraction_cache_requires_this_installations_ownership_record() {
        let base = root("unpack-owner");
        let cfg = Config {data_root:base.join("station"), comfy_dir:base.join("install/ComfyUI_windows_portable")};
        std::fs::create_dir_all(cfg.data_root.join("data")).unwrap();
        let p = Paths::new(&cfg.data_root, &base.join("web"), Some(cfg.comfy_dir.join("ComfyUI")));
        let unpack = base.join("install/_anima_u");
        let file = unpack.join("ComfyUI_windows_portable/fixture.bin"); put(&file);
        let count = || inventory(&p, Some(&cfg)).iter().find(|g| g.id == "unpack").unwrap().files;
        assert_eq!(count(), 0);
        std::fs::write(p.data.join("installer_cache.json"), serde_json::to_vec(&serde_json::json!({"comfy_dir":cfg.comfy_dir,"unpack":unpack})).unwrap()).unwrap();
        assert_eq!(count(), 1);
        let groups = inventory(&p, Some(&cfg));
        clear_files(groups.iter().find(|g| g.id == "unpack").unwrap());
        assert!(!file.exists());
    }
}
