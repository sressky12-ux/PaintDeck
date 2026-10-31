//! 解除安裝時「連資料一起刪」。2026-10-30 使用者：「直接讓移除的程式能直接刪除全部東西 全部，包括暫存，模型，LORA，工作流等等，全清理乾淨。」
//!
//! NSIS 移除程式（nsis\hooks.nsh 的 NSIS_HOOK_PREUNINSTALL）在刪程式檔之前跑 `anima-app.exe --uninstall-cleanup --lang <$LANGUAGE>`：
//! 列出要刪的東西和大小 → 確認視窗（「全部刪除」／「只移除程式」，預設後者；「連生成的圖片一起刪」預設不勾）
//! → 關掉在這些資料夾裡跑的程式 → 永久刪除（不進資源回收筒）→ 回報。App 內更新（/UPDATE）、/P、/S 不會呼叫。
//!
//! 刪：資料資料夾的 data\、待刪除\（刪完沒別的東西就連資料資料夾一起刪）；整個 ComfyUI 資料夾（使用者選的：不管是不是精靈裝的），
//!     模型、LoRA、工作流、自訂節點、AI 改寫模型、llama.cpp 都在裡面；ComfyUI 安裝暫存 _anima_dl、_anima_u（認法同 cache.rs）；
//!     %APPDATA%、%LOCALAPPDATA% 的 local.anima.studio（設定檔、WebView 快取）；刪完變空的外層資料夾（例如 AnimaStudio）。
//! 留：ComfyUI\output（沒勾「連圖片一起刪」）、junction／symlink 指到的東西（只拆連結）、外部模型資料夾（在別處，不會碰到）、
//!     資料資料夾裡不是繪匣建的檔案、程式資料夾（NSIS 自己刪）。磁碟根目錄、使用者資料夾、Windows、Program Files 和它們的上層一律不刪。
//!
//! 測試：ANIMA_CONFIG＝測試用 config.json（設定資料夾只算它所在的資料夾，不碰真的 %APPDATA%，也不關程式資料夾裡的程式）；
//!       ANIMA_UNINSTALL_ANSWER＝no｜yes｜yes+images：不跳視窗，直接照這個答，結果印在 stdout。
use crate::cache;
use crate::paths::Paths;
use crate::setup::Config;
use std::path::{Path, PathBuf};

const APP_ID: &str = "local.anima.studio";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Comfy,
    Data,
    Trash,
    AppDir,
    Images,
}

#[derive(Debug)]
pub struct Item {
    pub kind: Kind,
    pub path: PathBuf,
    pub bytes: u64,
}

/// 這台電腦的位置（測試時自己給）
pub struct Env {
    pub config: PathBuf,
    /// 設定資料夾：%APPDATA%、%LOCALAPPDATA% 的 local.anima.studio
    pub app_dirs: Vec<PathBuf>,
    /// 程式資料夾：NSIS 自己刪，這裡跳過
    pub install_dir: Option<PathBuf>,
    /// 太危險的位置：它本身和它的上層都不刪
    pub protected: Vec<PathBuf>,
    /// 程式資料夾裡在跑的繪匣也要關（測試時 false）
    pub kill_install: bool,
}

impl Env {
    pub fn from_system() -> Env {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let test = var("ANIMA_CONFIG");
        let config = test.clone().unwrap_or_else(|| var("APPDATA").unwrap_or_default().join(APP_ID).join("config.json"));
        let app_dirs = match &test {
            Some(c) => c.parent().map(Path::to_path_buf).into_iter().collect(),
            None => ["APPDATA", "LOCALAPPDATA"].iter().filter_map(|k| var(k)).map(|d| d.join(APP_ID)).collect(),
        };
        let mut protected: Vec<PathBuf> = ["USERPROFILE", "APPDATA", "LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)", "ProgramW6432",
            "ProgramData", "SystemRoot", "windir", "PUBLIC", "OneDrive"].iter().filter_map(|k| var(k)).collect();
        if let Some(home) = var("USERPROFILE") {
            protected.extend(["Desktop", "Documents", "Downloads", "Pictures", "Videos", "Music"].iter().map(|n| home.join(n)));
        }
        Env {
            config,
            app_dirs,
            install_dir: std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)),
            protected,
            kill_install: test.is_none(),
        }
    }
}

pub struct Plan {
    pub items: Vec<Item>,
    /// ComfyUI\output（勾了才刪）
    pub images: Option<Item>,
    /// 碰到保護清單、不刪的位置
    pub refused: Vec<PathBuf>,
    /// ComfyUI 安裝暫存（cache.rs 的 unpack：_anima_dl、_anima_u）
    unpack: Option<cache::Group>,
    /// 刪完是空的才刪：（資料夾, 連裡面的空資料夾一起清）
    prune: Vec<(PathBuf, bool)>,
    /// 資料資料夾：刪完還有別的東西就回報
    data_root: Option<PathBuf>,
    /// 在這些資料夾裡跑的程式全部關掉
    procs: Vec<PathBuf>,
    /// 程式資料夾（只關繪匣自己的程式；刪檔時跳過）
    install_dir: Option<PathBuf>,
    kill_install: bool,
}

impl Plan {
    pub fn total(&self) -> u64 {
        self.items.iter().map(|i| i.bytes).sum::<u64>() + self.unpack_bytes()
    }
    pub fn unpack_bytes(&self) -> u64 {
        self.unpack.as_ref().map_or(0, |g| g.bytes)
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.images.is_none() && self.unpack_bytes() == 0
    }
}

/// 比對路徑用：反斜線、去掉結尾的 \、不分大小寫
fn norm(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// child 是 parent 本身或在它裡面
fn inside(child: &Path, parent: &Path) -> bool {
    let (c, p) = (norm(child), norm(parent));
    c == p || c.starts_with(&format!("{p}\\"))
}

/// 磁碟根目錄、相對路徑、保護清單本身或它的上層：不刪
pub fn dangerous(path: &Path, protected: &[PathBuf]) -> bool {
    !path.is_absolute() || path.parent().is_none() || protected.iter().any(|p| inside(p, path))
}

/// 真的資料夾（不是 junction／symlink）
fn real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir() && !cache::linked(&m))
}

/// 大小：不跟進連結，keep 裡的不算
fn measure(path: &Path, keep: &[PathBuf]) -> u64 {
    if keep.iter().any(|k| inside(path, k)) {
        return 0;
    }
    let Ok(m) = std::fs::symlink_metadata(path) else { return 0 };
    if cache::linked(&m) {
        0
    } else if m.is_dir() {
        std::fs::read_dir(path).map_or(0, |rd| rd.flatten().map(|e| measure(&e.path(), keep)).sum())
    } else {
        m.len()
    }
}

pub fn plan(env: &Env) -> Plan {
    let mut p = Plan {
        items: vec![], images: None, refused: vec![], unpack: None, prune: vec![], data_root: None, procs: vec![],
        install_dir: env.install_dir.clone(), kill_install: env.kill_install,
    };
    let mut keep: Vec<PathBuf> = env.install_dir.iter().cloned().collect();
    let mut want: Vec<(Kind, PathBuf)> = vec![];
    if let Some(c) = Config::load(&env.config) {
        let root = crate::setup::comfyui_dir(&c.comfy_dir);
        let output = root.join("output");
        if !dangerous(&c.comfy_dir, &env.protected) && real_dir(&output) && inside(&output, &c.comfy_dir) {
            p.images = Some(Item { kind: Kind::Images, bytes: measure(&output, &keep), path: output.clone() });
            keep.push(output);
        }
        want.push((Kind::Comfy, c.comfy_dir.clone()));
        want.push((Kind::Data, c.data_root.join("data")));
        want.push((Kind::Trash, c.data_root.join("待刪除")));
        if !dangerous(&c.comfy_dir, &env.protected) {
            let paths = Paths::new(&c.data_root, &c.data_root.join("web"), Some(root));
            p.unpack = cache::inventory(&paths, Some(&c)).into_iter().find(|g| g.id == "unpack" && g.files > 0);
            if let Some(parent) = c.comfy_dir.parent() {
                p.prune.push((parent.join("_anima_dl"), true));
                p.prune.push((parent.join("_anima_u"), true));
            }
            if let Some(drive) = c.comfy_dir.ancestors().last() {
                p.prune.push((drive.join("_anima_u"), true));
            }
        }
        p.prune.push((c.data_root.clone(), false));
        p.data_root = Some(c.data_root.clone());
        for d in [c.data_root.parent(), c.comfy_dir.parent()].into_iter().flatten() {
            if !p.prune.iter().any(|(x, _)| x == d) {
                p.prune.push((d.to_path_buf(), false));
            }
        }
        p.procs = vec![c.comfy_dir.clone(), c.data_root.clone()];
    }
    want.extend(env.app_dirs.iter().map(|d| (Kind::AppDir, d.clone())));
    for (kind, path) in want {
        if std::fs::symlink_metadata(&path).is_err() {
            continue;
        }
        if dangerous(&path, &env.protected) {
            p.refused.push(path);
            continue;
        }
        // 已經在別的項目裡面的不重複列；把已列的包進去的，換成大的
        if p.items.iter().any(|i| inside(&path, &i.path)) {
            continue;
        }
        p.items.retain(|i| !inside(&i.path, &path));
        p.items.push(Item { kind, bytes: measure(&path, &keep), path });
    }
    if p.images.as_ref().is_some_and(|img| !p.items.iter().any(|i| inside(&img.path, &i.path))) {
        p.images = None; // 包著圖片的資料夾沒要刪（不在、太危險），圖片本來就留著
    }
    p.procs.retain(|d| !dangerous(d, &env.protected));
    p
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub bytes: u64,
    pub failed: usize,
    /// 刪不掉的前幾個
    pub failed_paths: Vec<PathBuf>,
    /// 資料資料夾裡還有不是繪匣建的東西，留著
    pub left: Option<PathBuf>,
    /// 留著的圖片
    pub kept_images: Option<PathBuf>,
}

impl Outcome {
    fn fail(&mut self, p: &Path) {
        self.failed += 1;
        if self.failed_paths.len() < 8 {
            self.failed_paths.push(p.to_path_buf());
        }
    }
}

/// 刪檔：唯讀的（git 的 objects 就是）先拿掉唯讀再刪
fn remove_file(path: &Path, len: u64, out: &mut Outcome) {
    let mut r = std::fs::remove_file(path);
    if r.is_err() {
        if let Ok(m) = std::fs::metadata(path) {
            let mut perm = m.permissions();
            if perm.readonly() {
                #[allow(clippy::permissions_set_readonly_false)] // Windows 的唯讀屬性，不是 Unix 的權限
                perm.set_readonly(false);
                let _ = std::fs::set_permissions(path, perm);
                r = std::fs::remove_file(path);
            }
        }
    }
    match r {
        Ok(()) => out.bytes += len,
        Err(_) => out.fail(path),
    }
}

/// 整棵刪掉：連結只拆連結（不跟進去）；keep 裡的和包著 keep 的資料夾留著
fn remove_tree(path: &Path, keep: &[PathBuf], out: &mut Outcome) {
    if keep.iter().any(|k| inside(path, k)) {
        return;
    }
    let Ok(m) = std::fs::symlink_metadata(path) else { return };
    if cache::linked(&m) {
        if std::fs::remove_dir(path).or_else(|_| std::fs::remove_file(path)).is_err() {
            out.fail(path);
        }
    } else if m.is_dir() {
        let before = out.failed;
        match std::fs::read_dir(path) {
            Ok(rd) => rd.flatten().for_each(|e| remove_tree(&e.path(), keep, out)),
            Err(_) => return out.fail(path),
        }
        if keep.iter().any(|k| inside(k, path)) {
            return;
        }
        if std::fs::remove_dir(path).is_err() && out.failed == before {
            out.fail(path); // 裡面都刪掉了、資料夾本身卻刪不掉才算（裡面刪不掉的已經算過）
        }
    } else {
        remove_file(path, m.len(), out);
    }
}

/// 刪掉空資料夾；deep＝裡面的空資料夾也一起清（不跟進連結）
fn prune(dir: &Path, deep: bool) {
    let Ok(m) = std::fs::symlink_metadata(dir) else { return };
    if !m.is_dir() || cache::linked(&m) {
        return;
    }
    if deep {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            prune(&e.path(), true);
        }
    }
    let _ = std::fs::remove_dir(dir); // 不是空的就失敗，不管它
}

pub fn execute(plan: &Plan, images: bool) -> Outcome {
    kill_under(plan);
    let mut out = Outcome::default();
    let mut keep: Vec<PathBuf> = plan.install_dir.iter().cloned().collect();
    // 勾了「連圖片一起刪」就不用留：圖片在 ComfyUI 裡面，跟著一起刪
    if let (Some(i), false) = (&plan.images, images) {
        keep.push(i.path.clone());
        out.kept_images = Some(i.path.clone());
    }
    for it in &plan.items {
        remove_tree(&it.path, &keep, &mut out);
    }
    if let Some(g) = &plan.unpack {
        let (bytes, _, skipped) = cache::clear_files(g);
        out.bytes += bytes;
        out.failed += skipped;
    }
    for (d, deep) in &plan.prune {
        prune(d, *deep);
    }
    out.left = plan.data_root.clone().filter(|d| d.exists());
    out
}

/// 關掉在要刪的資料夾裡跑的程式（ComfyUI 的 Python、llama-server、Node、ffmpeg、繪匣本身），檔案才刪得掉
fn kill_under(plan: &Plan) {
    let me = std::process::id();
    for (pid, exe) in processes() {
        if pid == me {
            continue;
        }
        let ours = plan.kill_install
            && plan.install_dir.as_ref().is_some_and(|d| inside(&exe, d))
            && exe.file_name().is_some_and(|n| ["anima-app.exe", "anima-server.exe"].iter().any(|x| n.eq_ignore_ascii_case(x)));
        if ours || plan.procs.iter().any(|d| inside(&exe, d)) {
            crate::pe::win::kill(pid as i64);
        }
    }
}

#[cfg(windows)]
fn processes() -> Vec<(u32, PathBuf)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let mut out = vec![];
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snap, &mut e) != 0;
        while more {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, e.th32ProcessID);
            if !h.is_null() {
                let mut buf = [0u16; 1024];
                let mut n = buf.len() as u32;
                if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut n) != 0 {
                    out.push((e.th32ProcessID, PathBuf::from(String::from_utf16_lossy(&buf[..n as usize]))));
                }
                CloseHandle(h);
            }
            more = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
    }
    out
}

#[cfg(not(windows))]
fn processes() -> Vec<(u32, PathBuf)> {
    vec![]
}

// ---------------------------------------------------------------- 視窗文字（跟著移除程式的語言）
struct Text {
    title: &'static str,
    instruction: &'static str,
    intro: &'static str,
    labels: [&'static str; 4], // Comfy、Data、Trash、AppDir
    unpack: &'static str,
    total: &'static str,
    note: &'static str,
    images_check: &'static str,
    all_btn: &'static str,
    keep_btn: &'static str,
    refused: &'static str,
    done: &'static str,
    failed: &'static str,
    kept: &'static str,
    left: &'static str,
}

const ZH_TW: Text = Text {
    title: "繪匣 PaintDeck",
    instruction: "要不要連模型、ComfyUI 和所有資料一起刪掉？",
    intro: "按「全部刪除」會永久刪除下面這些，不會進資源回收筒，刪了就救不回來：",
    labels: ["ComfyUI（模型、LoRA、工作流、自訂節點、AI 改寫模型）", "繪匣資料（圖庫紀錄、設定、提示詞庫、暫存）", "待刪除區", "程式設定與快取"],
    unpack: "ComfyUI 安裝暫存",
    total: "合計約 {}",
    note: "連結（junction）只拆掉連結，指到的檔案不會刪；自己加的外部模型資料夾也不會動。",
    images_check: "連生成的圖片一起刪（{}，約 {}）",
    all_btn: "全部刪除",
    keep_btn: "只移除程式，資料留著",
    refused: "這些位置太危險，不會刪：",
    done: "刪好了，空出約 {}。",
    failed: "有 {} 個檔案或資料夾刪不掉（可能正被其他程式開著），可以等一下手動刪：",
    kept: "生成的圖片留在：{}",
    left: "資料資料夾裡還有不是繪匣建的東西，留著沒刪：{}",
};

const ZH_CN: Text = Text {
    title: "绘匣 PaintDeck",
    instruction: "要不要连模型、ComfyUI 和所有数据一起删掉？",
    intro: "点「全部删除」会永久删除下面这些，不会进回收站，删了就找不回来：",
    labels: ["ComfyUI（模型、LoRA、工作流、自定义节点、AI 改写模型）", "绘匣数据（图库记录、设置、提示词库、缓存）", "待删除区", "程序设置与缓存"],
    unpack: "ComfyUI 安装缓存",
    total: "合计约 {}",
    note: "链接（junction）只拆掉链接，指向的文件不会删；自己添加的外部模型文件夹也不会动。",
    images_check: "连生成的图片一起删（{}，约 {}）",
    all_btn: "全部删除",
    keep_btn: "只卸载程序，数据留着",
    refused: "这些位置太危险，不会删：",
    done: "删好了，腾出约 {}。",
    failed: "有 {} 个文件或文件夹删不掉（可能正被其他程序打开），可以稍后手动删：",
    kept: "生成的图片留在：{}",
    left: "数据文件夹里还有不是绘匣建的东西，留着没删：{}",
};

const EN: Text = Text {
    title: "PaintDeck",
    instruction: "Also delete the models, ComfyUI and all data?",
    intro: "\"Delete everything\" permanently deletes the following. They do not go to the Recycle Bin and cannot be recovered:",
    labels: ["ComfyUI (models, LoRAs, workflows, custom nodes, AI rewrite models)", "PaintDeck data (gallery records, settings, prompt library, cache)", "Pending-delete folder", "App settings and cache"],
    unpack: "ComfyUI install cache",
    total: "Total about {}",
    note: "Junctions are only unlinked; the files they point to are kept. External model folders you added are not touched.",
    images_check: "Also delete generated images ({}, about {})",
    all_btn: "Delete everything",
    keep_btn: "Uninstall the app only, keep the data",
    refused: "These locations are too risky and will not be deleted:",
    done: "Done. Freed about {}.",
    failed: "{} files or folders could not be deleted (another program may have them open). You can delete them by hand later:",
    kept: "Generated images were kept in: {}",
    left: "The data folder still has files PaintDeck did not create, so it was kept: {}",
};

/// NSIS 的 $LANGUAGE（LANGID）：1028 繁中、2052 簡中、1033 英文；沒給就繁中
fn text(lang: &str) -> &'static Text {
    match lang {
        "2052" | "zh-CN" => &ZH_CN,
        "1033" | "en" => &EN,
        _ => &ZH_TW,
    }
}

fn human(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.0} MB", b as f64 / (1u64 << 20) as f64),
        b => format!("{} KB", b.div_ceil(1024)),
    }
}

fn fill(s: &str, args: &[&str]) -> String {
    args.iter().fold(s.to_string(), |acc, a| acc.replacen("{}", a, 1))
}

fn body(plan: &Plan, t: &Text) -> String {
    let mut s = format!("{}\n", t.intro);
    for i in &plan.items {
        let label = match i.kind {
            Kind::Comfy => t.labels[0],
            Kind::Data => t.labels[1],
            Kind::Trash => t.labels[2],
            _ => t.labels[3],
        };
        s += &format!("\n• {label}\n   {}　{}", i.path.display(), human(i.bytes));
    }
    if plan.unpack_bytes() > 0 {
        s += &format!("\n• {}　{}", t.unpack, human(plan.unpack_bytes()));
    }
    s += &format!("\n\n{}\n{}", fill(t.total, &[&human(plan.total())]), t.note);
    s
}

pub enum Answer {
    Keep,
    All { images: bool },
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

#[cfg(windows)]
fn ask(plan: &Plan, t: &Text) -> Answer {
    use windows_sys::Win32::UI::Controls::{
        TaskDialogIndirect, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TASKDIALOG_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION,
        TDF_SIZE_TO_CONTENT, TD_WARNING_ICON,
    };
    const ALL: i32 = 100;
    const KEEP: i32 = 101;
    let (title, main, content) = (wide(t.title), wide(t.instruction), wide(&body(plan, t)));
    let verify = plan.images.as_ref().map(|i| wide(&fill(t.images_check, &[&i.path.display().to_string(), &human(i.bytes)])));
    let footer = (!plan.refused.is_empty()).then(|| {
        wide(&plan.refused.iter().fold(t.refused.to_string(), |s, p| s + "\n" + &p.display().to_string()))
    });
    let (all, keep) = (wide(t.all_btn), wide(t.keep_btn));
    let buttons = [
        TASKDIALOG_BUTTON { nButtonID: ALL, pszButtonText: all.as_ptr() },
        TASKDIALOG_BUTTON { nButtonID: KEEP, pszButtonText: keep.as_ptr() },
    ];
    let mut c: TASKDIALOGCONFIG = unsafe { std::mem::zeroed() };
    c.cbSize = std::mem::size_of::<TASKDIALOGCONFIG>() as u32;
    c.dwFlags = TDF_ALLOW_DIALOG_CANCELLATION | TDF_SIZE_TO_CONTENT;
    c.pszWindowTitle = title.as_ptr();
    c.Anonymous1 = TASKDIALOGCONFIG_0 { pszMainIcon: TD_WARNING_ICON };
    c.pszMainInstruction = main.as_ptr();
    c.pszContent = content.as_ptr();
    c.cButtons = buttons.len() as u32;
    c.pButtons = buttons.as_ptr();
    c.nDefaultButton = KEEP;
    if let Some(v) = &verify {
        c.pszVerificationText = v.as_ptr();
    }
    if let Some(f) = &footer {
        c.pszFooter = f.as_ptr();
    }
    let (mut button, mut checked) = (0i32, 0i32);
    let hr = unsafe { TaskDialogIndirect(&c, &mut button, std::ptr::null_mut(), &mut checked) };
    if hr < 0 {
        // 叫不出 TaskDialog：退回一般的是／否（圖片照預設留著）
        let msg = format!("{}\n\n{}", t.instruction, body(plan, t));
        return if message(t.title, &msg, true) { Answer::All { images: false } } else { Answer::Keep };
    }
    if button == ALL {
        Answer::All { images: checked != 0 }
    } else {
        Answer::Keep
    }
}

#[cfg(not(windows))]
fn ask(_: &Plan, _: &Text) -> Answer {
    Answer::Keep
}

/// question＝是／否（預設「否」），否則是只有確定的通知；回傳有沒有按「是」
#[cfg(windows)]
fn message(title: &str, msg: &str, question: bool) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_DEFBUTTON2, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
    };
    let style = if question { MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2 } else { MB_OK | MB_ICONINFORMATION };
    let (text, title) = (wide(msg), wide(title));
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), style | MB_TOPMOST | MB_SETFOREGROUND) == IDYES }
}

#[cfg(not(windows))]
fn message(_: &str, _: &str, _: bool) -> bool {
    false
}

fn report(out: &Outcome, t: &Text) -> String {
    let mut s = fill(t.done, &[&human(out.bytes)]);
    if let Some(p) = &out.kept_images {
        s += &format!("\n\n{}", fill(t.kept, &[&p.display().to_string()]));
    }
    if let Some(p) = &out.left {
        s += &format!("\n\n{}", fill(t.left, &[&p.display().to_string()]));
    }
    if out.failed > 0 {
        s += &format!("\n\n{}", fill(t.failed, &[&out.failed.to_string()]));
        for p in &out.failed_paths {
            s += &format!("\n{}", p.display());
        }
    }
    s
}

/// `anima-app.exe --uninstall-cleanup [--lang <LANGID>]`；回傳結束碼（一律 0：資料刪不刪都不擋程式移除）
pub fn run_cli(args: &[String]) -> i32 {
    let lang = args.windows(2).find(|w| w[0] == "--lang").map_or("", |w| w[1].as_str());
    let t = text(lang);
    let plan = plan(&Env::from_system());
    let scripted = std::env::var("ANIMA_UNINSTALL_ANSWER").ok();
    if plan.is_empty() {
        if scripted.is_some() {
            println!("nothing to delete");
        }
        return 0;
    }
    let answer = match scripted.as_deref() {
        Some("yes") => Answer::All { images: false },
        Some("yes+images") => Answer::All { images: true },
        Some(_) => Answer::Keep,
        None => ask(&plan, t),
    };
    if scripted.is_some() {
        println!("{}\nrefused: {:?}", body(&plan, t), plan.refused);
    }
    let Answer::All { images } = answer else { return 0 };
    let out = execute(&plan, images);
    match scripted {
        Some(_) => println!("{}", report(&out, t)),
        None => {
            message(t.title, &report(&out, t), false);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root(tag: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../待刪除").join(format!("uninstall-{tag}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }
    fn put(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"fixture bytes").unwrap();
    }
    fn junction(link: &Path, target: &Path) -> bool {
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::process::Command::new("cmd").args(["/c", "mklink", "/J"]).arg(link).arg(target).output().is_ok_and(|o| o.status.success())
    }
    /// 精靈的預設擺法：<base>\AnimaStudio\anima_data、ComfyUI_windows_portable，設定在 <base>\appdata\local.anima.studio
    fn station(base: &Path) -> (Env, Config) {
        let studio = base.join("AnimaStudio");
        let cfg = Config { data_root: studio.join("anima_data"), comfy_dir: studio.join("ComfyUI_windows_portable") };
        let appdir = base.join("appdata").join(APP_ID);
        std::fs::create_dir_all(&appdir).unwrap();
        std::fs::write(appdir.join("config.json"), serde_json::to_vec(&serde_json::json!({"data_root": cfg.data_root, "comfy_dir": cfg.comfy_dir})).unwrap()).unwrap();
        put(&base.join("local").join(APP_ID).join("EBWebView/x.bin"));
        let comfy = cfg.comfy_dir.join("ComfyUI");
        for f in ["main.py", "models/loras/a.safetensors", "models/diffusion_models/m.safetensors", "user/default/workflows/w.json",
            "custom_nodes/anima_prompt_enhancer/bin/llama-server.exe", "output/AnimaWeb/1.png", "input/ref.png", ".git/objects/ab/cd"] {
            put(&comfy.join(f));
        }
        put(&cfg.comfy_dir.join("python_embeded/python.exe"));
        let ro = comfy.join(".git/objects/ab/cd");
        let mut perm = std::fs::metadata(&ro).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&ro, perm).unwrap();
        for f in ["data/settings.json", "data/anima.db", "data/_dl/x.part", "data/tools/node/node.exe", "待刪除/安裝用的壓縮檔_20261030_010101/a.7z"] {
            put(&cfg.data_root.join(f));
        }
        // ComfyUI 安裝暫存（有歸屬紀錄的 _anima_u、壓縮檔）
        let unpack = studio.join("_anima_u");
        put(&unpack.join("ComfyUI_windows_portable/left.bin"));
        put(&studio.join("_anima_dl/ComfyUI_windows_portable_nvidia.7z.part"));
        std::fs::write(cfg.data_root.join("data/installer_cache.json"),
            serde_json::to_vec(&serde_json::json!({"comfy_dir": cfg.comfy_dir, "unpack": unpack})).unwrap()).unwrap();
        let env = Env {
            config: appdir.join("config.json"),
            app_dirs: vec![appdir, base.join("local").join(APP_ID)],
            install_dir: Some(base.join("program")),
            protected: vec![base.join("profile")],
            kill_install: false,
        };
        put(&base.join("program/anima-app.exe"));
        (env, cfg)
    }

    #[test]
    fn deletes_everything_but_keeps_images_and_junction_targets() {
        let base = root("keep");
        let (env, cfg) = station(&base);
        let elsewhere = base.join("my_models/keep.safetensors");
        put(&elsewhere);
        assert!(junction(&cfg.comfy_dir.join("ComfyUI/models/checkpoints"), elsewhere.parent().unwrap()), "junction fixture");
        let plan = plan(&env);
        let kinds: Vec<Kind> = plan.items.iter().map(|i| i.kind).collect();
        assert_eq!(kinds, [Kind::Comfy, Kind::Data, Kind::Trash, Kind::AppDir, Kind::AppDir]);
        assert!(plan.refused.is_empty());
        assert_eq!(plan.images.as_ref().unwrap().bytes, 13);
        assert_eq!(plan.unpack_bytes(), 26);
        assert_eq!(plan.items[0].bytes, 13 * 8); // 圖片、連結指到的、程式資料夾都不算
        let out = execute(&plan, false);
        assert_eq!((out.failed, &out.failed_paths), (0, &vec![]));
        let comfy = cfg.comfy_dir.join("ComfyUI");
        assert!(comfy.join("output/AnimaWeb/1.png").is_file());
        for gone in [comfy.join("models"), comfy.join("user"), comfy.join("custom_nodes"), comfy.join(".git"), comfy.join("input"),
            cfg.comfy_dir.join("python_embeded"), cfg.data_root.clone(), base.join("AnimaStudio/_anima_u"), base.join("AnimaStudio/_anima_dl"),
            base.join("appdata").join(APP_ID), base.join("local").join(APP_ID)] {
            assert!(!gone.exists(), "{}", gone.display());
        }
        assert!(elsewhere.is_file() && std::fs::read_dir(elsewhere.parent().unwrap()).unwrap().count() == 1);
        assert!(base.join("program/anima-app.exe").is_file());
        assert_eq!(out.kept_images.as_deref(), Some(comfy.join("output").as_path()));
        assert!(out.left.is_none());
        assert_eq!(out.bytes, plan.total());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn images_too_and_foreign_files_in_the_data_folder_survive() {
        let base = root("all");
        let (mut env, cfg) = station(&base);
        put(&cfg.data_root.join("my_notes.txt"));
        // 程式裝在 ComfyUI 資料夾裡：程式檔留給 NSIS 刪
        env.install_dir = Some(cfg.comfy_dir.join("app"));
        put(&cfg.comfy_dir.join("app/anima-app.exe"));
        let out = execute(&plan(&env), true);
        assert_eq!(out.failed, 0, "{:?}", out.failed_paths);
        assert!(!cfg.comfy_dir.join("ComfyUI").exists());
        assert!(cfg.comfy_dir.join("app/anima-app.exe").is_file());
        assert!(cfg.data_root.join("my_notes.txt").is_file() && !cfg.data_root.join("data").exists());
        assert_eq!(out.left.as_deref(), Some(cfg.data_root.as_path()));
        assert!(out.kept_images.is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn empty_outer_folder_is_removed() {
        let base = root("outer");
        let (env, _) = station(&base);
        execute(&plan(&env), true);
        assert!(!base.join("AnimaStudio").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn protected_locations_are_refused() {
        assert!(dangerous(Path::new("D:\\"), &[]));
        assert!(dangerous(Path::new("relative\\dir"), &[]));
        let home = PathBuf::from("C:\\Users\\someone");
        assert!(dangerous(&home, std::slice::from_ref(&home)));
        assert!(dangerous(Path::new("c:\\users"), std::slice::from_ref(&home)));
        assert!(!dangerous(&home.join("AnimaStudio"), std::slice::from_ref(&home)));
        let base = root("protected");
        let (mut env, _) = station(&base);
        let profile = base.join("profile");
        std::fs::create_dir_all(profile.join("stuff")).unwrap();
        // ComfyUI 位置指到使用者資料夾的上層：整個不刪，連圖片勾選都不出現
        std::fs::write(&env.config, serde_json::to_vec(&serde_json::json!({"data_root": base.join("AnimaStudio/anima_data"), "comfy_dir": base})).unwrap()).unwrap();
        env.app_dirs.truncate(1);
        let plan = plan(&env);
        assert_eq!(plan.refused, vec![base.clone()]);
        assert!(plan.images.is_none() && plan.unpack_bytes() == 0);
        assert!(plan.items.iter().all(|i| i.kind != Kind::Comfy));
        execute(&plan, true);
        assert!(profile.join("stuff").is_dir() && base.join("AnimaStudio/ComfyUI_windows_portable/ComfyUI/main.py").is_file());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn no_config_means_only_app_dirs() {
        let base = root("noconfig");
        let (env, cfg) = station(&base);
        std::fs::remove_file(&env.config).unwrap();
        let plan = plan(&env);
        assert!(plan.items.iter().all(|i| i.kind == Kind::AppDir) && plan.images.is_none());
        execute(&plan, true);
        assert!(cfg.comfy_dir.join("ComfyUI/main.py").is_file());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn language_follows_the_uninstaller() {
        assert_eq!(text("1028").all_btn, "全部刪除");
        assert_eq!(text("2052").all_btn, "全部删除");
        assert_eq!(text("1033").all_btn, "Delete everything");
        assert_eq!(text("").all_btn, "全部刪除");
        assert_eq!(human(41_700_000_000), "38.8 GB");
        assert_eq!(human(5 << 20), "5 MB");
        assert_eq!((human(16_384), human(1), human(0)), ("16 KB".into(), "1 KB".into(), "0 KB".into()));
    }
}
