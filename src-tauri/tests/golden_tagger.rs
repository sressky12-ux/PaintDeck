//! 反推 golden test：tests/make_golden_tagger.py 用舊版 tagger.py（Pillow＋onnxruntime）產生的結果。
//!
//! - 前處理（不需要模型）：疊白底、補正方形、BICUBIC 縮到 448 的 RGB 位元組要跟 Pillow 一模一樣（JPEG 例外：解碼器不同，只報差異）
//! - 反推結果（#[ignore]，要模型和 onnxruntime.dll）：ORT_DYLIB_PATH=…\onnxruntime.dll cargo test --test golden_tagger -- --ignored
mod common;

use anima_app::tagger;
use common::golden_dir;
use serde_json::Value;
use sha2::{Digest, Sha256};

fn load() -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(golden_dir().join("golden_tagger.json"))
            .expect("先跑 make_golden_tagger.py"),
    )
    .unwrap()
}

fn hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

#[test]
fn preprocess_matches_pillow() {
    let data = load();
    let dir = golden_dir().join("tagger");
    let mut fails = vec![];
    let mut n = 0;
    for c in data["preprocess"].as_array().unwrap() {
        let f = c["file"].as_str().unwrap();
        let rgba = tagger::load_rgba(&dir.join(f)).unwrap();
        let got = tagger::preprocess(&rgba, 448).into_raw();
        if hex(&got) == c["sha256"].as_str().unwrap() {
            n += 1;
            continue;
        }
        let want = std::fs::read(dir.join(format!("{f}.448.bin"))).unwrap();
        let diff: Vec<i32> = got
            .iter()
            .zip(&want)
            .map(|(a, b)| *a as i32 - *b as i32)
            .filter(|d| *d != 0)
            .collect();
        let max = diff.iter().map(|d| d.abs()).max().unwrap_or(0);
        let msg = format!(
            "{f}：{} / {} 個值不同，最大差 {max}",
            diff.len(),
            want.len()
        );
        if c["jpeg"].as_bool() == Some(true) {
            eprintln!("（JPEG 解碼器不同，只記錄）{msg}");
            n += 1;
        } else {
            fails.push(msg);
        }
    }
    for c in data["fmt"].as_array().unwrap() {
        let got = tagger::fmt_tag(c[0].as_str().unwrap());
        if got != c[1].as_str().unwrap() {
            fails.push(format!("fmt_tag({}) python={} rust={got}", c[0], c[1]));
        }
        n += 1;
    }
    // 2026-11-12 護欄已拆：Rust 端 SKIP 永遠空，不再跟 python 的 golden 清單比對
    let _ = data["skip"].as_array();
    assert!(fails.is_empty(), "\n{}", fails.join("\n"));
    eprintln!("反推前處理 golden：{n} 個案例一致");
}

/// 評級一樣、每個 tag 分數差 ≤ 0.02、只有一邊有的 tag 分數離門檻不到 0.02
fn close(a: &Value, b: &Value, general: f64, character: f64) -> bool {
    const TOL: f64 = 0.02;
    if a["rating"] != b["rating"] {
        return false;
    }
    for (k, thr) in [("general", general), ("character", character)] {
        let map = |v: &Value| -> std::collections::HashMap<String, f64> {
            v[k].as_array()
                .unwrap()
                .iter()
                .map(|x| (x[0].as_str().unwrap().to_string(), x[1].as_f64().unwrap()))
                .collect()
        };
        let (x, y) = (map(a), map(b));
        for (t, p) in x.iter().chain(y.iter()) {
            let ok = match (x.get(t), y.get(t)) {
                (Some(p1), Some(p2)) => (p1 - p2).abs() <= TOL,
                _ => (p - thr).abs() <= TOL,
            };
            if !ok {
                return false;
            }
        }
    }
    true
}

#[test]
#[ignore]
fn tags_match_python() {
    let data = load();
    let dir = golden_dir().join("tagger");
    let model = std::path::PathBuf::from(
        std::env::var("ANIMA_TAGGER_DIR")
            .unwrap_or_else(|_| r"E:\AI\anima_ui\data\models\tagger".into()),
    );
    let t = tagger::Tagger::new(&model);
    let mut fails = vec![];
    for c in data["tag"].as_array().unwrap() {
        let original = c["path"].as_str().unwrap();
        let mut path = std::path::PathBuf::from(original);
        if !path.exists() {
            // 舊版 golden 在 E: 建立；目前 ComfyUI 已搬到 D:。
            if let Some(rest) = original.strip_prefix(r"E:\AI\ComfyUI_windows_portable\") {
                path = std::path::PathBuf::from(r"D:\AI\ComfyUI_windows_portable").join(rest);
            }
        }
        let t0 = std::time::Instant::now();
        let got = t
            .tag(
                &path,
                c["general"].as_f64().unwrap(),
                c["character"].as_f64().unwrap(),
            )
            .unwrap();
        let secs = t0.elapsed().as_secs_f64();
        // 原始機率的最大差
        let want: Vec<f32> = std::fs::read(dir.join(c["probs"].as_str().unwrap()))
            .unwrap()
            .chunks(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        let probs = t.probs(&tagger::load_rgba(&path).unwrap()).unwrap();
        let max = probs
            .iter()
            .zip(&want)
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        let jpeg = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"));
        // PNG 要完全一樣；JPEG 兩邊解碼器不同（Pillow 用 libjpeg-turbo），容許分數差 0.02、門檻附近的 tag 有出入
        let same = got == c["result"]
            || (jpeg
                && close(
                    &got,
                    &c["result"],
                    c["general"].as_f64().unwrap(),
                    c["character"].as_f64().unwrap(),
                ));
        let how = if got == c["result"] {
            "完全一致"
        } else if same {
            "在容許誤差內"
        } else {
            "不同"
        };
        eprintln!(
            "{}：{:.2} 秒，機率最大差 {max:e}，結果{how}",
            path.file_name().unwrap().to_string_lossy(),
            secs
        );
        if !same {
            fails.push(format!(
                "{}\n python={}\n rust  ={got}",
                path.display(),
                c["result"]
            ));
        }
    }
    assert!(fails.is_empty(), "\n{}", fails.join("\n"));
}
