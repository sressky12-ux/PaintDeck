//! data\settings.json：讀寫與預設值（對照舊版 server.py 的 DEFAULT_SETTINGS／load_settings／save_settings）。
//!
//! Civitai API key 存在這個檔裡：任何回傳給網頁、匯出、記錄的東西都必須經過 public_view() 或 without_key()，
//! 絕對不能把金鑰本身送出去或印出來。
use base64::Engine;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::io;
use std::path::Path;

pub const KEY: &str = "civitai_key";
const PROTECTED_KEY: &str = "civitai_key_dpapi";

#[cfg(windows)]
pub(crate) fn protect(key: &str) -> io::Result<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    let bytes = key.as_bytes();
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let encoded = base64::engine::general_purpose::STANDARD
        .encode(unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) });
    unsafe {
        LocalFree(output.pbData as *mut _);
    }
    Ok(encoded)
}

#[cfg(windows)]
pub(crate) fn unprotect(encoded: &str) -> io::Result<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(io::Error::other)?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let plain =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        LocalFree(output.pbData as *mut _);
    }
    String::from_utf8(plain).map_err(io::Error::other)
}

/// 預設的負面提示詞：使用者沒同意不准改
pub const DEFAULT_NEGATIVE: &str = "worst quality, low quality, score_1, score_2, score_3, blurry, jpeg artifacts, sepia, bad anatomy, bad hands, extra fingers, watermark, text";

/// AI 改寫的預設值（舊版 pe.py 的 DEFAULTS）
pub fn pe_defaults() -> Map<String, Value> {
    let v = json!({"pe_mode": "gpu", "pe_partial_layers": 16, "pe_idle": 120, "pe_close_after": false,
                   "pe_temperature": 1.0, "pe_think": false, "pe_zh": false, "pe_general": "auto"});
    v.as_object().cloned().unwrap_or_default()
}

pub fn defaults() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(KEY.into(), json!(""));
    m.insert("blur_nsfw".into(), json!(true));
    m.insert("negative".into(), json!(DEFAULT_NEGATIVE));
    // 影片模型：內建 MiniMax H3，或 data\video_workflows 裡匯入的工作流代號（video_wf.rs）
    m.insert("video_engine".into(), json!(crate::video_wf::BUILTIN));
    // 取代內建功能：{"<家族>.<模式>": "<工作流組代號>"}（image_wf.rs），例如 anima.inpaint
    m.insert("feature_flows".into(), json!({}));
    m.extend(pe_defaults());
    m
}

/// 預設值＋檔案裡的值（檔案壞掉或不存在就只有預設值）
pub fn load(path: &Path) -> Map<String, Value> {
    let mut s = defaults();
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Ok(Value::Object(mut file)) = serde_json::from_str::<Value>(&text) {
            #[cfg(windows)]
            if let Some(blob) = file.get(PROTECTED_KEY).and_then(Value::as_str) {
                if let Ok(key) = unprotect(blob) {
                    file.insert(KEY.into(), json!(key));
                    file.remove(PROTECTED_KEY);
                }
            }
            s.extend(file);
        }
    }
    // LTX 退場：舊設定的 ltx23 讀成 h3
    if let Some(e) = s.get("video_engine").and_then(Value::as_str) {
        let e = crate::video_wf::normalize_engine(e).to_string();
        s.insert("video_engine".into(), json!(e));
    }
    s
}

/// json.dump(s, f, ensure_ascii=False, indent=1)
pub fn to_text(v: &Value) -> String {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(
        &mut out,
        serde_json::ser::PrettyFormatter::with_indent(b" "),
    );
    v.serialize(&mut ser).expect("寫進記憶體不會失敗");
    String::from_utf8(out).expect("serde_json 只會寫出 UTF-8")
}

pub fn save(path: &Path, s: &Map<String, Value>) -> io::Result<()> {
    // 先寫暫存檔再換名，寫到一半當機也不會弄壞設定檔
    let mut disk = s.clone();
    #[cfg(windows)]
    {
        let key = disk
            .remove(KEY)
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        if !key.is_empty() {
            disk.insert(PROTECTED_KEY.into(), json!(protect(&key)?));
        }
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, to_text(&Value::Object(disk)))?;
    std::fs::rename(&tmp, path)
}

/// 舊版明文設定在第一次啟動時就轉存；若失敗，不啟動以免繼續暴露金鑰。
pub fn migrate_plaintext_key(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    if let Ok(bytes) = std::fs::read(path) {
        if let Ok(Value::Object(file)) = serde_json::from_slice::<Value>(&bytes) {
            if file
                .get(KEY)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
            {
                return save(path, &load(path));
            }
        }
    }
    Ok(())
}

pub fn clear_key(s: &mut Map<String, Value>) {
    s.insert(KEY.into(), json!(""));
    s.remove(PROTECTED_KEY);
}

/// 拿掉金鑰（匯出、備份用）
pub fn without_key(s: &Map<String, Value>) -> Map<String, Value> {
    let mut m = s.clone();
    m.remove(KEY);
    m.remove(PROTECTED_KEY);
    m
}

/// 給網頁看的設定：金鑰只回報「有沒有填」
pub fn public_view(s: &Map<String, Value>) -> Value {
    let has = s
        .get(KEY)
        .and_then(Value::as_str)
        .map(|k| !k.is_empty())
        .unwrap_or(false);
    let mut m = without_key(s);
    m.insert("has_civitai_key".into(), json!(has));
    Value::Object(m)
}

/// 金鑰本身（只給 Civitai 請求用）
pub fn civitai_key(s: &Map<String, Value>) -> String {
    s.get(KEY).and_then(Value::as_str).unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LTX 退場（2026-10-30）：沒寫、或寫著舊的 ltx23，載入後都是 h3
    #[test]
    fn video_engine_defaults_to_h3() {
        let dir = std::env::temp_dir().join(format!("anima_settings_video_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.json");
        std::fs::write(&p, r#"{"video_engine": "ltx23"}"#).unwrap();
        assert_eq!(load(&p)["video_engine"], "h3");
        std::fs::write(&p, "{}").unwrap();
        assert_eq!(load(&p)["video_engine"], "h3");
        std::fs::write(&p, r#"{"video_engine": "Wan 2_2"}"#).unwrap();
        assert_eq!(load(&p)["video_engine"], "Wan 2_2");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn key_never_in_public_view() {
        let mut s = defaults();
        s.insert(KEY.into(), json!("SECRET-TEST-VALUE"));
        let v = public_view(&s);
        assert!(!v.to_string().contains("SECRET-TEST-VALUE"));
        assert_eq!(v["has_civitai_key"], json!(true));
        assert!(!Value::Object(without_key(&s))
            .to_string()
            .contains("SECRET-TEST-VALUE"));
    }

    #[test]
    fn indent_like_python() {
        // Python: json.dumps({"a": [1, 2], "b": {}}, indent=1)
        assert_eq!(
            to_text(&json!({"a": [1, 2], "b": {}})),
            "{\n \"a\": [\n  1,\n  2\n ],\n \"b\": {}\n}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn key_is_encrypted_on_disk_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!(
            "anima_settings_test_{}_{}",
            std::process::id(),
            rand::random::<u32>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let mut s = defaults();
        s.insert(KEY.into(), json!("SECRET-TEST-VALUE"));
        save(&path, &s).unwrap();
        let disk = std::fs::read_to_string(&path).unwrap();
        assert!(!disk.contains("SECRET-TEST-VALUE"));
        assert!(disk.contains(PROTECTED_KEY));
        assert_eq!(civitai_key(&load(&path)), "SECRET-TEST-VALUE");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
