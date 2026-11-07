//! 資料夾位置（對照舊版 server.py 開頭那一串常數）。
//!
//! root：生圖站的資料根目錄（裡面有 data\、待刪除\）；開發時指向舊版 E:\AI\anima_ui，新舊兩版共用同一份圖庫。
//! comfy_root：ComfyUI 資料夾；預設是 root 旁邊的 ComfyUI_windows_portable\ComfyUI，可用環境變數 COMFY_ROOT 指定。
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Paths {
    pub root: PathBuf,
    pub data: PathBuf,
    pub web: PathBuf,
    pub comfy_root: PathBuf,
    pub input: PathBuf,
    pub output: PathBuf,
    pub loras: PathBuf,
    pub pe_node: PathBuf,
    /// 生圖站的 ComfyUI 自己的主模型資料夾；要連外部模型資料夾一起找用 unet_roots()
    pub unet_dirs: Vec<PathBuf>,
    pub thumbs: PathBuf,
    pub previews: PathBuf,
    pub settings: PathBuf,
    pub db: PathBuf,
    /// 圖庫回收區：output\AnimaWeb\_trash
    pub trash: PathBuf,
    /// 程式附帶的檔案（resources：姿勢大彙整、姿勢參考圖…）
    pub res: PathBuf,
}

/// 執行檔的檔名：Windows 加 .exe，Linux 原樣（llama-server、ffmpeg、node…）
pub fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// 資料夾 d 裡的 Python 可能放的位置。venv＝d 是 venv（Windows：Scripts\python.exe）；
/// 不是 venv 的（Windows 的 python_embeded、秋葉的 python、conda 環境）python.exe 直接在 d 裡。
/// Linux 不分：venv、conda 都是 d/bin/python（有的只有 python3）
pub fn python_paths(d: &Path, venv: bool) -> Vec<PathBuf> {
    if cfg!(windows) {
        vec![if venv { d.join("Scripts").join("python.exe") } else { d.join("python.exe") }]
    } else {
        vec![d.join("bin").join("python"), d.join("bin").join("python3")]
    }
}

/// venv 的 Python（要建 venv、或只要一個位置時用；Linux 是 bin/python）
pub fn venv_python(venv: &Path) -> PathBuf {
    python_paths(venv, true).swap_remove(0)
}

/// 是不是 Python 執行檔的檔名：Windows 只認 python.exe；Linux 認 python、python3、python3.12 這類
pub fn is_python_name(name: &str) -> bool {
    if cfg!(windows) {
        name.eq_ignore_ascii_case("python.exe")
    } else {
        name == "python"
            || name.strip_prefix("python3").is_some_and(|v| v.is_empty() || (v.starts_with('.') && v[1..].chars().all(|c| c.is_ascii_digit()) && v.len() > 1))
    }
}

/// resources 資料夾：環境變數 ANIMA_RES → 程式旁邊的 resources（Windows 安裝版）→
/// Linux 安裝版（deb：/usr/bin/anima-app 配 /usr/lib/<產品名>/resources；AppImage 同樣在 usr/lib 底下）→ 開發時的 anima_app\resources
pub fn default_res() -> PathBuf {
    if let Some(p) = std::env::var_os("ANIMA_RES").filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    if let Some(p) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("resources")))
        .filter(|p| p.join("poses").is_dir())
    {
        return p;
    }
    if let Some(p) = linux_lib_dir().map(|d| d.join("resources")).filter(|p| p.join("poses").is_dir()) {
        return p;
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("resources")
}

/// Linux 安裝版放附帶檔案的資料夾：程式在 <prefix>/bin，附帶檔在 <prefix>/lib/<產品名>（Tauri 的 deb、AppImage 都這樣擺）。
/// 產品名有中文和空白，直接找 lib 底下有 resources/poses 的那一個。Windows、找不到都回 None
pub fn linux_lib_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let lib = exe.parent()?.parent()?.join("lib");
    let named = lib.join("繪匣 PaintDeck");
    if named.join("resources").join("poses").is_dir() {
        return Some(named);
    }
    std::fs::read_dir(&lib)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|d| d.join("resources").join("poses").is_dir() && d.join("ui").is_dir())
}

impl Paths {
    pub fn new(root: &Path, web: &Path, comfy_root: Option<PathBuf>) -> Paths {
        let root = root.to_path_buf();
        let data = root.join("data");
        let comfy_root = comfy_root.unwrap_or_else(|| {
            root.parent()
                .unwrap_or(&root)
                .join("ComfyUI_windows_portable")
                .join("ComfyUI")
        });
        let models = comfy_root.join("models");
        let output = comfy_root.join("output");
        Paths {
            web: web.to_path_buf(),
            input: comfy_root.join("input"),
            loras: models.join("loras"),
            pe_node: comfy_root
                .join("custom_nodes")
                .join("anima_prompt_enhancer"),
            unet_dirs: vec![models.join("diffusion_models"), models.join("unet")],
            thumbs: data.join("thumbs"),
            previews: data.join("previews"),
            settings: data.join("settings.json"),
            db: data.join("anima.db"),
            trash: output.join("AnimaWeb").join("_trash"),
            res: default_res(),
            output,
            comfy_root,
            data,
            root,
        }
    }

    /// 主模型的資料夾：生圖站的 ComfyUI＋外部模型資料夾（model_dirs.rs）。順序跟 ComfyUI 找檔一樣，自己的先。
    /// 最後接 ComfyUI 自己的 models\checkpoints：SDXL 是整包 checkpoint（內建流程用 CheckpointLoaderSimple 載），
    /// 要找得到檔才判得出家族（外部的 checkpoints 資料夾 model_dirs::effective 本來就算主模型資料夾）
    pub fn unet_roots(&self) -> Vec<PathBuf> {
        let mut v = self.unet_dirs.clone();
        v.extend(crate::model_dirs::roots("diffusion_models"));
        v.push(self.comfy_root.join("models").join("checkpoints"));
        v
    }

    /// LoRA 的資料夾：生圖站的 ComfyUI＋外部模型資料夾。下載新的 LoRA 一律放 self.loras
    pub fn lora_roots(&self) -> Vec<PathBuf> {
        let mut v = vec![self.loras.clone()];
        v.extend(crate::model_dirs::roots("loras"));
        v
    }

    /// 從環境變數建立：ANIMA_ROOT（必填）、ANIMA_WEB（網頁檔，預設 root\web）、COMFY_ROOT（選填）
    pub fn from_env() -> Option<Paths> {
        let root = PathBuf::from(std::env::var_os("ANIMA_ROOT")?);
        let web = std::env::var_os("ANIMA_WEB")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("web"));
        let comfy = std::env::var_os("COMFY_ROOT").map(PathBuf::from);
        Some(Paths::new(&root, &web, comfy))
    }
}
