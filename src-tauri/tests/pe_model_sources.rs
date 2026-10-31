use anima_app::{gpu, model_dirs, pe::Core};
use std::path::{Path, PathBuf};

fn source(path: &Path) -> model_dirs::Dir {
    model_dirs::Dir {
        kind: "LLM".into(), path: path.to_path_buf(), source: "manual".into(), added: String::new(),
    }
}

/// 用假的模型檔驗證設定讀取及定位，不啟動 LLM、ComfyUI、網頁站或瀏覽器。
/// 2026-10-30 官方 PE 拿掉了：改成找通用模型（Defiant Fable 9B），PE 檔放在哪個來源都不會被挑到
#[test]
fn pe_finds_model_added_in_settings_without_restart() {
    assert!(std::env::var_os("ANIMA_PE_MODELS").filter(|v| !v.is_empty()).is_none(),
        "Run this isolated test with ANIMA_PE_MODELS unset or empty");
    let unique = format!("pe-flow-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../待刪除").join(unique);
    let data = base.join("data");
    let comfy = base.join("AnimaStudio/ComfyUI_windows_portable/ComfyUI");
    let node = comfy.join("custom_nodes/anima_prompt_enhancer");
    let own = comfy.join("models/LLM");
    let first = base.join("first/LLM");
    let second = base.join("second/LLM");
    let nested = second.join("PE/中文子目錄");
    for dir in [&node, &own, &first, &nested] { std::fs::create_dir_all(dir).unwrap(); }
    // main.py 的標記阻止模型來源查詢回頭讀 APPDATA 中的真 ComfyUI 設定。
    std::fs::write(comfy.join("main.py"), b"# isolated fixture").unwrap();
    gpu::init(&base.join("comfy.json"));
    model_dirs::init(&data);
    model_dirs::init_comfy(&comfy);
    // 只有官方 PE（舊版留下的）：當成沒有改寫模型，講清楚去哪下載通用 9B
    std::fs::write(first.join("pe_t2i_heretic-Q4_K_M.gguf"), b"fixture").unwrap();
    model_dirs::save(&data, &comfy, vec![source(&first)]).unwrap();
    let core = Core::new(&node);
    let missing = core.model_files("t2i", None).unwrap_err().0;
    assert!(missing.contains("找不到 AI 改寫用的通用模型") && missing.contains("安裝與修復"));
    assert!(missing.contains("AI 改寫、生圖助手") && missing.contains("不再支援"));

    let fable = nested.join("QWEN3.5-9B-THE-DEFIANT-FABLE-Uncnr-Heretic-NEO-MAX-IQ3_M.GGUF");
    let mmproj = first.join("Qwen3.5-9B-Defiant-Fable-mmproj-F16.gguf");
    let pe_i2i = nested.join("pe_i2i_heretic-Q6_K.gguf");
    for file in [&fable, &mmproj, &pe_i2i] { std::fs::write(file, b"fixture").unwrap(); }
    model_dirs::save(&data, &comfy, vec![source(&first), source(&second)]).unwrap();
    // 舊的 t2i／i2i 名稱導向通用模型；i2i 要看圖檔，跨來源配得到
    assert_eq!(core.model_files("t2i", None).unwrap(), (fable.clone(), None));
    assert_eq!(core.model_files("i2i", None).unwrap(), (fable.clone(), Some(mmproj)));
    // 同一個 Core 不重建；從儲存的設定重新載入仍能定位第二個來源。
    model_dirs::init(&data);
    assert_eq!(core.model_files("adult", None).unwrap().0, fable);
    // 在測試設定移除該來源後不沿用舊位置，也不自動複製／下載模型。
    model_dirs::save(&data, &comfy, vec![source(&first)]).unwrap();
    assert!(core.model_files("adult", None).is_err());
    assert!(std::fs::read_dir(&own).unwrap().next().is_none());
    assert!(!node.join("server_state.json").exists());
    assert!(!node.join("server.lock").exists());
    // 測試資料留在待刪除，交由使用者決定清理。
}
