//! 與 ComfyUI 的連線：送出任務、聽 websocket 進度與即時預覽、把完成的圖存進圖庫（對照舊版 comfy.py）。
//!
//! 所有任務都用同一個 client_id 送出，所以伺服器自己收得到進度與預覽，網頁關掉也不影響；圖完成時一定會進圖庫。
use crate::db::Db;
use crate::hub::Hub;
use crate::py::dumps;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

/// ComfyUI 的事件只送給「最後一個用這個 client_id 連上的」：正式站（8190）用 anima-web，
/// 其他 port 的測試站加上 port，借用同一個 ComfyUI 時才不會把正式站的完成通知搶走（2026-10-28 測試站關掉後，正式站整個收不到圖）
pub fn client_id_for(port: Option<&str>) -> String {
    match port.map(str::trim).filter(|p| !p.is_empty() && *p != "8190") {
        Some(p) => format!("anima-web-{p}"),
        None => "anima-web".into(),
    }
}

static CLIENT_ID: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| client_id_for(std::env::var("ANIMA_PORT").ok().as_deref()));

#[derive(Debug, Clone)]
pub struct ComfyError(pub String);
impl std::fmt::Display for ComfyError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ComfyError {}

/// 即時狀態（網頁的 hello／state 會帶）
#[derive(Debug, Clone, Default)]
pub struct Live {
    pub online: bool,
    pub queue_remaining: i64,
    /// 目前執行中的 prompt_id
    pub running: Option<String>,
    /// {"value":, "max":}
    pub progress: Option<Value>,
}

pub struct Comfy {
    url: String,
    ws_url: String,
    http: reqwest::Client,
    db: Arc<Db>,
    hub: Hub,
    live: Mutex<Live>,
    diagnostics: OnceLock<Arc<crate::diagnostics::Store>>,
}

impl Comfy {
    /// 連的是哪個 ComfyUI（狀態列「ComfyUI 原生介面」用；可能不是 8188，見 launcher::find_running）
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn new(url: &str, db: Arc<Db>, hub: Hub) -> Arc<Comfy> {
        let url = url.trim_end_matches('/').to_string();
        let ws_url = format!("{}/ws?clientId={}", url.replacen("http", "ws", 1), CLIENT_ID.as_str());
        // 本機的 ComfyUI 直連；雲端網址模式照代理設定（net.rs）
        let http = reqwest::Client::builder()
            .proxy(crate::net::proxy())
            .read_timeout(Duration::from_secs(600))
            .build()
            .expect("建立 HTTP client");
        Arc::new(Comfy {
            url,
            ws_url,
            http,
            db,
            hub,
            live: Mutex::new(Live::default()),
            diagnostics: OnceLock::new(),
        })
    }

    /// 背景連 websocket（斷線每 3 秒重連）
    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move { me.ws_loop().await });
    }

    pub fn attach_diagnostics(&self, store: Arc<crate::diagnostics::Store>) { let _ = self.diagnostics.set(store); }

    pub fn live(&self) -> Live {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn with_live<T>(&self, f: impl FnOnce(&mut Live) -> T) -> T {
        f(&mut self.live.lock().unwrap_or_else(|e| e.into_inner()))
    }

    // ---------------------------------------------------------------- REST
    pub async fn get(&self, path: &str) -> Result<Value, ComfyError> {
        let r = self
            .http
            .get(format!("{}{path}", self.url))
            .send()
            .await
            .map_err(conn_err)?;
        let status = r.status();
        let body = r.text().await.map_err(conn_err)?;
        if status.is_client_error() || status.is_server_error() {
            return Err(ComfyError(format!(
                "ComfyUI 回應 {status}：{}",
                head(&body, 300)
            )));
        }
        serde_json::from_str(&body).map_err(|e| ComfyError(format!("ComfyUI 回傳的不是 JSON：{e}")))
    }

    /// 讀 ComfyUI 存的圖（/view）：kind 是 output／input／temp
    pub async fn view(&self, filename: &str, subfolder: &str, kind: &str) -> Result<Vec<u8>, ComfyError> {
        let r = self
            .http
            .get(format!("{}/view", self.url))
            .query(&[("filename", filename), ("subfolder", subfolder), ("type", kind)])
            .send()
            .await
            .map_err(conn_err)?;
        let status = r.status();
        if !status.is_success() {
            return Err(ComfyError(format!("ComfyUI 回應 {status}（讀 {filename}）")));
        }
        Ok(r.bytes().await.map_err(conn_err)?.to_vec())
    }

    pub async fn post(&self, path: &str, payload: &Value) -> Result<Value, ComfyError> {
        let (ok, data) = self.post_raw(path, payload).await?;
        if !ok {
            return Err(ComfyError(explain(&data)));
        }
        Ok(data)
    }

    /// POST → (成功與否, 回應)。/prompt 驗證失敗時要看回應裡的 node_errors
    async fn post_raw(&self, path: &str, payload: &Value) -> Result<(bool, Value), ComfyError> {
        if path == "/prompt" { crate::diagnostics::snapshot("comfy_request", payload); }
        else if path == "/free" { crate::diagnostics::event("unload_requested", payload.clone()); }
        let r = self
            .http
            .post(format!("{}{path}", self.url))
            .json(payload)
            .send()
            .await
            .map_err(conn_err)?;
        let status = r.status();
        let body = r.text().await.map_err(conn_err)?;
        let data: Value = serde_json::from_str(&body).unwrap_or_else(|_| json!({"raw": body}));
        if matches!(path, "/prompt" | "/free") { crate::diagnostics::event("comfy_http_response", json!({"path":path,"status":status.as_u16(),"data":data})); }
        Ok((status.as_u16() < 400, data))
    }

    pub async fn submit(&self, nodes: &Value, extra_pnginfo: &Value) -> Result<String, ComfyError> {
        if let Some(run) = crate::diagnostics::current() {
            let result = self.submit_recorded(nodes, extra_pnginfo).await;
            if let Err(e) = &result { run.fail(&e.0); }
            return result;
        }
        let fallback = self.diagnostics.get().and_then(|store| store.begin("/comfy/submit", extra_pnginfo));
        if let Some(run) = fallback {
            let result = crate::diagnostics::scope(run.clone(), self.submit_recorded(nodes, extra_pnginfo)).await;
            run.http_finished(if result.is_ok() {200} else {500});
            if let Err(e) = &result { run.fail(&e.0); }
            result
        } else { self.submit_recorded(nodes, extra_pnginfo).await }
    }

    async fn submit_recorded(&self, nodes: &Value, extra_pnginfo: &Value) -> Result<String, ComfyError> {
        crate::diagnostics::snapshot("workflow_requested", nodes);
        crate::diagnostics::snapshot("generation_parameters", extra_pnginfo);
        // 預設的文字編碼器／VAE 檔名換成這台 ComfyUI 手上有的同家版本（別人的電腦檔名常常不一樣）
        let mut nodes = nodes.clone();
        fix_loaders(self, &mut nodes).await?;
        crate::diagnostics::snapshot("workflow_api", &nodes);
        let payload = |n: &Value| json!({"prompt": n, "client_id": CLIENT_ID.as_str(), "extra_data": {"extra_pnginfo": extra_pnginfo}});
        let (mut ok, mut r) = self.post_raw("/prompt", &payload(&nodes)).await?;
        // 模型檔名只差在斜線方向、子資料夾（別台電腦的工作流）：照 ComfyUI 附的清單換成同一個檔，再送一次
        if !ok && fix_listed_names(&mut nodes, &r) {
            crate::diagnostics::snapshot("workflow_retry_resolved_names", &nodes);
            (ok, r) = self.post_raw("/prompt", &payload(&nodes)).await?;
        }
        match r.get("prompt_id").and_then(Value::as_str) {
            // 有 prompt_id＝已經排進佇列：node_errors 只是某些輸出節點跑不了（其他的照跑），記下來就好
            Some(id) if ok => {
                if let Some(run) = crate::diagnostics::current() { run.store.bind_job(&run.id, id); }
                if crate::py::truthy(r.get("node_errors")) {
                    eprintln!("ComfyUI 收下了，但有些節點跑不了：{}", explain(&json!({"node_errors": r["node_errors"]})));
                }
                Ok(id.to_owned())
            }
            _ => Err(ComfyError(explain(&r))),
        }
    }

    pub async fn object_info(&self, node: &str) -> Result<Value, ComfyError> {
        let mut v = self.get(&format!("/object_info/{node}")).await?;
        v.get_mut(node)
            .map(Value::take)
            .ok_or_else(|| ComfyError(format!("ComfyUI 沒有 {node} 節點")))
    }

    /// 某個節點某個欄位的選項（模型清單、取樣器…）
    pub async fn options(&self, node: &str, field: &str) -> Result<Vec<Value>, ComfyError> {
        let info = self.object_info(node).await?;
        let spec = &info["input"]["required"][field];
        if let Some(list) = spec[0].as_array() {
            return Ok(list.clone());
        }
        Ok(spec[1]["options"].as_array().cloned().unwrap_or_default())
    }

    /// ComfyUI 全部節點的資訊（GET /object_info，整份可能好幾 MB）。快取 60 秒；
    /// 工作流匯入（UI 格式要靠它把 widgets_values 對回欄位名）和參數面板（欄位型別、範圍、選項）用
    pub async fn object_info_all(&self) -> Result<Arc<Value>, ComfyError> {
        type Cached = Option<(String, std::time::Instant, Arc<Value>)>;
        const TTL: Duration = Duration::from_secs(60);
        static CACHE: Mutex<Cached> = Mutex::new(None);
        let fresh = |c: &Cached| c.as_ref().filter(|(u, t, _)| *u == self.url && t.elapsed() < TTL).map(|(_, _, v)| v.clone());
        if let Some(v) = fresh(&CACHE.lock().unwrap_or_else(|e| e.into_inner())) {
            return Ok(v);
        }
        if !self.live().online {
            return Err(ComfyError("ComfyUI 沒有連線，讀不到節點資訊。解法：重新開啟「繪匣 PaintDeck」。".into()));
        }
        let v = Arc::new(self.get("/object_info").await?);
        *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((self.url.clone(), std::time::Instant::now(), v.clone()));
        Ok(v)
    }

    // ---------------------------------------------------------------- websocket
    async fn ws_loop(self: Arc<Self>) {
        loop {
            // 連不上＝ComfyUI 沒開或重啟中：等一下再試
            if let Ok((ws, _)) = tokio_tungstenite::connect_async(self.ws_url.as_str()).await {
                self.with_live(|l| l.online = true);
                self.hub.send(json!({"type": "comfy", "online": true}));
                self.recover().await;
                let (_, mut rx) = ws.split();
                while let Some(msg) = rx.next().await {
                    match msg {
                        Ok(Message::Text(t)) => {
                            if let Ok(m) = serde_json::from_str::<Value>(t.as_str()) {
                                self.on_json(&m).await;
                            }
                        }
                        Ok(Message::Binary(b)) => self.on_binary(&b),
                        Ok(Message::Close(_)) => break,
                        Ok(_) => {}
                        Err(e) => {
                            eprintln!("ComfyUI 連線中斷：{e}");
                            break;
                        }
                    }
                }
            }
            if self.with_live(|l| std::mem::replace(&mut l.online, false)) {
                self.hub.send(json!({"type": "comfy", "online": false}));
                if let Some(store) = self.diagnostics.get() { store.disconnected(); }
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }

    fn on_binary(&self, b: &[u8]) {
        if b.len() < 8 {
            return;
        }
        let event = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        let img = match event {
            1 => &b[8..], // PREVIEW_IMAGE: [type][format][image]
            4 => {
                // PREVIEW_IMAGE_WITH_METADATA: [type][meta len][meta json][image]
                let n = u32::from_be_bytes([b[4], b[5], b[6], b[7]]) as usize;
                match b.get(8 + n..) {
                    Some(x) => x,
                    None => return,
                }
            }
            _ => return,
        };
        self.hub.send_bin(bytes::Bytes::copy_from_slice(img));
    }

    async fn on_json(&self, m: &Value) {
        let t = m.get("type").and_then(Value::as_str).unwrap_or("");
        let empty = json!({});
        let d = m.get("data").unwrap_or(&empty);
        let pid = d
            .get("prompt_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(store) = self.diagnostics.get() {
            if let Some(job) = pid.as_deref() { store.job_event(job, t, d); }
        }
        match t {
            "status" => {
                let n = d["status"]["exec_info"]["queue_remaining"]
                    .as_i64()
                    .unwrap_or(0);
                self.with_live(|l| l.queue_remaining = n);
                self.hub.send(json!({"type": "queue", "remaining": n}));
            }
            "execution_start" => {
                self.with_live(|l| {
                    l.running = pid.clone();
                    l.progress = None;
                });
                if let Some(p) = &pid {
                    if matches!(self.db.job(p), Ok(Some(_))) {
                        let _ = self.db.set_job(p, "running", None);
                    }
                }
                self.hub.send(json!({"type": "start", "job": pid}));
            }
            "progress" => {
                let prog = json!({"value": d.get("value").cloned().unwrap_or(Value::Null), "max": d.get("max").cloned().unwrap_or(Value::Null)});
                self.with_live(|l| l.progress = Some(prog.clone()));
                self.hub.send(json!({"type": "progress", "job": pid, "value": prog["value"], "max": prog["max"]}));
            }
            "executed" => {
                let out = d.get("output").cloned().unwrap_or_else(|| json!({}));
                self.store(pid.as_deref(), &out);
            }
            "execution_success" => self.finish(pid.as_deref(), "done", None),
            "execution_interrupted" => self.finish(pid.as_deref(), "cancelled", None),
            "execution_error" => {
                let s = |k: &str| py_str(d.get(k));
                let msg = format!(
                    "{}: {}",
                    s("node_type"),
                    d.get("exception_message")
                        .map(|v| py_str(Some(v)))
                        .unwrap_or_default()
                );
                self.finish(pid.as_deref(), "error", Some(friendly(msg.trim())));
            }
            _ => {}
        }
    }

    fn store(&self, pid: Option<&str>, output: &Value) {
        let Some(pid) = pid else { return };
        let Ok(Some(job)) = self.db.job(pid) else {
            return; // 不是生圖站送出的任務（例如直接在 ComfyUI 介面跑的）
        };
        // gifs：Video Combine（VHS）存的影片（自訂影片工作流）
        let saved = ["images", "gifs"].into_iter().flat_map(|k| output[k].as_array().into_iter().flatten());
        for im in saved {
            if im.get("type").and_then(Value::as_str) != Some("output") {
                continue;
            }
            let (Some(fname), sub) = (
                im["filename"].as_str(),
                im.get("subfolder").and_then(Value::as_str).unwrap_or(""),
            ) else {
                continue;
            };
            // 影片的封面（第一格）只拿來做縮圖，不進圖庫
            if sub.replace('\\', "/").starts_with("AnimaWeb/_poster") {
                continue;
            }
            match self.db.add_image(
                fname,
                sub,
                "output",
                &job["info"],
                &job["params"],
                Some(pid),
            ) {
                // 整本上色的原始輸出：只記一筆給背景工作找（跑完就搬走、從圖庫隱藏），不推到網頁的圖庫
                Ok(_) if job["params"].get("colorbook").is_some() => {}
                Ok(iid) => {
                    let image = self
                        .db
                        .image(iid)
                        .ok()
                        .flatten()
                        .map(|r| {
                            let mut rows = [r];
                            let _ = self.db.add_gen_seconds(&mut rows);
                            let [r] = rows;
                            Value::Object(r)
                        })
                        .unwrap_or(Value::Null);
                    self.hub
                        .send(json!({"type": "image", "job": pid, "image": image}));
                }
                Err(e) => eprintln!("圖片存進圖庫失敗：{e}"),
            }
        }
    }

    fn finish(&self, pid: Option<&str>, status: &str, error: Option<String>) {
        if let Some(p) = pid {
            if matches!(self.db.job(p), Ok(Some(_))) {
                let _ = self.db.set_job(p, status, error.as_deref());
            }
        }
        self.with_live(|l| {
            if l.running.as_deref() == pid {
                l.running = None;
                l.progress = None;
            }
        });
        self.hub
            .send(json!({"type": "finish", "job": pid, "status": status, "error": error, "diagnostic_id": pid.and_then(|p|self.diagnostics.get().and_then(|s|s.job_id(p)))}));
    }

    /// 重新連上時，補收斷線期間完成的任務
    async fn recover(&self) {
        for pid in self.db.open_jobs().unwrap_or_default() {
            let Ok(h) = self.get(&format!("/history/{pid}")).await else {
                continue;
            };
            if h.get(&pid).is_some() {
                self.store_history(&pid, &h);
            } else if let Ok(q) = self.get("/queue").await {
                let live: Vec<&str> = ["queue_running", "queue_pending"]
                    .iter()
                    .flat_map(|k| q[*k].as_array().into_iter().flatten())
                    .filter_map(|x| x[1].as_str())
                    .collect();
                if !live.contains(&pid.as_str()) {
                    let _ = self
                        .db
                        .set_job(&pid, "lost", Some("ComfyUI 重啟，任務遺失"));
                }
            }
        }
    }

    /// 把 /history 裡這個任務的結果收進圖庫、更新狀態（已經收過圖的不重複收）
    fn store_history(&self, pid: &str, h: &Value) {
        let Some(entry) = h.get(pid) else { return };
        let has = self.db.q1("SELECT id FROM images WHERE job=? LIMIT 1", [pid]).ok().flatten().is_some();
        if !has {
            for out in entry["outputs"].as_object().into_iter().flat_map(|o| o.values()) {
                self.store(Some(pid), out);
            }
        }
        if let Some(store) = self.diagnostics.get() {
            store.job_event(pid, "history_snapshot", &json!({"status":entry["status"],"outputs":entry["outputs"]}));
            if entry["status"]["status_str"] == "success" {
                store.job_event(pid, "execution_success", &entry["status"]);
            } else if entry["status"]["completed"] == true || entry["status"]["status_str"] == "error" {
                store.job_event(pid, "execution_error", &entry["status"]);
            }
        }
        let ok = entry["status"]["status_str"].as_str() == Some("success");
        let _ = self.db.set_job(pid, if ok { "done" } else { "error" }, if ok { None } else { Some("執行失敗") });
    }

    /// 送出後才登記的任務如果跑得太快（例如接片不重新編碼，不到一秒），
    /// websocket 的結果會比登記早到而被略過；登記完呼叫這個，從 /history 補收
    pub async fn sync_job(&self, pid: &str) {
        if let Ok(h) = self.get(&format!("/history/{pid}")).await {
            self.store_history(pid, &h);
        }
    }
}

fn conn_err(e: reqwest::Error) -> ComfyError {
    ComfyError(friendly(&format!("Cannot connect to ComfyUI: {e}")))
}

fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Python 的 str(x)：字串原樣、None → "None"、其他用 JSON 寫法
fn py_str(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => {
            if *b {
                "True".into()
            } else {
                "False".into()
            }
        }
        Some(other) => other.to_string(),
    }
}

/// 把 ComfyUI 的驗證錯誤整理成看得懂的一句話
pub fn explain(data: &Value) -> String {
    let mut parts: Vec<String> = vec![];
    if let Some(e) = data.get("error").filter(|e| e.is_object()) {
        parts.push(
            e.get("message")
                .map(|m| py_str(Some(m)))
                .unwrap_or_default(),
        );
    }
    if let Some(ne) = data.get("node_errors").and_then(Value::as_object) {
        for n in ne.values() {
            for e in n["errors"].as_array().into_iter().flatten() {
                let details = e
                    .get("details")
                    .map(|d| py_str(Some(d)))
                    .unwrap_or_default();
                parts.push(
                    format!(
                        "{}：{} {}",
                        py_str(n.get("class_type")),
                        py_str(e.get("message")),
                        details
                    )
                    .trim()
                    .to_string(),
                );
            }
        }
    }
    let joined = parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("；");
    friendly(&if joined.is_empty() {
        head(&dumps(data), 300)
    } else {
        joined
    })
}

/// 常見錯誤 → 白話說明＋解法（原文附在後面，方便查）
const FRIENDLY: &[(&[&str], &str)] = &[
    // 要排在「少了節點」前面：MOSS 照指令改圖的節點在繪匣的 comfy_moss 補丁裡（2026-10-30），舊補丁沒有
    (&["textencodemossimageedit"],
     "這台 ComfyUI 的 MOSS 支援還是舊版，沒有「照指令改圖」要的節點。解法：ComfyUI 是 0.38.0 的話，到「設定 → 安裝與修復」再按一次安裝，會自動換上新的 MOSS 支援；其他版本請更新 ComfyUI 到 0.43 以後。"),
    // 新版 ComfyUI：missing_node_type「Node 'X' not found. The custom node may not be installed.」；舊版：「Cannot execute because node X does not exist.」
    (&["custom node may not be installed", "cannot execute because node"],
     "這台 ComfyUI 少了工作流要用的節點（自訂節點沒裝，或 ComfyUI 版本太舊）。解法：用 ComfyUI Manager 安裝缺少的節點（名稱在下面的原始訊息裡），裝完重開 ComfyUI；匯入的工作流也可以到「設定 → 模型與工作流」看缺哪些節點。"),
    (&["out of memory", "outofmemory", "cuda error: out", "allocation on device"],
     "顯示卡記憶體（VRAM）不夠。解法：張數改少、尺寸改小、關掉吃顯卡的遊戲；或到右上角狀態列按「釋放顯存」再試一次。"),
    // 要排在「value not in list」前面：CLIPLoader 的 type 選單沒有 moss_image＝ComfyUI 程式太舊，跟模型檔無關（2026-10-30 有人因此重下模型）
    (&["'moss_image' not in"],
     "這台 ComfyUI 的程式還沒有 MOSS-Image 支援（模型檔沒問題，不用重新下載；官方正式版到 0.40.0 都還沒有 MOSS）。解法：ComfyUI 是 0.38.0 的話，到「設定 → 安裝與修復」再按一次安裝，會自動補上；其他版本的免安裝版執行 ComfyUI 資料夾裡的 update\\update_comfyui.bat 更新到最新開發版（不要用 update_comfyui_stable.bat），或讓安裝精靈另外下載一份 ComfyUI。"),
    // 要排在「value not in list」前面：H3 影片的模型檔沒裝（2026-10-30 回報：站叫人去 LoRA 分頁刷新，方向錯）
    (&["minimax_h3", "10eros_max_h3", "qwen3vl_32b_minimax"],
     "這台 ComfyUI 沒有 H3 影片的模型檔（主模型、文字編碼器或 VAE）。解法：到安裝精靈勾「H3 影片（本機，選配）」下載，或在 H3 影片台改用雲端。"),
    (&["value not in list"],
     "找不到選的模型或 LoRA 檔（可能被移走或改名；匯入的工作流也可能寫著你沒有的檔）。解法：LoRA 分頁按「重新整理清單」，重新選一次模型／LoRA；用「⚙ 生圖工作流」時，到左欄「工作流參數」把標紅的欄位換成你有的檔（檔名在下面的原始訊息裡）。"),
    (&["mat1 and mat2", "size mismatch", "shapes cannot be multiplied", "lora key not loaded"],
     "模型和 LoRA 不相容（多半是家族不同，例如 Anima 的 LoRA 套在 MOSS-Image 上）。解法：把劃掉的 LoRA 移除，或換回對應的模型。"),
    (&["no such file", "filenotfound", "invalid image file", "cannot identify image"],
     "找不到輸入圖片，或圖片壞掉了。解法：重新放一次圖片。"),
    (&["cannot connect", "connection refused", "connect call failed"],
     "連不到 ComfyUI。解法：重新開啟「繪匣 PaintDeck」。"),
    (&["expected all tensors to be on the same device"],
     "ComfyUI 模型載入狀態異常。解法：狀態列按「釋放顯存」，或重開 ComfyUI。"),
];

pub fn friendly(msg: &str) -> String {
    if msg.is_empty() {
        return String::new();
    }
    let low = msg.to_lowercase();
    for (keys, text) in FRIENDLY {
        if keys.iter().any(|k| low.contains(k)) {
            return format!("{text}\n（原始訊息：{}）", head(msg, 300));
        }
    }
    msg.to_string()
}

/// 檔名在 ComfyUI 的清單裡找不到，但其實是同一個檔：斜線方向不同（Windows 的 \ 和 Linux 的 /）、大小寫不同、
/// 或模型放在子資料夾裡（工作流寫 a.safetensors，清單是 SDXL\a.safetensors）→ 回清單裡那一個。只比檔名時要剛好一個對得上才換
pub fn match_listed(cur: &str, have: &[String]) -> Option<String> {
    if cur.is_empty() || have.iter().any(|h| h == cur) {
        return None;
    }
    let key = |s: &str| s.replace('\\', "/").to_lowercase();
    let k = key(cur);
    if let Some(h) = have.iter().find(|h| key(h) == k) {
        return Some(h.clone());
    }
    let base = |s: &str| s.rsplit('/').next().unwrap_or("").to_string();
    let b = base(&k);
    let hits: Vec<&String> = have.iter().filter(|h| base(&key(h)) == b).collect();
    if let [one] = hits[..] {
        Some(one.clone())
    } else {
        None
    }
}

/// /prompt 回的 value_not_in_list：照 ComfyUI 附的清單（extra_info.input_config，舊格式 [清單, {…}]、新格式 ["COMBO", {"options": 清單}]）
/// 把對得上的檔名換掉。有換到回 true
fn fix_listed_names(nodes: &mut Value, r: &Value) -> bool {
    let mut changed = false;
    for (id, n) in r["node_errors"].as_object().into_iter().flatten() {
        for e in n["errors"].as_array().into_iter().flatten().filter(|e| e["type"] == "value_not_in_list") {
            let x = &e["extra_info"];
            let (Some(field), Some(cur)) = (x["input_name"].as_str(), x["received_value"].as_str()) else {
                continue;
            };
            let cfg = &x["input_config"];
            let have: Vec<String> = cfg[0].as_array().or_else(|| cfg[1]["options"].as_array()).into_iter().flatten()
                .filter_map(|v| v.as_str().map(str::to_owned)).collect();
            let Some(h) = match_listed(cur, &have) else { continue };
            // 只改送出去的那個值（節點、欄位要真的存在）
            if nodes.get(id).and_then(|n| n["inputs"].get(field)).and_then(Value::as_str) == Some(cur) {
                nodes[id]["inputs"][field] = json!(h);
                changed = true;
            }
        }
    }
    changed
}

// ---------------------------------------------------------------- 預設檔名換成手上有的版本（文字編碼器、VAE、修臉、去背、深度、構圖控制）
/// 生圖站內建流程寫死的模型檔名：ComfyUI 沒有這個檔時，換成同一家的其他版本或改過名字的檔（比對時不分大小寫、- 和 _ 當成一樣）。
/// 別人的 ComfyUI 常放官方倉庫的其他版本（MOSS 文字編碼器有 bf16／int8_convrot／w4a8_gptq，VAE 有 flux2-vae／flux2-vae_small），
/// 或改過名字的（moss_image_vae_bf16）。只換這幾個預設名稱：使用者自己的工作流照原樣送。
struct Alt {
    /// 載入這個檔的節點和欄位
    class: &'static str,
    field: &'static str,
    /// 生圖站寫死的預設檔名
    name: &'static str,
    /// 給人看的名字
    label: &'static str,
    /// 候選的關鍵字，依序（比對前先把 - 和空白換成 _、轉小寫）
    keys: &'static [&'static str],
    /// 這種檔要放的資料夾（ComfyUI\models 底下）和去哪下載（找不到時照這個講）
    folder: &'static str,
    from: &'static str,
    /// 這個資料夾裡只有一個檔的話（使用者改了名字），就用那一個
    single: bool,
}

// 表格用的建構函式：一列一個欄位，比拆成結構體好讀
#[allow(clippy::too_many_arguments)]
const fn alt(class: &'static str, field: &'static str, name: &'static str, label: &'static str, keys: &'static [&'static str], folder: &'static str, from: &'static str, single: bool) -> Alt {
    Alt { class, field, name, label, keys, folder, from, single }
}

const LOADER_ALTS: &[Alt] = &[
    alt("CLIPLoader", "clip_name", crate::graphs::MOSS_TE, "MOSS-Image 的文字編碼器（Qwen3.5 9B）", &["qwen3.5_9b_w4a8", "qwen3.5_9b_int8", "qwen3.5_9b_fp8", "qwen3.5_9b"], "text_encoders", "Hugging Face 的 Comfy-Org/MOSS-Image", false),
    alt("VAELoader", "vae_name", crate::graphs::MOSS_VAE, "MOSS-Image 的 VAE（flux2-vae）", &["flux2_vae.", "moss_image_vae", "flux2_vae"], "vae", "Hugging Face 的 Comfy-Org/MOSS-Image", false),
    alt("CLIPLoader", "clip_name", crate::graphs::QWEN21_TE, "Qwen 2.1 的文字編碼器（Qwen3-VL 8B）", &["qwen3vl_8b_w4a8", "qwen3vl_8b", "qwen3_vl_8b"], "text_encoders", "Hugging Face 的 Comfy-Org/Qwen-Image-2.1", false),
    alt("VAELoader", "vae_name", crate::graphs::QWEN21_VAE, "Qwen 2.1 的 VAE", &["qwen_image_2.1_vae", "qwen_image_2_1_vae"], "vae", "Hugging Face 的 Comfy-Org/Qwen-Image-2.1", false),
    alt("CLIPLoader", "clip_name", crate::graphs::CLIP, "Anima 的文字編碼器（Qwen3 0.6B）", &["qwen_3_06b", "qwen3_0.6b", "qwen_3_0.6b"], "text_encoders", "Hugging Face 的 circlestone-labs/Anima", false),
    alt("VAELoader", "vae_name", crate::graphs::VAE, "Anima 的 VAE（qwen_image_vae）", &["qwen_image_vae"], "vae", "Hugging Face 的 circlestone-labs/Anima", false),
    // 修臉／修手（SAM3）放在 checkpoints，那裡有很多檔，所以不能用「只有一個檔」判斷
    alt("CheckpointLoaderSimple", "ckpt_name", crate::graphs::SAM3_CKPT, "修臉／修手用的偵測模型（SAM 3.1）", &["sam3.1_multiplex", "sam3.1", "sam_3.1", "sam3"], "checkpoints", "Hugging Face 的 Comfy-Org/sam3.1", false),
    alt("LoadBackgroundRemovalModel", "bg_removal_name", crate::graphs::BG_MODEL, "一鍵去背用的模型（BiRefNet）", &["birefnet", "bg_removal", "rmbg"], "background_removal", "Hugging Face 的 Comfy-Org/BiRefNet", true),
    alt("LoadDA3Model", "model_name", crate::graphs::DEPTH_MODEL, "深度估計模型（Depth Anything 3）", &["depth_anything_3_mono_large", "depth_anything_3_mono", "depth_anything_3", "da3_mono", "da3"], "geometry_estimation", "Hugging Face 的 Comfy-Org/Depth-Anything-3", true),
    alt("ModelPatchLoader", "name", crate::graphs::POSE_CONTROLNET, "Qwen 2.1 的姿勢參考模型（Fun ControlNet）", &["qwen_image_2.1_fun_controlnet_union", "fun_controlnet_union"], "model_patches", "Hugging Face 的 Comfy-Org/Qwen-Image-2.1", false),
    alt("ModelPatchLoader", "name", "anima-lllite-lineart-1.safetensors", "Anima 構圖控制（線稿）", &["lllite_lineart"], "model_patches", "Hugging Face 的 Comfy-Org/Anima-LLLite", false),
    alt("ModelPatchLoader", "name", "anima-lllite-scribble-1.safetensors", "Anima 構圖控制（塗鴉）", &["lllite_scribble"], "model_patches", "Hugging Face 的 Comfy-Org/Anima-LLLite", false),
    alt("ModelPatchLoader", "name", "anima-lllite-depth-1.safetensors", "Anima 構圖控制（深度）", &["lllite_depth"], "model_patches", "Hugging Face 的 Comfy-Org/Anima-LLLite", false),
    alt("ModelPatchLoader", "name", "anima-lllite-inpainting-v2.safetensors", "Anima 局部重繪的構圖控制", &["lllite_inpaint"], "model_patches", "Hugging Face 的 Comfy-Org/Anima-LLLite", false),
    alt("ModelPatchLoader", "name", "anima-lllite-any-test-like-v2.safetensors", "Anima 構圖控制（通用）", &["lllite_any"], "model_patches", "Hugging Face 的 Comfy-Org/Anima-LLLite", false),
    alt("ModelPatchLoader", "name", "anima-lllite-pose-1.safetensors", "Anima 構圖控制（姿勢骨架）", &["lllite_pose"], "model_patches", "Hugging Face 的 Comfy-Org/Anima-LLLite", false),
    alt("CheckpointLoaderSimple", "ckpt_name", crate::graphs::SDPOSE_CKPT, "抽骨架用的模型（SDPose）", &["sdpose_wholebody", "sdpose"], "checkpoints", "Hugging Face 的 Comfy-Org/SDPose", false),
];

fn norm(s: &str) -> String {
    s.to_lowercase().replace(['-', ' '], "_")
}

/// 預設檔名 name 在 have 裡沒有時，挑同一家的替代品。
/// Ok(None)＝不用換（有這個檔，或不是生圖站的預設名稱）；Err＝是預設名稱但同一家一個都沒有（給人看的原因，講清楚缺哪個檔、放哪、去哪下載）
pub fn pick_alternative(name: &str, have: &[String]) -> Result<Option<String>, String> {
    if have.iter().any(|h| h == name) {
        return Ok(None);
    }
    let Some(a) = LOADER_ALTS.iter().find(|a| a.name == name) else {
        return Ok(None);
    };
    for k in a.keys {
        let k = norm(k);
        // Anima 的 qwen_image_vae 不能拿 Qwen 2.1 的 VAE（qwen_image_2.1_vae）來頂
        if let Some(h) = have.iter().find(|h| {
            let h = norm(h);
            h.contains(&k) && !(name == crate::graphs::VAE && h.contains("2.1"))
        }) {
            return Ok(Some(h.clone()));
        }
    }
    // 資料夾裡只有一個檔：多半是使用者改了名字
    if a.single {
        let files: Vec<&String> = have.iter().filter(|h| !h.is_empty() && !h.eq_ignore_ascii_case("none")).collect();
        if let [only] = files[..] {
            return Ok(Some(only.clone()));
        }
    }
    Err(format!(
        "ComfyUI 裡找不到{}：生圖站要的是 {name}（或同系列的其他版本）。解法：把檔案放進 ComfyUI 的 models\\{} 資料夾（下載：{}），或到「設定 → 安裝與修復」再按一次安裝補下載；放好之後重開 ComfyUI",
        a.label, a.folder, a.from
    ))
}

/// 送出前：預設檔名（ALTS 那幾個）ComfyUI 卻沒有的，換成手上有的同家版本，找不到就講清楚缺什麼
async fn fix_loaders(comfy: &Comfy, nodes: &mut Value) -> Result<(), ComfyError> {
    let mut pairs: Vec<(&str, &str)> = vec![];
    for a in LOADER_ALTS {
        if !pairs.contains(&(a.class, a.field)) {
            pairs.push((a.class, a.field));
        }
    }
    for (cls, field) in pairs {
        let Some(map) = nodes.as_object_mut() else { return Ok(()) };
        let is_default = |v: &str| LOADER_ALTS.iter().any(|a| a.class == cls && a.field == field && a.name == v);
        let targets: Vec<String> = map
            .iter()
            .filter(|(_, n)| n["class_type"] == cls)
            .filter(|(_, n)| n["inputs"][field].as_str().is_some_and(is_default))
            .map(|(id, _)| id.clone())
            .collect();
        if targets.is_empty() {
            continue;
        }
        // 讀不到清單（舊版 ComfyUI、連線問題）就照原樣送，讓 ComfyUI 自己報錯
        let Ok(list) = comfy.options(cls, field).await else { continue };
        let have: Vec<String> = list.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect();
        let map = nodes.as_object_mut().expect("上面確認過是物件");
        for id in targets {
            let cur = map[&id]["inputs"][field].as_str().unwrap_or_default().to_string();
            if let Some(alt) = pick_alternative(&cur, &have).map_err(ComfyError)? {
                map[&id]["inputs"][field] = json!(alt);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_stations_get_their_own_client_id() {
        assert_eq!(client_id_for(None), "anima-web");
        assert_eq!(client_id_for(Some("8190")), "anima-web");
        assert_eq!(client_id_for(Some("8191")), "anima-web-8191");
    }

    use super::*;

    fn have(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// 2026-10-21 有人回報：MOSS 的 VAE、文字編碼器檔名跟生圖站預設的不一樣，ComfyUI 拒絕執行
    #[test]
    fn moss_alternatives() {
        let vaes = have(&["flux2-vae_small.safetensors", "moss_image_vae_bf16.safetensors", "qwen_image_vae.safetensors", "pixel_space"]);
        assert_eq!(pick_alternative(crate::graphs::MOSS_VAE, &vaes), Ok(Some("moss_image_vae_bf16.safetensors".into())));
        assert_eq!(pick_alternative(crate::graphs::MOSS_VAE, &have(&["flux2-vae_small.safetensors"])), Ok(Some("flux2-vae_small.safetensors".into())));
        assert_eq!(pick_alternative(crate::graphs::MOSS_VAE, &have(&["flux2-vae.safetensors"])), Ok(None));
        let tes = have(&["qwen_3_06b_base.safetensors", "qwen3.5_9b_bf16.safetensors", "Qwen3.5-9B-int8_convrot.safetensors"]);
        assert_eq!(pick_alternative(crate::graphs::MOSS_TE, &tes), Ok(Some("Qwen3.5-9B-int8_convrot.safetensors".into())));
        assert_eq!(pick_alternative(crate::graphs::MOSS_TE, &have(&["qwen3.5_9b_bf16.safetensors"])), Ok(Some("qwen3.5_9b_bf16.safetensors".into())));
        let e = pick_alternative(crate::graphs::MOSS_TE, &have(&["qwen_3_06b_base.safetensors"])).unwrap_err();
        assert!(e.contains("MOSS") && e.contains(crate::graphs::MOSS_TE), "{e}");
    }

    /// 2026-10-30 有人回報：ComfyUI 程式沒有 MOSS 支援，以前講成「找不到模型或 LoRA 檔」，害人去重下模型
    #[test]
    fn friendly_video_models() {
        let m = friendly("CLIPLoader: Value not in list: clip_name: 'qwen3vl_32b_minimax_h3_nvfp4_awq.safetensors' not in ['a.safetensors']");
        assert!(m.contains("H3 影片（本機，選配）") && !m.contains("LoRA 分頁"), "{m}");
        let g = friendly("UNETLoader: Value not in list: unet_name: 'anima-base.safetensors' not in ['b.safetensors']");
        assert!(g.contains("LoRA 分頁") && !g.contains("H3"), "{g}");
    }

    #[test]
    fn moss_type_missing_is_not_a_missing_file() {
        let m = friendly("Prompt outputs failed validation；CLIPLoader: Value not in list: type: 'moss_image' not in (list of length 29)");
        assert!(m.contains("MOSS-Image 支援") && m.contains("update_comfyui.bat") && !m.contains("找不到選的模型"), "{m}");
    }

    #[test]
    fn listed_names_match_slashes_and_subfolders() {
        let list = have(&["SDXL\\juggernaut.safetensors", "anima\\Style.safetensors", "a\\dup.safetensors", "b\\dup.safetensors"]);
        // 斜線方向、大小寫
        assert_eq!(match_listed("SDXL/juggernaut.safetensors", &list), Some("SDXL\\juggernaut.safetensors".into()));
        assert_eq!(match_listed("anima/style.safetensors", &list), Some("anima\\Style.safetensors".into()));
        // 沒寫子資料夾、清單裡只有一個同名的
        assert_eq!(match_listed("juggernaut.safetensors", &list), Some("SDXL\\juggernaut.safetensors".into()));
        assert_eq!(match_listed("other/juggernaut.safetensors", &list), Some("SDXL\\juggernaut.safetensors".into()));
        // 好幾個同名的：不猜；本來就有的、清單裡沒有的：不換
        assert_eq!(match_listed("dup.safetensors", &list), None);
        assert_eq!(match_listed("SDXL\\juggernaut.safetensors", &list), None);
        assert_eq!(match_listed("missing.safetensors", &list), None);

        // ComfyUI 的 value_not_in_list 回應 → 換掉再送
        let mut nodes = json!({"4": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "SDXL/juggernaut.safetensors"}},
                               "5": {"class_type": "LoraLoader", "inputs": {"lora_name": "nope.safetensors"}}});
        let r = json!({"error": {"type": "prompt_outputs_failed_validation"}, "node_errors": {
            "4": {"class_type": "CheckpointLoaderSimple", "errors": [{"type": "value_not_in_list", "message": "Value not in list",
                  "extra_info": {"input_name": "ckpt_name", "received_value": "SDXL/juggernaut.safetensors", "input_config": [["SDXL\\juggernaut.safetensors"], {}]}}]},
            "5": {"class_type": "LoraLoader", "errors": [{"type": "value_not_in_list", "message": "Value not in list",
                  "extra_info": {"input_name": "lora_name", "received_value": "nope.safetensors", "input_config": ["COMBO", {"options": ["x.safetensors"]}]}}]},
            "9": {"class_type": "Ghost", "errors": [{"type": "value_not_in_list",
                  "extra_info": {"input_name": "f", "received_value": "x.safetensors", "input_config": ["COMBO", {"options": ["y\\x.safetensors"]}]}}]}}});
        assert!(fix_listed_names(&mut nodes, &r));
        assert_eq!(nodes["4"]["inputs"]["ckpt_name"], "SDXL\\juggernaut.safetensors");
        assert_eq!(nodes["5"]["inputs"]["lora_name"], "nope.safetensors");
        assert!(nodes.get("9").is_none(), "不存在的節點不能被建出來");
        assert!(!fix_listed_names(&mut nodes, &json!({"node_errors": {}})));
    }

    #[test]
    fn other_families_and_custom_names() {
        // Anima 的 VAE 不能拿 Qwen 2.1 的頂
        assert!(pick_alternative(crate::graphs::VAE, &have(&["qwen_image_2.1_vae_bf16.safetensors"])).is_err());
        assert_eq!(pick_alternative(crate::graphs::VAE, &have(&["qwen_image_vae_fp16.safetensors"])), Ok(Some("qwen_image_vae_fp16.safetensors".into())));
        assert_eq!(pick_alternative(crate::graphs::QWEN21_VAE, &have(&["qwen_image_vae.safetensors", "qwen_image_2.1_vae_fp32.safetensors"])), Ok(Some("qwen_image_2.1_vae_fp32.safetensors".into())));
        assert_eq!(pick_alternative(crate::graphs::QWEN21_TE, &have(&["qwen3vl_8b_bf16.safetensors"])), Ok(Some("qwen3vl_8b_bf16.safetensors".into())));
        // 不是生圖站預設的名稱（使用者自己的工作流）：不管
        assert_eq!(pick_alternative("my_custom_vae.safetensors", &have(&["x.safetensors"])), Ok(None));
    }

    /// MOSS 照指令改圖（2026-10-30）：ComfyUI 還是舊補丁、沒有 TextEncodeMOSSImageEdit → 叫人重跑安裝精靈，不是去 Manager 找節點
    #[test]
    fn moss_edit_node_missing_points_to_setup() {
        let v = json!({"error": {"type": "missing_node_type", "message": "Node 'TextEncodeMOSSImageEdit' not found. The custom node may not be installed."}, "node_errors": {}});
        let e = explain(&v);
        assert!(e.contains("安裝與修復") && !e.contains("ComfyUI Manager"), "{e}");
    }

    /// 匯入的工作流用到沒裝的自訂節點：新舊版 ComfyUI 的回應都要翻成白話，節點名稱留在原始訊息裡
    #[test]
    fn missing_node_is_explained() {
        let new = json!({"error": {"type": "missing_node_type", "message": "Node 'Impact Pack Detailer' not found. The custom node may not be installed."}, "node_errors": {}});
        let old = json!({"error": {"type": "invalid_prompt", "message": "Cannot execute because node FaceDetailer does not exist."}, "node_errors": {}});
        for (v, name) in [(new, "Impact Pack Detailer"), (old, "FaceDetailer")] {
            let e = explain(&v);
            assert!(e.contains("ComfyUI Manager") && e.contains(name), "{e}");
        }
    }
}
