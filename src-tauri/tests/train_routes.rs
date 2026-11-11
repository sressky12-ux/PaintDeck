//! 訓練台（/api/train/*）路由的整合測試：真的開一個伺服器，資料放暫存資料夾。不需要模型、顯卡、訓練器。
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

async fn station(name: &str) -> (String, PathBuf) {
    let root = std::env::temp_dir().join("anima_train_route_test").join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let web = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("ui");
    let paths = anima_app::paths::Paths::new(&root, &web, Some(root.join("comfy")));
    let app = anima_app::server::start(paths, "http://127.0.0.1:9").await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(anima_app::server::serve_on(anima_app::server::router(app), listener));
    (format!("http://{addr}"), root)
}

async fn call(method: &str, base: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let c = reqwest::Client::new();
    let url = format!("{base}{path}");
    let mut r = match method {
        "GET" => c.get(url),
        "DELETE" => c.delete(url),
        _ => c.post(url),
    };
    if let Some(b) = body {
        r = r.json(&b);
    }
    let r = r.send().await.unwrap();
    let code = r.status().as_u16();
    (code, r.json().await.unwrap_or(Value::Null))
}

/// 自己畫的測試圖：色塊＋漸層，seed 不同圖就不同
fn png(path: &Path, w: u32, h: u32, seed: u32) {
    let im = image::RgbImage::from_fn(w, h, |x, y| {
        let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
        let cells = 2 + seed % 5;
        let block = if ((fx * cells as f32) as u32 + (fy * (cells + 1) as f32) as u32 + seed).is_multiple_of(2) { 210.0 } else { 30.0 };
        let v = (block * 0.8 + fx * 40.0) as u8;
        image::Rgb([v, ((fy * 255.0) as u32 ^ (seed * 40)) as u8, 255 - v])
    });
    im.save(path).unwrap();
}

#[tokio::test]
async fn train_project_images_captions() {
    let (base, root) = station("basic").await;
    let (code, v) = call("GET", &base, "/api/train", None).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["families"].as_array().unwrap().len(), 6);
    assert_eq!(v["projects"], json!([]));
    assert_eq!(v["trainer"]["installed"], false);

    // 建專案：觸發詞不合格擋掉；沒給就自動產生
    let (code, v) = call("POST", &base, "/api/train", Some(json!({"name": "x", "kind": "style", "family": "sdxl", "trigger": "a,b"}))).await;
    assert_eq!(code, 400, "{v}");
    let (code, v) = call("POST", &base, "/api/train", Some(json!({"name": "測試畫風", "kind": "style", "family": "sdxl"}))).await;
    assert_eq!(code, 200, "{v}");
    let id = v["project"]["id"].as_str().unwrap().to_string();
    let trigger = v["project"]["trigger"].as_str().unwrap().to_string();
    assert!(trigger.ends_with("style"), "{trigger}");
    let p = format!("/api/train/{id}");

    // 匯入：兩張好圖、一張假圖（文字檔改名）；原圖不能被動到
    let src = root.join("src 圖");
    std::fs::create_dir_all(&src).unwrap();
    png(&src.join("甲 (1).png"), 600, 400, 1);
    png(&src.join("b.png"), 400, 600, 2);
    std::fs::write(src.join("c.png"), "not an image").unwrap();
    let before = std::fs::read(src.join("b.png")).unwrap();
    let paths: Vec<String> = ["甲 (1).png", "b.png", "c.png"].iter().map(|n| src.join(n).to_string_lossy().into_owned()).collect();
    let (code, v) = call("POST", &base, &format!("{p}/images"), Some(json!({"paths": paths}))).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["added"], 2, "{v}");
    assert_eq!(v["skipped"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["skipped"][0]["name"], "c.png");
    assert_eq!(v["report"]["count"], 2);
    assert_eq!(std::fs::read(src.join("b.png")).unwrap(), before);
    // 整個資料夾再丟一次：多兩張，而且體檢要抓到重複
    let (code, v) = call("POST", &base, &format!("{p}/images"), Some(json!({"paths": [src.to_string_lossy()]}))).await;
    assert_eq!((code, v["added"].as_u64()), (200, Some(2)), "{v}");
    assert_eq!(v["report"]["dupes"].as_array().unwrap().len(), 2, "{v}");
    // 用上傳的（網頁拖檔）
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&before);
    let (code, v) = call("POST", &base, &format!("{p}/images"), Some(json!({"file_name": "拖進來.png", "data_b64": b64}))).await;
    assert_eq!((code, v["added"].as_u64()), (200, Some(1)), "{v}");

    let (code, v) = call("GET", &base, &p, None).await;
    assert_eq!(code, 200);
    let files: Vec<&str> = v["project"]["images"].as_array().unwrap().iter().map(|i| i["file"].as_str().unwrap()).collect();
    assert_eq!(files, ["0001.png", "0002.png", "0003.png", "0004.png", "0005.png"]);
    let r = reqwest::get(format!("{base}{p}/img/0001.png")).await.unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(r.headers()["content-type"], "image/png");
    for bad in ["..%2Fproject.json", "project.json", "9999.png"] {
        let code = reqwest::get(format!("{base}{p}/img/{bad}")).await.unwrap().status().as_u16();
        assert!(code == 400 || code == 404, "{bad} → {code}");
    }

    // 描述：寫一張 → 觸發詞自動在最前面；全部加一個詞；全部拿掉一個詞
    let (code, v) = call("POST", &base, &format!("{p}/captions"), Some(json!({"file": "0001.png", "text": "1girl, smile"}))).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["captions"]["0001.png"], format!("{trigger}, 1girl, smile"));
    let (_, v) = call("POST", &base, &format!("{p}/captions"), Some(json!({"add": "outdoors"}))).await;
    assert_eq!(v["captions"]["0001.png"], format!("{trigger}, 1girl, smile, outdoors"));
    assert_eq!(v["captions"]["0002.png"], format!("{trigger}, outdoors"));
    let (_, v) = call("POST", &base, &format!("{p}/captions"), Some(json!({"drop": ["smile"]}))).await;
    assert_eq!(v["captions"]["0001.png"], format!("{trigger}, 1girl, outdoors"));
    assert_eq!(std::fs::read_to_string(root.join("data/train").join(&id).join("captions/0001.txt")).unwrap(), format!("{trigger}, 1girl, outdoors"));
    let (code, v) = call("POST", &base, &format!("{p}/captions"), Some(json!({"file": "../x.png", "text": "x"}))).await;
    assert_eq!(code, 400, "{v}");
    let (code, v) = call("GET", &base, &format!("{p}/traits"), None).await;
    assert_eq!((code, v["traits"].clone()), (200, json!([])));

    // 參數：自動算；使用者改過的要留著
    let (code, v) = call("GET", &base, &format!("{p}/params?level=std"), None).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["params"]["steps"], 500);
    assert_eq!(v["samples"].as_array().unwrap().len(), 4);
    let (code, v) = call("POST", &base, &p, Some(json!({"name": "新名字", "level": "full", "params": {"steps": 700, "rank": 8}}))).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!((v["project"]["name"].as_str(), v["project"]["params"]["steps"].as_u64(), v["project"]["params"]["rank"].as_u64()), (Some("新名字"), Some(700), Some(8)));
    assert_eq!(v["project"]["params"]["epochs"], 5, "沒給的欄位用自動算的補上：{v}");
    // 改觸發詞：描述開頭跟著換
    let (code, v) = call("POST", &base, &p, Some(json!({"trigger": "newtrig"}))).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["captions"]["0001.png"], "newtrig, 1girl, outdoors");
    let (code, _) = call("POST", &base, &p, Some(json!({"trigger": "bad trigger"}))).await;
    assert_eq!(code, 400);

    // 勾掉、移除
    let (code, v) = call("POST", &base, &format!("{p}/images/set"), Some(json!({"files": ["0002.png"], "off": true}))).await;
    assert_eq!((code, v["report"]["count"].as_u64()), (200, Some(4)), "{v}");
    let (code, v) = call("POST", &base, &format!("{p}/images/remove"), Some(json!({"files": ["0003.png"]}))).await;
    assert_eq!((code, v["project"]["images"].as_array().unwrap().len()), (200, 4), "{v}");
    assert!(!root.join("data/train").join(&id).join("images/0003.png").exists());
    // 移除後再匯入不能撞號
    let (_, v) = call("POST", &base, &format!("{p}/images"), Some(json!({"paths": [src.join("b.png").to_string_lossy()]}))).await;
    assert_eq!(v["project"]["images"].as_array().unwrap().last().unwrap()["file"], "0006.png", "{v}");

    // 清單看得到；刪除要 confirm，刪掉是搬到 待刪除
    let (_, v) = call("GET", &base, "/api/train", None).await;
    assert_eq!((v["projects"][0]["id"].as_str(), v["projects"][0]["images"].as_u64()), (Some(id.as_str()), Some(4)));
    let (code, _) = call("DELETE", &base, &p, Some(json!({}))).await;
    assert_eq!(code, 400);
    let (code, v) = call("DELETE", &base, &p, Some(json!({"confirm": true}))).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(call("GET", &base, &p, None).await.0, 404);
    assert!(root.join("待刪除").join("訓練專案").read_dir().unwrap().count() == 1);
}

#[tokio::test]
async fn train_bad_ids_and_guards() {
    let (base, _root) = station("guards").await;
    for bad in ["..%2Fx", "t123", "T0123456789ab", "t0123456789ab"] {
        let (code, _) = call("GET", &base, &format!("/api/train/{bad}"), None).await;
        assert!(code == 400 || code == 404, "{bad} → {code}");
    }
    let (_, v) = call("POST", &base, "/api/train", Some(json!({"name": "角色", "kind": "character", "family": "anima", "trigger": "xkchr"}))).await;
    let id = v["project"]["id"].as_str().unwrap().to_string();
    // 沒有圖不能開始；花錢的地方沒 confirm 不能開始
    let (code, v) = call("POST", &base, &format!("/api/train/{id}/start"), Some(json!({"target": "local"}))).await;
    assert_eq!(code, 400, "{v}");
    assert!(v["error"].as_str().unwrap().contains("訓練圖"), "{v}");
    for target in ["runpod", "civitai", "modelscope"] {
        let (code, v) = call("POST", &base, &format!("/api/train/{id}/start"), Some(json!({"target": target}))).await;
        assert_eq!(code, 400, "{target} {v}");
        assert!(v["error"].as_str().unwrap().contains("確認"), "{target} {v}");
    }
    let (code, v) = call("POST", &base, "/api/train/trainer/remove", Some(json!({}))).await;
    assert_eq!(code, 400, "{v}");
    // 沒有描述也不能開始打標以外的東西：打標在沒有反推模型時要回人話
    let (code, v) = call("POST", &base, &format!("/api/train/{id}/caption"), Some(json!({"mode": "auto"}))).await;
    assert_eq!(code, 400, "{v}");
}
