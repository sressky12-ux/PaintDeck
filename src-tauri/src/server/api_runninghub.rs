//! RunningHub API（crate::runninghub）：設定（金鑰 DPAPI 加密）、查餘額、把一份節點圖送上去跑、成品抓回圖庫。
//! 設定檔：<資料>\data\agent\cloud\runninghub.json（{site, instance, models, key_dpapi}）。
//! 目前接在 H3 影片台的「在哪跑」；長片要接上一段的 latent，RunningHub 一個任務一台機器，接不了。
use super::{bad, ok, ApiResult, Body, Shared};
use crate::director::tx;
use crate::runninghub as rh;
use axum::extract::State;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// 一個任務最久等多久
const MAX_WAIT: Duration = Duration::from_secs(60 * 60);
const POLL: Duration = Duration::from_secs(5);

fn cfg_path(app: &Shared) -> PathBuf {
    app.paths.data.join("agent").join("cloud").join("runninghub.json")
}

fn http() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .proxy(crate::net::proxy())
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(300))
            .build()
            .expect("建立 HTTP client")
    })
}

#[cfg(windows)]
pub(super) fn seal(s: &str) -> Result<String, String> {
    crate::settings::protect(s).map_err(|e| format!("加密金鑰失敗：{e}"))
}
#[cfg(not(windows))]
pub(super) fn seal(s: &str) -> Result<String, String> {
    Ok(s.to_string())
}
#[cfg(windows)]
pub(super) fn unseal(s: &str) -> Option<String> {
    crate::settings::unprotect(s).ok()
}
#[cfg(not(windows))]
pub(super) fn unseal(s: &str) -> Option<String> {
    Some(s.to_string())
}

/// 磁碟上的設定（金鑰還是加密的）
fn load(app: &Shared) -> Map<String, Value> {
    std::fs::read(cfg_path(app)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default()
}

pub struct Cfg {
    pub key: String,
    pub site: String,
    pub instance: &'static str,
    pub models: Map<String, Value>,
    pub workflow_id: String,
}

pub fn cfg(app: &Shared) -> Cfg {
    let m = load(app);
    Cfg {
        key: m.get("key_dpapi").and_then(Value::as_str).and_then(unseal).unwrap_or_default(),
        site: m.get("site").and_then(Value::as_str).filter(|s| rh::SITES.iter().any(|(k, _)| k == s)).unwrap_or("ai").to_string(),
        instance: rh::instance(m.get("instance").and_then(Value::as_str).unwrap_or("plus")),
        models: m.get("models").and_then(Value::as_object).cloned().unwrap_or_default(),
        workflow_id: m.get("workflow_id").and_then(Value::as_str).unwrap_or(rh::DEFAULT_WORKFLOW).to_string(),
    }
}

/// 有沒有填金鑰（H3 送出前先擋）
pub fn ready(app: &Shared) -> Result<(), String> {
    if cfg(app).key.is_empty() {
        return Err("還沒填 RunningHub 的 API Key：到 H3 影片台「在哪跑 → RunningHub」填".into());
    }
    Ok(())
}

fn view(app: &Shared) -> Value {
    let c = cfg(app);
    json!({"has_key": !c.key.is_empty(), "site": c.site, "instance": c.instance, "models": c.models, "workflow_id": c.workflow_id,
           "sites": rh::SITES.iter().map(|(k, h)| json!({"key": k, "host": h})).collect::<Vec<_>>(),
           "instances": rh::INSTANCES, "rates": rh::COINS_PER_SEC.iter().map(|(k, r)| json!({"instance": k, "coins_per_sec": r})).collect::<Vec<_>>()})
}

/// GET：目前設定（金鑰只說有沒有）
pub async fn get(State(app): State<Shared>) -> ApiResult {
    ok(view(&app))
}

/// {key?, site?, instance?, models?, workflow_id?}：key 沒帶＝不動、空字串＝清掉；models＝{我們的檔名: RunningHub 上的檔名}
pub async fn config(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let mut m = load(&app);
    if let Some(k) = b.get("key") {
        let k = tx(k).trim().to_string();
        if k.is_empty() {
            m.remove("key_dpapi");
        } else if k.len() > 200 || !k.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
            return Err(bad("API Key 的格式不對：從 RunningHub 的 API Key 頁整串複製過來"));
        } else {
            m.insert("key_dpapi".into(), json!(seal(&k).map_err(bad)?));
        }
    }
    if let Some(s) = b.get("site").and_then(Value::as_str) {
        if !rh::SITES.iter().any(|(k, _)| *k == s) {
            return Err(bad("沒有這個站：ai（國際版）或 cn（中國版）"));
        }
        m.insert("site".into(), json!(s));
    }
    if let Some(w) = b.get("workflow_id") {
        let w = tx(w).trim().to_string();
        if !w.is_empty() && (w.len() > 30 || !w.chars().all(|c| c.is_ascii_digit())) {
            return Err(bad("工作流編號只能是數字（RunningHub 網址 /post/ 後面那串）"));
        }
        m.insert("workflow_id".into(), json!(w));
    }
    if let Some(i) = b.get("instance").and_then(Value::as_str) {
        m.insert("instance".into(), json!(rh::instance(i)));
    }
    if let Some(Value::Object(models)) = b.get("models") {
        let mut clean = Map::new();
        for (from, to) in models.iter().take(64) {
            let (from, to) = (from.trim(), tx(to).trim().to_string());
            if from.is_empty() || to.is_empty() {
                continue;
            }
            if from.len() > 200 || to.len() > 200 || to.contains("..") {
                return Err(bad(format!("模型對照「{from}」寫得不對")));
            }
            clean.insert(from.to_string(), json!(to));
        }
        m.insert("models".into(), Value::Object(clean));
    }
    let p = cfg_path(&app);
    std::fs::create_dir_all(p.parent().unwrap_or(&app.paths.data)).map_err(|e| bad(e.to_string()))?;
    crate::director::write_atomic(&p, &Value::Object(m)).map_err(|e| bad(format!("存 RunningHub 設定失敗：{e}")))?;
    ok(view(&app))
}

async fn post_json(c: &Cfg, path: &str, body: &Value) -> Result<Value, String> {
    let r = http()
        .post(format!("{}{path}", rh::base(&c.site)))
        .bearer_auth(&c.key)
        .json(body)
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| format!("連不到 RunningHub：{e}"))?;
    let status = r.status();
    let text = r.text().await.map_err(|e| format!("RunningHub 回應讀不完：{e}"))?;
    serde_json::from_str(&text).map_err(|_| format!("RunningHub 回的不是 JSON（HTTP {status}）：{}", text.chars().take(200).collect::<String>()))
}

/// 測試金鑰＋看餘額（不花錢）：{ok, coins, running, raw}
pub async fn test(State(app): State<Shared>) -> ApiResult {
    let c = cfg(&app);
    if c.key.is_empty() {
        return Err(bad("還沒填 API Key"));
    }
    let v = post_json(&c, "/uc/openapi/accountStatus", &json!({"apikey": c.key})).await.map_err(bad)?;
    let code = v["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        return Err(bad(rh::err_msg(code, v["msg"].as_str().unwrap_or(""))));
    }
    let d = &v["data"];
    ok(json!({"ok": true, "coins": d["remainCoins"].clone(), "running": d["currentTaskCounts"].clone(),
              "money": d["remainMoney"].clone(), "api_type": d["apiType"].clone()}))
}

async fn upload(c: &Cfg, path: &std::path::Path) -> Result<String, String> {
    let bytes = tokio::fs::read(path).await.map_err(|e| format!("讀素材 {} 失敗：{e}", path.display()))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("input.bin");
    let boundary = format!("----paintdeck{:016x}", super::api_director::random_seed());
    let body = rh::multipart(&boundary, &[("apiKey", &c.key), ("fileType", "input")], "file", name, &bytes);
    let r = http()
        .post(format!("{}/task/openapi/upload", rh::base(&c.site)))
        .bearer_auth(&c.key)
        .header(reqwest::header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
        .body(body)
        .timeout(Duration::from_secs(600))
        .send()
        .await
        .map_err(|e| format!("上傳素材到 RunningHub 失敗：{e}"))?;
    let v: Value = r.json().await.map_err(|e| format!("上傳素材到 RunningHub：回應看不懂（{e}）"))?;
    let code = v["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        return Err(rh::err_msg(code, v["msg"].as_str().unwrap_or("")));
    }
    v["data"]["fileName"].as_str().filter(|s| !s.is_empty()).map(str::to_string).ok_or_else(|| "RunningHub 上傳完沒回檔名".into())
}

/// 一個成品：(放哪個子資料夾, 檔名, 內容)；子資料夾是空的＝主成品（影片），AnimaWeb/_poster＝封面
pub type Fetched = (String, String, Vec<u8>);

/// 送一份節點圖上 RunningHub 跑完、把成品抓回來（不存檔）。job＝(工作編號前面要記進工作清單的 params, info)，長片不記
pub async fn run_files(
    app: &Shared,
    nodes: Map<String, Value>,
    job: Option<(&Value, &Value)>,
    token: &CancellationToken,
    on_status: &(dyn Fn(String) + Send + Sync),
) -> Result<(String, Vec<Fetched>), String> {
    run_files_in(app, nodes, job, token, on_status, None).await
}

/// 同 run_files；wid＝建任務時帶的 workflowId（下載中心「在 RunningHub 跑」帶那個工作流自己的編號），None＝設定裡的
pub async fn run_files_in(
    app: &Shared,
    mut nodes: Map<String, Value>,
    job: Option<(&Value, &Value)>,
    token: &CancellationToken,
    on_status: &(dyn Fn(String) + Send + Sync),
    wid: Option<&str>,
) -> Result<(String, Vec<Fetched>), String> {
    let c = cfg(app);
    if c.key.is_empty() {
        return Err("還沒填 RunningHub 的 API Key".into());
    }
    let files = rh::input_files(&nodes);
    let mut map = HashMap::new();
    for (k, f) in files.iter().enumerate() {
        if token.is_cancelled() {
            return Err("已停止".into());
        }
        on_status(format!("上傳素材到 RunningHub {}／{}…", k + 1, files.len()));
        let local = app.paths.input.join(f);
        if !local.is_file() {
            return Err(format!("素材 {f} 不見了：重新放一次"));
        }
        map.insert(f.clone(), upload(&c, &local).await?);
    }
    rh::rewrite(&mut nodes, &map, &c.models);
    on_status("送到 RunningHub…".into());
    let created = post_json(&c, "/task/openapi/create", &rh::create_body(&c.key, &nodes, c.instance, wid.unwrap_or(&c.workflow_id))).await?;
    let task = rh::parse_create(&created)?;
    let jid = format!("rh-{task}");
    if let Some((params, info)) = job {
        app.db.add_job(&jid, params, info).map_err(|e| e.to_string())?;
    }
    let fail = |e: &str| {
        if job.is_some() {
            let _ = app.db.set_job(&jid, "error", Some(e));
        }
    };
    let t0 = Instant::now();
    let outs = loop {
        if token.is_cancelled() {
            let _ = post_json(&c, "/task/openapi/cancel", &json!({"apiKey": c.key, "taskId": task})).await;
            fail("已停止");
            return Err("已停止".into());
        }
        if t0.elapsed() > MAX_WAIT {
            fail("等太久");
            return Err(format!("RunningHub 跑超過 {} 分鐘還沒好：任務編號 {task}，可以到 RunningHub 網站的任務頁看", MAX_WAIT.as_secs() / 60));
        }
        tokio::time::sleep(POLL).await;
        let v = match post_json(&c, "/task/openapi/outputs", &json!({"apiKey": c.key, "taskId": task})).await {
            Ok(v) => v,
            // 網路抖一下：下一輪再問
            Err(e) => {
                on_status(format!("問 RunningHub 進度失敗，等一下再問：{e}"));
                continue;
            }
        };
        match rh::parse_outputs(&v) {
            rh::Poll::Waiting(m) => on_status(format!("{m}（{} 秒，約 {} RH 幣）", t0.elapsed().as_secs(), rh::coins_for(c.instance, t0.elapsed().as_secs_f64()))),
            rh::Poll::Done(o) => break o,
            rh::Poll::Failed(e) => {
                fail(&e);
                return Err(e);
            }
        }
    };
    on_status("下載成品…".into());
    let mut got = vec![];
    for o in &outs {
        let (sub, base) = rh::local_name(&nodes, o);
        let bytes = http()
            .get(&o.url)
            .timeout(Duration::from_secs(900))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("下載 RunningHub 的成品失敗：{e}"))?
            .bytes()
            .await
            .map_err(|e| format!("下載 RunningHub 的成品失敗：{e}"))?;
        got.push((sub, base, bytes.to_vec()));
    }
    Ok((jid, got))
}

/// 送一份節點圖上 RunningHub 跑完，成品存回本機（封面放 _poster、影片進圖庫）→ (工作編號, 圖庫編號)
pub async fn run_graph(
    app: &Shared,
    nodes: Map<String, Value>,
    params: &Value,
    info: &Value,
    token: &CancellationToken,
    on_status: &(dyn Fn(String) + Send + Sync),
) -> Result<(String, Vec<i64>), String> {
    run_graph_in(app, nodes, params, info, token, on_status, None).await
}

/// 同 run_graph；wid 見 run_files_in
pub async fn run_graph_in(
    app: &Shared,
    nodes: Map<String, Value>,
    params: &Value,
    info: &Value,
    token: &CancellationToken,
    on_status: &(dyn Fn(String) + Send + Sync),
    wid: Option<&str>,
) -> Result<(String, Vec<i64>), String> {
    let (job, got) = run_files_in(app, nodes, Some((params, info)), token, on_status, wid).await?;
    let mut ids = vec![];
    for (sub, base, bytes) in &got {
        ids.extend(super::api_cloud::keep_output(app, sub, base, bytes, params, info, &job)?);
    }
    let _ = app.db.set_job(&job, "done", None);
    Ok((job, ids))
}
