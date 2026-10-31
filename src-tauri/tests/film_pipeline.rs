//! 影片製作流程的隔離路由驗收；不啟動 ComfyUI 或模型、不讀正式資料。
use anima_app::{director, h3chain, paths::Paths, server};
use axum::{body::{to_bytes, Body}, http::Request};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn station() -> (axum::Router, std::path::PathBuf) {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("待刪除").join(format!("film-api-{}", director::new_id()));
    std::fs::create_dir_all(&root).unwrap();
    let web = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("ui");
    let paths = Paths::new(&root, &web, Some(root.join("comfy")));
    let data = paths.data.clone();
    let app = server::start(paths, "http://127.0.0.1:9").await.unwrap();
    (server::router(app), data)
}
async fn post(r: &axum::Router, url: &str, body: Value) -> (u16, Value) {
    let response = r.clone().oneshot(Request::builder().method("POST").uri(url).header("host", "127.0.0.1:12345").header("content-type", "application/json").body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn shared_sheet_changes_protocol_without_changing_global_engine() {
    let (r, _) = station().await;
    let sheet = json!({"engine":"ltx23","cast":[{"name":"Ada"}],"segments":[{"secs":7,"beats":[{"from":1,"to":4,"action":"raises a cup"}],"exit":{"actors":[{"who":"S1","holding":"right hand holds cup"}]}},{"secs":9,"beats":[{"from":1,"to":4,"action":"turns toward window"}]}]});
    let (status, composed) = post(&r, "/api/shotsheet/compose", json!({"sheet":sheet})).await;
    assert_eq!(status, 200, "{composed}");
    let prompts: Vec<Value> = composed["segments"].as_array().unwrap().iter().map(|s| s["prompt"].clone()).collect();
    assert!(prompts[1].as_str().unwrap().contains("right hand holds cup"));
    // 2026-10-30 LTX 退場：舊拍攝單記的 ltx23 當 H3，寫 H3 的格式、套進導演台也記成 h3
    assert!(prompts[0].as_str().unwrap().contains("integrated_multimodal_description"));
    let (status, project) = post(&r, "/api/director/from_sheet", json!({"sheet":sheet,"engine":"ltx23","prompts":prompts})).await;
    assert_eq!(status, 200, "{project}");
    assert_eq!(project["settings"]["video_engine"], "h3");
    assert_eq!(project["segments"][0]["seconds"].as_f64(), Some(7.0));
    assert_eq!(project["segments"][1]["seconds"].as_f64(), Some(9.0));
    let response = r.clone().oneshot(Request::builder().uri("/api/video/workflows").header("host","127.0.0.1:12345").body(Body::empty()).unwrap()).await.unwrap();
    let v:Value=serde_json::from_slice(&to_bytes(response.into_body(),1024*1024).await.unwrap()).unwrap();
    // 全域影片模型沒被拍攝單改掉：還是預設（2026-10-30 LTX 退場後預設是 H3）
    assert_eq!(v["engine"], "h3");
    let (status, _) = post(&r, "/api/director/from_sheet", json!({"sheet":sheet,"engine":"missing-workflow","prompts":["x","y"]})).await;
    assert_eq!(status,400);
}

#[tokio::test]
async fn h3_take_selection_preserves_old_media_and_rejects_wrong_parent() {
    let (r, data) = station().await;
    let store = h3chain::Store::new(&data);
    let fixture = h3chain::normalize(&json!({"title":"isolated","size":[640,960],"segments":[{"secs":5,"text":"raises cup","status":"done","files":{"cut":"seg01_cut.mp4","end":"seg01_end.mp4","lat":"lat_00001.safetensors"}},{"secs":5,"text":"turns head","status":"todo"}]})).unwrap();
    let c = store.save(&fixture).unwrap();
    let id=c["id"].as_str().unwrap();
    let dir=store.seg_dir(id);
    for file in ["seg01_cut.mp4","seg01_end.mp4","lat_00001.safetensors"] {std::fs::write(dir.join(file),b"version-one").unwrap();}
    let mut c=c;
    anima_app::h3_takes::preserve(&mut c["segments"][0],&dir).unwrap();
    let take=c["segments"][0]["selected_take"].clone();
    c["segments"][1]["takes"]=json!([{"id":"old-child","source_take":"wrong-parent","result":{}}]);
    store.save(&c).unwrap();
    let id=c["id"].as_str().unwrap();
    let (status,v)=post(&r,"/api/h3/chain/take/select",json!({"id":id,"seg":1,"take_id":"old-child"})).await;
    assert_eq!(status,400,"{v}");
    let (status,v)=post(&r,"/api/h3/chain/take/select",json!({"id":id,"seg":0,"take_id":take})).await;
    assert_eq!(status,200,"{v}");
    assert_eq!(std::fs::read(dir.join("lat_00001.safetensors")).unwrap(),b"version-one");
    // Client saves cannot overwrite server-owned takes or selected media.
    let mut client=v.clone();
    client["segments"][0]["takes"]=json!([]);
    client["segments"][0]["files"]=json!({"end":"forged.mp4"});
    let (status,saved)=post(&r,"/api/h3/chain/save",json!({"chain":client})).await;
    assert_eq!(status,200,"{saved}");
    assert_eq!(saved["segments"][0]["takes"],v["segments"][0]["takes"]);
    assert_eq!(saved["segments"][0]["files"],v["segments"][0]["files"]);
}

#[tokio::test]
async fn review_route_rejects_missing_shot_without_touching_media() {
    let (r,_) = station().await;
    let (_,p)=post(&r,"/api/director/project/new",json!({"title":"review"})).await;
    let (status,_)=post(&r,"/api/director/tail/candidates",json!({"id":p["id"],"seg":0})).await;
    assert_eq!(status,400);
    let (status,_)=post(&r,"/api/director/tail/select",json!({"id":"../outside","seg":0,"video_id":1,"frame":0})).await;
    assert_eq!(status,400);
}
