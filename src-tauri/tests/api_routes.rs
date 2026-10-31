//! HTTP 路由的整合測試：真的開一個伺服器（資料放暫存資料夾、ComfyUI 指向沒人聽的 port），用 reqwest 打 API。
//! 不需要模型、ComfyUI、llama-server，任何一台電腦都能跑。
use serde_json::{json, Value};
use std::path::PathBuf;

/// 開一個測試站，回傳網址（http://127.0.0.1:<隨機 port>）。資料根目錄每個測試各一個，開始前先清掉上次留下的
async fn station(name: &str) -> String {
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join("anima_route_test").join(name));
    station_keep(name).await
}

/// 同 station，但不清資料根目錄（測試要先放好舊資料再開站時用）
async fn station_keep(name: &str) -> String {
    let root = std::env::temp_dir().join("anima_route_test").join(name);
    std::fs::create_dir_all(&root).unwrap();
    let web = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("ui");
    let paths = anima_app::paths::Paths::new(&root, &web, Some(root.join("comfy")));
    // port 9（discard）沒有 ComfyUI：伺服器會當成離線
    let app = anima_app::server::start(paths, "http://127.0.0.1:9")
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(anima_app::server::serve_on(
        anima_app::server::router(app),
        listener,
    ));
    format!("http://{addr}")
}

async fn post(base: &str, path: &str, body: Value) -> (u16, Value) {
    let r = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let code = r.status().as_u16();
    (code, r.json().await.unwrap_or(Value::Null))
}

fn txt2img() -> Value {
    json!({"mode": "txt2img", "model": "x.safetensors", "prompt": "1girl", "negative": "",
           "width": 1024, "height": 1024, "steps": 8, "cfg": 1, "sampler": "euler", "scheduler": "simple", "seed": 1})
}

#[tokio::test]
async fn state_works_without_comfy() {
    let base = station("state").await;
    let r = reqwest::get(format!("{base}/api/state")).await.unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["online"], false);
}

#[tokio::test]
async fn index_page_uses_versioned_static_paths() {
    let base = station("index").await;
    let html = reqwest::get(format!("{base}/"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(html.contains("<html"));
    assert!(!html.contains("\"/static/"), "頁面裡的 /static/ 要換成這次啟動的前綴");
    assert!(html.contains(anima_app::server::static_prefix()));
}

#[tokio::test]
async fn txt2img_missing_params_is_rejected() {
    let base = station("missing").await;
    let (code, v) = post(&base, "/api/generate", json!({"params": {"mode": "txt2img", "prompt": "1girl"}})).await;
    assert_eq!(code, 400);
    let msg = v["error"].as_str().unwrap();
    for k in ["width", "height", "steps", "cfg"] {
        assert!(msg.contains(k), "錯誤訊息要列出缺少的 {k}：{msg}");
    }
    // 沒寫 mode＝文生圖，一樣要擋
    let (code, _) = post(&base, "/api/generate", json!({"params": {"prompt": "1girl"}})).await;
    assert_eq!(code, 400);
}

#[tokio::test]
async fn count_out_of_range_is_rejected() {
    let base = station("count").await;
    for n in [0, 33, 99] {
        let (code, v) = post(&base, "/api/generate", json!({"params": txt2img(), "count": n})).await;
        assert_eq!(code, 400, "count {n}");
        assert!(v["error"].as_str().unwrap().contains("count"));
    }
}

#[tokio::test]
async fn complete_params_pass_validation() {
    // 參數齊全就過了檢查，卡在下一關「ComfyUI 沒有連線」
    let base = station("complete").await;
    let (code, v) = post(&base, "/api/generate", json!({"params": txt2img(), "count": 2})).await;
    assert_eq!(code, 400);
    assert!(v["error"].as_str().unwrap().contains("ComfyUI"), "{v}");
    // 其他模式不檢查尺寸、步數（例如去背只帶模型和輸入圖）
    let rembg = json!({"mode": "rembg", "model": "x.safetensors", "init_image": "a.png", "prompt": "", "negative": "", "seed": 0});
    let (_, v) = post(&base, "/api/generate", json!({"params": rembg})).await;
    assert!(v["error"].as_str().unwrap().contains("ComfyUI"), "{v}");
}

#[tokio::test]
async fn other_sites_are_refused() {
    let base = station("origin").await;
    let r = reqwest::Client::new()
        .post(format!("{base}/api/generate"))
        .header("Origin", "http://evil.invalid")
        .json(&json!({"params": txt2img()}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);
}

/// 影片模型：匯入自訂工作流 → 出現在清單 → 設成影片模型 → 刪掉後自動換回內建
#[tokio::test]
async fn video_workflow_import_select_delete() {
    let base = station("video_wf").await;
    let list = || async { reqwest::get(format!("{base}/api/video/workflows")).await.unwrap().json::<Value>().await.unwrap() };
    let v = list().await;
    // 2026-10-30 LTX 退場：內建影片是 H3
    assert_eq!(v["engine"], "h3");
    assert_eq!(v["workflows"], json!([]));

    // ComfyUI 一般存檔（有 nodes、links）要擋，並且講要用「匯出（API）」
    let (code, v) = post(&base, "/api/video/workflows", json!({"name": "壞的", "graph": {"nodes": [], "links": []}})).await;
    assert_eq!(code, 400);
    assert!(v["error"].as_str().unwrap().contains("匯出（API）"));

    let graph = json!({
        "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{prompt}}", "clip": ["38", 0]}},
        "38": {"class_type": "CLIPLoader", "inputs": {"clip_name": "umt5.safetensors", "type": "wan"}},
        "52": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
        "50": {"class_type": "WanImageToVideo", "inputs": {"positive": ["6", 0], "negative": ["6", 0], "start_image": ["52", 0],
               "width": 832, "height": 480, "length": 81, "batch_size": 1}},
        "3": {"class_type": "KSampler", "inputs": {"seed": 1, "positive": ["50", 0], "negative": ["50", 1], "latent_image": ["50", 2]}},
        "57": {"class_type": "CreateVideo", "inputs": {"images": ["3", 0], "fps": 16}},
        "58": {"class_type": "SaveVideo", "inputs": {"video": ["57", 0], "filename_prefix": "video/ComfyUI"}},
    });
    let (code, v) = post(&base, "/api/video/workflows", json!({"name": "Wan 2.2", "graph": graph})).await;
    assert_eq!(code, 200, "{v}");
    let id = v["workflow"]["id"].as_str().unwrap().to_string();
    assert_eq!(v["workflow"]["info"]["image"]["node"], "52");
    assert_eq!(v["workflow"]["info"]["seconds"], true);

    // 沒匯入過的不能選
    let (code, _) = post(&base, "/api/settings", json!({"video_engine": "沒有這個"})).await;
    assert_eq!(code, 400);
    let (code, _) = post(&base, "/api/settings", json!({"video_engine": id})).await;
    assert_eq!(code, 200);
    let v = list().await;
    assert_eq!(v["engine"], id.as_str());
    assert_eq!(v["workflows"][0]["name"], "Wan 2.2");
    let st: Value = reqwest::get(format!("{base}/api/state")).await.unwrap().json().await.unwrap();
    assert_eq!(st["settings"]["video_engine"], id.as_str());

    let r = reqwest::Client::new()
        .delete(format!("{base}/api/video/workflows/{}", urlencode(&id)))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let v = list().await;
    assert_eq!(v["engine"], "h3", "正在用的被刪掉：換回內建");
    assert_eq!(v["workflows"], json!([]));
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

#[tokio::test]
async fn h3_routes_without_comfy_or_cloud() {
    use base64::Engine;
    let base = station("h3").await;
    // 玩法牆：範本、預設模型都在；本機 ComfyUI 離線
    let v: Value = reqwest::get(format!("{base}/api/h3/info")).await.unwrap().json().await.unwrap();
    assert!(v["templates"].as_array().is_some_and(|t| t.len() >= 35), "{v}");
    assert_eq!(v["local"]["online"], false);
    assert!(v["defaults"]["model"].as_str().unwrap_or("").contains("10Eros"), "{v}");
    // 素材：上傳一張圖 → input 裡的 h3_<雜湊>.png，量得到大小
    let mut png = std::io::Cursor::new(vec![]);
    image::RgbImage::new(64, 48).write_to(&mut png, image::ImageFormat::Png).unwrap();
    let b64 = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
    let (code, m) = post(&base, "/api/h3/media", json!({"file_name": "a.png", "data_b64": b64})).await;
    assert_eq!(code, 200, "{m}");
    assert!(m["name"].as_str().unwrap_or("").starts_with("h3_") && m["w"] == 64 && m["h"] == 48, "{m}");
    // 送出：沒有 params、本機離線、雲端沒開、素材不見 → 都是 400 並說原因
    let (code, e) = post(&base, "/api/h3/generate", json!({})).await;
    assert_eq!(code, 400, "{e}");
    let p = json!({"h3_mode": "i2v", "prompt": "a cat walks", "seconds": 5, "first": m["name"]});
    let (code, e) = post(&base, "/api/h3/generate", json!({"params": p, "where": "local"})).await;
    assert!(code == 400 && e["error"].as_str().unwrap_or("").contains("沒有連線"), "{e}");
    let (code, e) = post(&base, "/api/h3/generate", json!({"params": p, "where": "cloud"})).await;
    assert!(code == 400 && e["error"].as_str().unwrap_or("").contains("雲端"), "{e}");
    let gone = json!({"h3_mode": "i2v", "prompt": "x", "first": "h3_000000000000.png"});
    let (code, e) = post(&base, "/api/h3/generate", json!({"params": gone, "where": "cloud"})).await;
    assert_eq!(code, 400, "{e}");
    // 素材名帶路徑：不收（不然雲端模式會把別的檔傳上去）
    let sneaky = json!({"h3_mode": "i2v", "prompt": "x", "first": "../../h3_x.png"});
    let (code, e) = post(&base, "/api/h3/generate", json!({"params": sneaky, "where": "cloud"})).await;
    assert!(code == 400 && e["error"].as_str().unwrap_or("").contains("素材名稱不對"), "{e}");
    // 停一個不存在的工作、空的想法
    let (code, _) = post(&base, "/api/h3/stop", json!({"task": "h3-0"})).await;
    assert_eq!(code, 400);
    let (code, _) = post(&base, "/api/h3/write", json!({"idea": " "})).await;
    assert_eq!(code, 400);
}

/// H3 長片：存、讀、清單、雲端沒開不能拍（不會偷開機）、檔名擋路徑、刪除是搬到待刪除
#[tokio::test]
async fn h3_chain_routes() {
    let base = station("h3chain").await;
    let chain = json!({"title": "測試", "look": "a girl", "segments": [{"text": "she waves", "secs": 30}, {"text": ""}]});
    let (code, c) = post(&base, "/api/h3/chain/save", json!({"chain": chain})).await;
    assert_eq!(code, 200, "{c}");
    let id = c["id"].as_str().unwrap().to_string();
    assert_eq!(c["segments"][0]["secs"], 15.0);
    assert_eq!(c["est"], 25.0 * 25.0, "15 秒＋10 秒，每秒 25 秒");
    // 網頁送來的「拍好了」不算：結果以存檔為準
    let mut fake = c.clone();
    fake["segments"][0]["status"] = json!("done");
    let (code, c2) = post(&base, "/api/h3/chain/save", json!({"chain": fake})).await;
    assert!(code == 200 && c2["segments"][0]["status"] == "todo", "{c2}");
    let got: Value = reqwest::get(format!("{base}/api/h3/chain/{id}")).await.unwrap().json().await.unwrap();
    assert_eq!(got["title"], "測試");
    let l: Value = reqwest::get(format!("{base}/api/h3/chains")).await.unwrap().json().await.unwrap();
    assert_eq!(l["chains"][0]["id"], id.as_str());
    // 雲端沒開：不能拍
    let (code, e) = post(&base, "/api/h3/chain/run", json!({"id": id})).await;
    assert!(code == 400 && e["error"].as_str().unwrap_or("").contains("雲端"), "{e}");
    let (code, _) = post(&base, "/api/h3/chain/stop", json!({"id": id})).await;
    assert_eq!(code, 400);
    // 檔名擋路徑
    let r = reqwest::get(format!("{base}/api/h3/chain/{id}/file/chain.json")).await.unwrap();
    assert_eq!(r.status().as_u16(), 404);
    let (code, bad) = post(&base, "/api/h3/chain/save", json!({"chain": {"ref": "../x.png"}})).await;
    assert_eq!(code, 400, "{bad}");
    let (code, _) = post(&base, "/api/h3/chain/delete", json!({"id": id})).await;
    assert_eq!(code, 200);
    let r = reqwest::get(format!("{base}/api/h3/chain/{id}")).await.unwrap();
    assert_eq!(r.status().as_u16(), 404);
    let trash = std::env::temp_dir().join("anima_route_test").join("h3chain").join("待刪除").join("H3長片");
    assert_eq!(std::fs::read_dir(trash).unwrap().count(), 1, "刪除＝搬到待刪除");
}

#[tokio::test]
async fn h3_video_engine_setting() {
    let base = station("h3eng").await;
    // 設定 → 影片模型選「MiniMax H3」
    let (code, e) = post(&base, "/api/settings", json!({"video_engine": "h3"})).await;
    assert_eq!(code, 200, "{e}");
    let v: Value = reqwest::get(format!("{base}/api/video/workflows")).await.unwrap().json().await.unwrap();
    assert_eq!(v["engine"], "h3");
    // 2026-10-30 LTX 退場：設定選 H3（內建）時，生成頁的影片也走 H3（測試站沒有 ComfyUI → 講沒連線）
    let p = json!({"mode": "video", "prompt": "x", "init_image": "a.png", "model": "", "seed": 1});
    let (code, e) = post(&base, "/api/generate", json!({"count": 1, "params": p})).await;
    assert!(code == 400 && e["error"].as_str().unwrap_or("").contains("ComfyUI 沒有連線"), "{e}");
    // 舊網頁送來 ltx23：一樣當 H3
    let p = json!({"mode": "video", "prompt": "x", "init_image": "a.png", "model": "", "seed": 1, "video_engine": "ltx23"});
    let (code, e) = post(&base, "/api/generate", json!({"count": 1, "params": p})).await;
    assert!(code == 400 && e["error"].as_str().unwrap_or("").contains("ComfyUI 沒有連線"), "{e}");
    // 「動起來」明確選 H3 本機：走 H3（測試站沒有 ComfyUI → 講沒連線，不是叫他去 H3 影片台）
    let p = json!({"mode": "video", "prompt": "x", "init_image": "a.png", "model": "", "seed": 1, "video_engine": "h3", "h3_model": "a.gguf"});
    let (code, e) = post(&base, "/api/generate", json!({"count": 1, "params": p})).await;
    assert!(code == 400 && e["error"].as_str().unwrap_or("").contains("ComfyUI 沒有連線"), "{e}");
}

/// 測試站的資料根目錄（station() 用的那個）
fn route_root(name: &str) -> PathBuf {
    std::env::temp_dir().join("anima_route_test").join(name)
}

async fn new_asset(base: &str, kind: &str, name: &str) -> i64 {
    let (code, v) = post(base, "/api/lib", json!({"kind": kind, "name": name, "data": {}})).await;
    assert_eq!(code, 200, "{v}");
    v["id"].as_i64().unwrap()
}

#[tokio::test]
async fn asset_routes() {
    let base = station("assets").await;
    let root = route_root("assets");
    std::fs::create_dir_all(root.join("comfy").join("input")).unwrap();
    image::RgbImage::new(4, 3).save(root.join("comfy").join("input").join("a.png")).unwrap();
    let id = new_asset(&base, "char", "小克").await;

    let (code, v) = post(&base, &format!("/api/assets/{id}/file"), json!({"input": "a.png", "role": "src"})).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["data"]["images"].as_array().unwrap().len(), 1);
    assert_eq!(v["data"]["images"][0]["role"], "src");
    assert_eq!(v["has_thumb"], 1, "第一張圖自動當縮圖");
    let first = v["data"]["images"][0]["file"].as_str().unwrap().to_string();
    assert_eq!(first, format!("{id}_1.png"));
    assert!(root.join("data").join("assets").join(&first).exists());

    // 路徑只取檔名
    let (code, v) = post(&base, &format!("/api/assets/{id}/file"), json!({"input": "..\\a.png", "role": "sheet"})).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["data"]["images"][1], json!({"file": format!("{id}_2.png"), "role": "sheet"}));
    let (code, _) = post(&base, &format!("/api/assets/{id}/file"), json!({"input": "nope.png", "role": "src"})).await;
    assert_eq!(code, 404);
    // 不是圖片、不是聲音的副檔名不收
    std::fs::write(root.join("comfy").join("input").join("x.txt"), "hi").unwrap();
    let (code, _) = post(&base, &format!("/api/assets/{id}/file"), json!({"input": "x.txt", "role": "src"})).await;
    assert_eq!(code, 400);
    // 沒有這筆資產
    let (code, _) = post(&base, "/api/assets/999999/file", json!({"input": "a.png", "role": "src"})).await;
    assert_eq!(code, 404);

    let r = reqwest::get(format!("{base}/api/assets/file/{first}")).await.unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(r.headers()["content-type"], "image/png");

    let (code, v) = post(&base, &format!("/api/assets/{id}/to_input"), json!({"file": first})).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["width"], 4);
    assert_eq!(v["name"], format!("aw_asset_{first}"));
    assert!(root.join("comfy").join("input").join(format!("aw_asset_{first}")).exists());

    let (code, v) = post(&base, &format!("/api/assets/{id}/file/remove"), json!({"file": first})).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["data"]["images"].as_array().unwrap().len(), 1);
    assert!(!root.join("data").join("assets").join(&first).exists());
    let trash = root.join("待刪除").join("資產庫");
    let moved = |name: &str| {
        std::fs::read_dir(&trash)
            .into_iter()
            .flatten()
            .flatten()
            .any(|d| d.path().join(name).exists())
    };
    assert!(moved(&first), "移除的檔案搬到 待刪除\\資產庫\\");

    let (code, v) = post(&base, &format!("/api/assets/{id}/apply"), json!({"family": "qwen21"})).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["images"], json!([format!("{id}_2.png")]));
    let all: Value = reqwest::get(format!("{base}/api/lib")).await.unwrap().json().await.unwrap();
    let it = all.as_array().unwrap().iter().find(|x| x["id"] == id).unwrap().clone();
    assert_eq!(it["uses"], 1, "拿去用記一次");

    let r = reqwest::Client::new().delete(format!("{base}/api/lib/{id}")).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert!(moved("資產.json"));
    assert!(moved(&format!("{id}_2.png")));
    let all: Value = reqwest::get(format!("{base}/api/lib")).await.unwrap().json().await.unwrap();
    assert!(all.as_array().unwrap().iter().all(|x| x["id"] != id));
}

#[tokio::test]
async fn asset_file_copy_failure_leaves_data() {
    let base = station("assets_fail").await;
    let root = route_root("assets_fail");
    std::fs::create_dir_all(root.join("comfy").join("input")).unwrap();
    image::RgbImage::new(4, 3).save(root.join("comfy").join("input").join("a.png")).unwrap();
    let id = new_asset(&base, "scene", "教室").await;
    let (_, before) = post(&base, &format!("/api/assets/{id}/file"), json!({"input": "a.png", "role": "src"})).await;
    let (code, _) = post(&base, &format!("/api/assets/{id}/file"), json!({"input": "gone.png", "role": "src"})).await;
    assert_eq!(code, 404);
    let (code, _) = post(&base, &format!("/api/assets/{id}/file"), json!({"gallery_id": 424242, "role": "src"})).await;
    assert_eq!(code, 404);
    let all: Value = reqwest::get(format!("{base}/api/lib")).await.unwrap().json().await.unwrap();
    let it = all.as_array().unwrap().iter().find(|x| x["id"] == id).unwrap().clone();
    assert_eq!(it["data"], before["data"], "複製失敗：data 不能多出不存在的檔名");
    assert_eq!(std::fs::read_dir(root.join("data").join("assets")).unwrap().count(), 1);
}

#[tokio::test]
async fn asset_file_rejects_traversal() {
    let base = station("assets_trav").await;
    let root = route_root("assets_trav");
    std::fs::create_dir_all(root.join("data").join("assets")).unwrap();
    for p in ["..%5C..%5Cconfig.json", "..%5Csettings.json", "%2E%2E"] {
        let r = reqwest::get(format!("{base}/api/assets/file/{p}")).await.unwrap();
        assert_eq!(r.status().as_u16(), 404, "{p}");
    }
    let id = new_asset(&base, "prop", "椅子").await;
    let (code, _) = post(&base, &format!("/api/assets/{id}/to_input"), json!({"file": "..\\..\\data\\settings.json"})).await;
    assert_eq!(code, 404);
    let (code, _) = post(&base, &format!("/api/assets/{id}/file/remove"), json!({"file": "..\\settings.json"})).await;
    assert_eq!(code, 404);
}

async fn lib_all(base: &str) -> Vec<Value> {
    reqwest::get(format!("{base}/api/lib")).await.unwrap().json::<Value>().await.unwrap().as_array().unwrap().clone()
}

#[tokio::test]
async fn director_chars_migrate_once() {
    // 舊資料要在開站前就放好（搬家在第一次讀角色時做）
    let root = route_root("dir_chars_mig");
    let _ = std::fs::remove_dir_all(&root);
    let ddir = root.join("data").join("director");
    std::fs::create_dir_all(ddir.join("chars")).unwrap();
    let legacy = r#"[{"id":"c1","name":"小克","desc":"orange hair","desc_zh":"橘髮","src":"c1_src.png","updated":1}]"#;
    std::fs::write(ddir.join("characters.json"), legacy).unwrap();
    image::RgbImage::new(4, 3).save(ddir.join("chars").join("c1_src.png")).unwrap();
    let base = station_keep("dir_chars_mig").await;

    let v: Value = reqwest::get(format!("{base}/api/director/chars")).await.unwrap().json().await.unwrap();
    let c = &v["chars"][0];
    assert_eq!(c["id"], "c1", "舊專案記的角色 id 不能變");
    assert_eq!(c["name"], "小克");
    assert_eq!(c["desc"], "orange hair");
    assert_eq!(c["desc_zh"], "橘髮");
    assert!(c["src_url"].as_str().unwrap().starts_with("/api/assets/file/"), "{c}");
    assert!(c["src_in"].as_str().unwrap().starts_with("adir_"), "{c}");
    let _: Value = reqwest::get(format!("{base}/api/director/chars")).await.unwrap().json().await.unwrap();
    let chars: Vec<Value> = lib_all(&base).await.into_iter().filter(|x| x["kind"] == "char").collect();
    assert_eq!(chars.len(), 1, "讀兩次不能搬兩次");
    assert_eq!(chars[0]["data"]["images"][0]["role"], "src");
    assert_eq!(chars[0]["has_thumb"], 1);
    assert!(ddir.join("characters.migrated").exists());
    assert_eq!(std::fs::read_to_string(ddir.join("characters.json")).unwrap(), legacy, "舊檔只讀不改");
    assert!(ddir.join("chars").join("c1_src.png").exists(), "舊圖不動");
    // 舊網址也還能用
    let r = reqwest::get(format!("{base}/api/director/file/c1_src.png")).await.unwrap();
    assert_eq!(r.status().as_u16(), 200);
}

#[tokio::test]
async fn director_chars_migrate_skips_existing_cid() {
    // 第一次搬到一半當掉（資產已經建了、標記檔還沒寫）：再開不能重複建
    let root = route_root("dir_chars_half");
    let _ = std::fs::remove_dir_all(&root);
    let ddir = root.join("data").join("director");
    std::fs::create_dir_all(ddir.join("chars")).unwrap();
    // 開站時還沒有舊檔（開站的背景搬家什麼都不做），先建好 c1 的資產，再放舊檔：等於上次搬到 c1 就當掉
    let base = station_keep("dir_chars_half").await;
    let (code, _) = post(&base, "/api/lib", json!({"kind": "char", "name": "小克", "data": {"cid": "c1"}})).await;
    assert_eq!(code, 200);
    std::fs::write(ddir.join("characters.json"), r#"[{"id":"c1","name":"小克","desc":"a"},{"id":"c2","name":"阿藍","desc":"b"}]"#).unwrap();
    let v: Value = reqwest::get(format!("{base}/api/director/chars")).await.unwrap().json().await.unwrap();
    assert_eq!(v["chars"].as_array().unwrap().len(), 2);
    let chars: Vec<Value> = lib_all(&base).await.into_iter().filter(|x| x["kind"] == "char").collect();
    assert_eq!(chars.len(), 2);
}

#[tokio::test]
async fn director_chars_save_creates_asset() {
    let base = station("dir_chars_save").await;
    let root = route_root("dir_chars_save");
    let (code, v) = post(&base, "/api/director/chars", json!({"name": "阿綠", "desc": "green"})).await;
    assert_eq!(code, 200, "{v}");
    let cid = v["id"].as_str().unwrap().to_string();
    let all = lib_all(&base).await;
    let a = all.iter().find(|x| x["kind"] == "char" && x["name"] == "阿綠").expect("要有一筆資產");
    assert_eq!(a["data"]["look_en"], "green");
    assert_eq!(a["data"]["cid"], cid.as_str());
    // 名字不能重複
    let (code, _) = post(&base, "/api/director/chars", json!({"name": "阿綠", "desc": "x"})).await;
    assert_eq!(code, 400);
    // 換圖：src 只留一張，舊的搬去待刪除
    std::fs::create_dir_all(root.join("comfy").join("input")).unwrap();
    image::RgbImage::new(4, 3).save(root.join("comfy").join("input").join("a.png")).unwrap();
    image::RgbImage::new(6, 3).save(root.join("comfy").join("input").join("b.png")).unwrap();
    let (code, v) = post(&base, "/api/director/chars", json!({"id": cid, "name": "阿綠", "src": "a.png"})).await;
    assert_eq!(code, 200, "{v}");
    let (code, _) = post(&base, "/api/director/chars", json!({"id": cid, "name": "阿綠", "src": "b.png", "sheet": "a.png"})).await;
    assert_eq!(code, 200);
    let v: Value = reqwest::get(format!("{base}/api/director/chars")).await.unwrap().json().await.unwrap();
    let c = v["chars"].as_array().unwrap().iter().find(|c| c["id"] == cid.as_str()).unwrap().clone();
    assert_eq!(c["desc"], "green", "沒送 desc 就不動");
    assert!(c["sheet_url"].as_str().is_some());
    let a = lib_all(&base).await.into_iter().find(|x| x["name"] == "阿綠").unwrap();
    let srcs: Vec<&Value> = a["data"]["images"].as_array().unwrap().iter().filter(|im| im["role"] == "src").collect();
    assert_eq!(srcs.len(), 1, "{a}");
    assert_eq!(c["src"], srcs[0]["file"]);
    // 拿掉設定圖
    let (code, _) = post(&base, "/api/director/chars", json!({"id": cid, "name": "阿綠", "clear_sheet": true})).await;
    assert_eq!(code, 200);
    let v: Value = reqwest::get(format!("{base}/api/director/chars")).await.unwrap().json().await.unwrap();
    assert!(v["chars"][0]["sheet_url"].is_null() || v["chars"][0].get("sheet_url").is_none());
    // 刪除：整筆搬到待刪除\資產庫\
    let (code, _) = post(&base, "/api/director/chars/delete", json!({"id": cid})).await;
    assert_eq!(code, 200);
    assert!(lib_all(&base).await.iter().all(|x| x["name"] != "阿綠"));
    assert!(root.join("待刪除").join("資產庫").exists());
}

#[tokio::test]
async fn asset_save_keeps_server_file_list() {
    let base = station("assets_keep").await;
    let root = route_root("assets_keep");
    std::fs::create_dir_all(root.join("comfy").join("input")).unwrap();
    image::RgbImage::new(4, 3).save(root.join("comfy").join("input").join("a.png")).unwrap();
    let a = new_asset(&base, "char", "甲").await;
    let b = new_asset(&base, "prop", "乙").await;
    let (_, va) = post(&base, &format!("/api/assets/{a}/file"), json!({"input": "a.png", "role": "src"})).await;
    let (_, vb) = post(&base, &format!("/api/assets/{b}/file"), json!({"input": "a.png", "role": "src"})).await;
    let b_file = vb["data"]["images"][0]["file"].as_str().unwrap().to_string();
    // 另一個視窗又加了一張；這邊拿舊的 data 存檔（改了用途、還想偷塞乙的檔）
    post(&base, &format!("/api/assets/{a}/file"), json!({"input": "a.png", "role": "detail"})).await;
    let mut stale = va["data"].clone();
    stale["images"][0]["role"] = json!("sheet");
    stale["images"].as_array_mut().unwrap().push(json!({"file": b_file, "role": "other"}));
    stale["look_en"] = json!("x");
    let (code, v) = post(&base, "/api/lib", json!({"id": a, "data": stale})).await;
    assert_eq!(code, 200, "{v}");
    let imgs = v["data"]["images"].as_array().unwrap();
    assert_eq!(imgs.len(), 2, "伺服器上的兩張都在、乙的檔塞不進來：{v}");
    assert_eq!(imgs[0]["role"], "sheet", "用途可以改");
    assert_eq!(imgs[1]["role"], "detail");
    assert_eq!(v["data"]["look_en"], "x");
    // 新增時不能直接帶檔名
    let (_, v) = post(&base, "/api/lib", json!({"kind": "scene", "name": "丙", "data": {"images": [{"file": b_file, "role": "src"}]}})).await;
    assert!(v["data"].get("images").is_none(), "{v}");
    // 資產不能改成整套設定或片段（檔案會沒人管）
    let (code, _) = post(&base, "/api/lib", json!({"id": a, "kind": "snippet", "data": {"text": "x"}})).await;
    assert_eq!(code, 400);
}

#[tokio::test]
async fn asset_kind_conversion_cannot_claim_another_asset_file() {
    let base = station("asset_convert_safe").await;
    let root = route_root("asset_convert_safe");
    image::RgbImage::new(4, 3).save(root.join("comfy/input/a.png")).unwrap();
    let a = new_asset(&base, "prop", "owner").await;
    let (_, owner) = post(&base, &format!("/api/assets/{a}/file"), json!({"input":"a.png", "role":"src"})).await;
    let file = owner["data"]["images"][0]["file"].as_str().unwrap();
    for kind in ["prop", "shot", "char"] {
        let (_, set) = post(&base, "/api/lib", json!({"kind":"set", "name":kind,
            "data":{"images":[{"file":file,"role":"src"}], "audio":{"file":file}}})).await;
        let id = set["id"].as_i64().unwrap();
        let (code, converted) = post(&base, "/api/lib", json!({"id":id,"kind":kind})).await;
        assert_eq!(code, 200, "{converted}");
        assert!(converted["data"].get("images").is_none());
        assert!(converted["data"].get("audio").is_none());
        let res = reqwest::Client::new().delete(format!("{base}/api/lib/{id}")).send().await.unwrap();
        assert_eq!(res.status().as_u16(), 200);
        assert!(root.join("data/assets").join(file).is_file(), "刪轉換的資產不能搬走別筆的檔");
    }
}

#[tokio::test]
async fn asset_partial_updates_keep_character_identity() {
    let base = station("asset_cid_safe").await;
    let id = new_asset(&base, "char", "identity").await;
    let cid = lib_all(&base).await[0]["data"]["cid"].clone();
    for data in [json!({"look_en":"new look"}), json!({"cid":"forged","look_en":"again"})] {
        let (code, row) = post(&base, "/api/lib", json!({"id":id,"data":data})).await;
        assert_eq!(code, 200);
        assert_eq!(row["data"]["cid"], cid);
    }
    // 舊匯入資料沒 cid 時，導演台已經用 lib<id>，第一次更新也不能換號。
    let con = rusqlite::Connection::open(route_root("asset_cid_safe").join("data/anima.db")).unwrap();
    con.execute("UPDATE prompt_lib SET data='{}' WHERE id=?", [id]).unwrap();
    let (_, row) = post(&base, "/api/lib", json!({"id":id,"fav":true})).await;
    assert_eq!(row["data"]["cid"], format!("lib{id}"));
}

#[tokio::test]
async fn asset_thumbnail_db_failure_rolls_back_data_and_copy() {
    let base = station("asset_thumb_fail").await;
    let root = route_root("asset_thumb_fail");
    image::RgbImage::new(4, 3).save(root.join("comfy/input/a.png")).unwrap();
    let id = new_asset(&base, "prop", "db failure").await;
    let con = rusqlite::Connection::open(root.join("data/anima.db")).unwrap();
    con.execute_batch("CREATE TRIGGER fail_thumb BEFORE UPDATE OF thumb ON prompt_lib BEGIN SELECT RAISE(ABORT, 'test thumbnail failure'); END;").unwrap();
    let (code, _) = post(&base, &format!("/api/assets/{id}/file"), json!({"input":"a.png","role":"src"})).await;
    assert_eq!(code, 500);
    assert!(lib_all(&base).await[0]["data"].get("images").is_none());
    assert!(!root.join(format!("data/assets/{id}_1.png")).exists());
}

#[tokio::test]
async fn asset_trash_and_db_failures_preserve_original_files() {
    let base = station("asset_trash_fail").await;
    let root = route_root("asset_trash_fail");
    image::RgbImage::new(4, 3).save(root.join("comfy/input/a.png")).unwrap();
    image::RgbImage::new(6, 3).save(root.join("comfy/input/b.png")).unwrap();
    std::fs::write(root.join("comfy/input/a.wav"), b"test audio").unwrap();
    let id = new_asset(&base, "char", "move failure").await;
    post(&base, &format!("/api/assets/{id}/file"), json!({"input":"a.png","role":"src"})).await;
    post(&base, &format!("/api/assets/{id}/file"), json!({"input":"a.wav","role":"audio"})).await;
    let before = lib_all(&base).await[0].clone();
    let file = before["data"]["images"][0]["file"].as_str().unwrap();
    // 待刪除的父路徑是檔案，模擬不可建立目的地。
    std::fs::write(root.join("待刪除"), "blocked").unwrap();
    for (url, body) in [
        (format!("/api/assets/{id}/file/remove"), json!({"file":file})),
        (format!("/api/assets/{id}/file"), json!({"input":"a.wav","role":"audio"})),
        ("/api/director/chars".into(), json!({"id":before["data"]["cid"],"name":"move failure","src":"b.png"})),
    ] {
        let (code, _) = post(&base, &url, body).await;
        assert_eq!(code, 500, "{url}");
        assert_eq!(lib_all(&base).await[0]["data"], before["data"]);
        assert_eq!(std::fs::read_dir(root.join("data/assets")).unwrap().count(), 2);
        assert!(root.join("data/assets").join(file).is_file());
    }
    std::fs::rename(root.join("待刪除"), root.join("trash.blocker")).unwrap();
    let con = rusqlite::Connection::open(root.join("data/anima.db")).unwrap();
    con.execute_batch("CREATE TRIGGER fail_data BEFORE UPDATE OF data ON prompt_lib BEGIN SELECT RAISE(ABORT, 'test data failure'); END;").unwrap();
    let (code, _) = post(&base, &format!("/api/assets/{id}/file/remove"), json!({"file":file})).await;
    assert_eq!(code, 500);
    assert!(root.join("data/assets").join(file).is_file(), "DB 失敗必須把已搬走的檔還原");
    assert_eq!(lib_all(&base).await[0]["data"], before["data"]);
    let (code, _) = post(&base, &format!("/api/assets/{id}/file"), json!({"input":"a.wav","role":"audio"})).await;
    assert_eq!(code, 500);
    assert_eq!(lib_all(&base).await[0]["data"], before["data"]);
    assert_eq!(std::fs::read_dir(root.join("data/assets")).unwrap().count(), 2);
}

#[tokio::test]
async fn director_chars_migration_retries_copy_and_parse_failures() {
    let base = station("asset_migrate_retry").await;
    let root = route_root("asset_migrate_retry");
    let dir = root.join("data/director");
    std::fs::create_dir_all(dir.join("chars")).unwrap();
    let legacy = r#"[{"id":"legacy1","name":"legacy","src":"old.png"}]"#;
    // 無效 JSON 不能被當作「空的舊角色清單」。
    std::fs::write(dir.join("characters.json"), "{").unwrap();
    let res = reqwest::get(format!("{base}/api/director/chars")).await.unwrap();
    assert_eq!(res.status().as_u16(), 500);
    assert!(!dir.join("characters.migrated").exists());
    std::fs::write(dir.join("characters.json"), legacy).unwrap();
    image::RgbImage::new(4, 3).save(dir.join("chars/old.png")).unwrap();
    std::fs::write(root.join("data/assets"), "blocked").unwrap();
    let res = reqwest::get(format!("{base}/api/director/chars")).await.unwrap();
    assert_eq!(res.status().as_u16(), 500);
    assert!(!dir.join("characters.migrated").exists());
    assert_eq!(lib_all(&base).await.len(), 1);
    std::fs::rename(root.join("data/assets"), root.join("data/assets.blocker")).unwrap();
    let res = reqwest::get(format!("{base}/api/director/chars")).await.unwrap();
    assert_eq!(res.status().as_u16(), 200);
    let rows = lib_all(&base).await;
    assert_eq!(rows.len(), 1, "重試補圖不能重複建立角色");
    assert_eq!(rows[0]["data"]["cid"], "legacy1");
    assert_eq!(rows[0]["data"]["images"].as_array().unwrap().len(), 1);
    assert!(dir.join("characters.migrated").exists());
    assert_eq!(std::fs::read_to_string(dir.join("characters.json")).unwrap(), legacy);
    assert!(dir.join("chars/old.png").is_file());
}

#[tokio::test]
async fn director_chars_migration_skips_missing_legacy_image() {
    // 舊清單記著的圖早就被使用者刪掉了：永遠補不回來，不能卡住搬家（否則導演台角色永遠 500）
    let root = route_root("asset_migrate_missing");
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("data").join("director");
    std::fs::create_dir_all(dir.join("chars")).unwrap();
    std::fs::write(dir.join("characters.json"), r#"[{"id":"m1","name":"沒圖的","desc":"x","src":"gone.png","sheet":"gone_sheet.png"}]"#).unwrap();
    let base = station_keep("asset_migrate_missing").await;
    let r = reqwest::get(format!("{base}/api/director/chars")).await.unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["chars"][0]["id"], "m1");
    assert!(v["chars"][0].get("src_url").is_none());
    assert!(dir.join("characters.migrated").exists());
    assert_eq!(lib_all(&base).await.len(), 1);
}

#[tokio::test]
async fn shotsheet_routes() {
    let base = station("shotsheet").await;
    let meta: Value = reqwest::get(format!("{base}/api/shotsheet/meta")).await.unwrap().json().await.unwrap();
    assert_eq!(meta["cheat"]["move"].as_array().unwrap().len(), 7, "{meta}");
    assert_eq!(meta["limits"]["tail"], 1.6);
    assert_eq!(meta["limits"]["max_segs"], 30);
    let sheet = json!({"first": {"file": "f.png"}, "segments": [
        {"beats": [{"from": 1, "to": 4, "action": "她右手揮一揮"}]}, {"beats": []}]});
    let (code, c) = post(&base, "/api/shotsheet/compose", json!({"sheet": sheet})).await;
    assert_eq!(code, 200, "{c}");
    let segs = c["segments"].as_array().unwrap();
    assert_eq!(segs.len(), 2);
    let p0 = segs[0]["prompt"].as_str().unwrap();
    assert!(p0.lines().next().unwrap().contains("<Picture 1>"), "{p0}");
    let m0 = segs[0]["mirror"].as_str().unwrap();
    assert!(m0.contains("integrated_multimodal_description: ") && m0.contains("From "), "{m0}");
    assert_eq!(c["sheet"]["lang"], "zh");
    assert_eq!(c["estimate"][0]["gpu"], "L40S");
    let (code, k) = post(&base, "/api/shotsheet/check", json!({"sheet": sheet, "prompts": ["a dreamy girl", ""]})).await;
    assert_eq!(code, 200, "{k}");
    assert!(k["issues"].as_array().unwrap().iter().any(|x| x["rule"] == 1), "{k}");
    let (code, d) = post(&base, "/api/shotsheet/danger", json!({"words": ["blood", " blood ", ""]})).await;
    assert_eq!(code, 200, "{d}");
    assert_eq!(d["words"], json!(["blood"]));
    let meta: Value = reqwest::get(format!("{base}/api/shotsheet/meta")).await.unwrap().json().await.unwrap();
    assert_eq!(meta["rules"]["danger_user"], json!(["blood"]));
    let (_, k) = post(&base, "/api/shotsheet/check", json!({"sheet": sheet, "prompts": ["blood", ""]})).await;
    assert!(k["issues"].as_array().unwrap().iter().any(|x| x["rule"] == 3), "{k}");
    let (code, t) = post(&base, "/api/shotsheet/template", json!({"name": "揮手範本", "sheet": sheet})).await;
    assert_eq!(code, 200, "{t}");
    let shot: Vec<Value> = lib_all(&base).await.into_iter().filter(|x| x["kind"] == "shot").collect();
    assert_eq!(shot.len(), 1);
    assert!(shot[0]["data"]["first"].get("file").is_none(), "{}", shot[0]);
    assert_eq!(shot[0]["data"]["segments"][0]["beats"][0]["action"], "她右手揮一揮");
    let (code, e) = post(&base, "/api/shotsheet/compose", json!({"sheet": {"first": {"file": "..\\x.png"}}})).await;
    assert_eq!(code, 400, "{e}");
    let (code, _) = post(&base, "/api/shotsheet/template", json!({"name": "", "sheet": sheet})).await;
    assert_eq!(code, 400);
}

#[tokio::test]
async fn shotsheet_danger_rejects_invalid_and_serializes_writes() {
    let base = station("shotdanger_review").await;
    post(&base,"/api/shotsheet/danger",json!({"words":["keep"]})).await;
    for body in [json!({}),json!({"words":"bad"}),json!({"words":[42]})] {
        let (code,_) = post(&base,"/api/shotsheet/danger",body).await;
        assert_eq!(code,400);
    }
    let meta: Value = reqwest::get(format!("{base}/api/shotsheet/meta")).await.unwrap().json().await.unwrap();
    assert_eq!(meta["rules"]["danger_user"],json!(["keep"]));
    let mut jobs=tokio::task::JoinSet::new();
    for i in 0..40 { let base=base.clone();jobs.spawn(async move {post(&base,"/api/shotsheet/danger",json!({"words":[format!("word{i}")]})).await.0}); }
    while let Some(result)=jobs.join_next().await {assert_eq!(result.unwrap(),200);}
}

#[tokio::test]
async fn charsheet_routes() {
    let base = station("charsheet").await;
    let root = route_root("charsheet");
    let (code, v) = post(&base, "/api/charsheet/prompt", json!({"kind": "turnaround", "desc": "orange hair", "family": "anima"})).await;
    assert_eq!(code, 200, "{v}");
    assert!(v["prompt"].as_str().unwrap().contains("orange hair"));
    assert_eq!((v["width"].as_u64(), v["height"].as_u64()), (Some(1664), Some(928)));
    let (code, v) = post(&base, "/api/charsheet/prompt", json!({"kind": "face", "family": "qwen21"})).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["width"], 1344);
    let (code, v) = post(&base, "/api/charsheet/prompt", json!({"kind": "pose", "family": "anima"})).await;
    assert_eq!(code, 200, "{v}");
    assert!(v["prompt"].as_str().unwrap().contains("full body"));
    let (code, _) = post(&base, "/api/charsheet/prompt", json!({"kind": "nope"})).await;
    assert_eq!(code, 400);
    // 反推模型沒下載：400，不是 404
    let (code, _) = post(&base, "/api/charsheet/tags", json!({"name": "a.png"})).await;
    assert_eq!(code, 400);

    // 同用途換圖：replace＝true 時 sheet 只會有一張，舊的搬待刪除
    std::fs::create_dir_all(root.join("comfy").join("input")).unwrap();
    image::RgbImage::new(4, 3).save(root.join("comfy").join("input").join("a.png")).unwrap();
    image::RgbImage::new(5, 3).save(root.join("comfy").join("input").join("b.png")).unwrap();
    let id = new_asset(&base, "char", "工坊").await;
    post(&base, &format!("/api/assets/{id}/file"), json!({"input": "a.png", "role": "sheet"})).await;
    let (code, v) = post(&base, &format!("/api/assets/{id}/file"), json!({"input": "b.png", "role": "sheet", "replace": true})).await;
    assert_eq!(code, 200, "{v}");
    let imgs = v["data"]["images"].as_array().unwrap();
    assert_eq!(imgs.len(), 1, "{v}");
    assert_eq!(imgs[0]["file"], format!("{id}_2.png"));
    // 新用途 face 收得進去（不會被改成 other）
    let (_, v) = post(&base, &format!("/api/assets/{id}/file"), json!({"input": "a.png", "role": "face"})).await;
    assert_eq!(v["data"]["images"][1]["role"], "face", "{v}");
}
