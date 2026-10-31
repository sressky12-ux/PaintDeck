//! 生圖工作流組（image_wf.rs）的 API：列出、匯入、刪除、選項、參數欄位、匯出，還有「取代內建功能」的對照（feature_flows）。
//! 生成頁用工作流組、取代內建功能時怎麼組圖在 api_gen.rs（submit_jobs）。
use super::{bad, blocking, internal, ok, ApiError, ApiResult, Body, Shared};
use crate::image_wf::{self, Meta, MODES};
use crate::paths::Paths;
use crate::{families, settings, wf_import};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;

// ---------------------------------------------------------------- 模型家族
/// 主模型檔所在的資料夾：diffusion_models（UNET）、checkpoints（整包模型），都含外部模型資料夾
pub fn model_roots(p: &Paths) -> Vec<PathBuf> {
    let mut v = p.unet_roots();
    v.push(p.comfy_root.join("models").join("checkpoints"));
    v.extend(crate::model_dirs::roots("checkpoints"));
    v
}

/// 家族：找得到主模型檔就看檔頭；找不到（別人的工作流、檔案不在這台電腦）或看不出來，就用存檔裡記的
fn pick_family(roots: &[PathBuf], file: Option<&str>, stored: &str) -> String {
    let fam = file.map_or(families::OTHER, |f| families::family_of(roots, f));
    if fam != families::OTHER || stored.is_empty() {
        fam.to_string()
    } else {
        stored.to_string()
    }
}

/// 工作流組（image_wf::load 讀出來的）的模型家族
pub fn set_family(p: &Paths, set: &Value) -> String {
    pick_family(&model_roots(p), image_wf::model_file(set).as_deref(), set["family"].as_str().unwrap_or(""))
}

/// image_wf::list 加上這台電腦實際偵測到的家族
pub fn list_sets(p: &Paths) -> Vec<Value> {
    let roots = model_roots(p);
    image_wf::list(&image_wf::dir(&p.data))
        .into_iter()
        .map(|mut e| {
            e["family"] = json!(pick_family(&roots, e["model_file"].as_str(), e["family"].as_str().unwrap_or("")));
            e
        })
        .collect()
}

// ---------------------------------------------------------------- 下載檔名
/// Content-Disposition：ASCII 備用檔名＋UTF-8 檔名（中文代號放得進去）
pub fn attachment(name: &str) -> String {
    let fallback: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).collect();
    let enc: String = name
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{enc}")
}

pub fn json_download(name: &str, v: &Value) -> ApiResult {
    let text = serde_json::to_string_pretty(v).map_err(internal)?;
    Ok(([(header::CONTENT_TYPE, "application/json; charset=utf-8".to_string()), (header::CONTENT_DISPOSITION, attachment(name))], text).into_response())
}

/// 存成 JSON 檔：跳 Windows 的「另存新檔」讓使用者選位置，後端寫檔 → 存好的完整路徑；按取消 → None。
/// dir：從這個資料夾開始（網頁記著上次存的地方）
pub async fn json_save(title: &str, name: &str, v: &Value, dir: Option<&str>) -> Result<Option<String>, ApiError> {
    let text = serde_json::to_string_pretty(v).map_err(internal)?;
    let (title, name) = (title.to_string(), name.to_string());
    // 網路路徑不拿來當起點（is_dir 會去連那台電腦）
    let start = dir.map(str::trim).filter(|d| !d.is_empty() && !is_unc(d)).map(PathBuf::from);
    let picked = blocking(move || crate::setup::save_json_dialog(&title, &name, start.as_deref())).await;
    let Some(path) = picked else {
        return Ok(None);
    };
    let path = with_json_ext(path);
    let p2 = path.clone();
    blocking(move || std::fs::write(&p2, text)).await.map_err(|e| bad(format!("存不進去：{e}")))?;
    Ok(Some(path.to_string_lossy().to_string()))
}

/// 副檔名不是 .json 就接在後面（「my.flow」→「my.flow.json」），ComfyUI 和「打開資料夾」才認得
fn with_json_ext(path: PathBuf) -> PathBuf {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) {
        return path;
    }
    let mut s = path.into_os_string();
    s.push(".json");
    PathBuf::from(s)
}

/// \\電腦\分享 這種網路路徑
fn is_unc(p: &str) -> bool {
    p.starts_with(r"\\") || p.starts_with("//")
}

/// POST /api/reveal {"path"}：打開檔案總管、選取這個檔（剛存好的工作流）。只收存在的 .json 檔（本機的完整路徑）
pub async fn reveal(Body(body): Body) -> ApiResult {
    let raw = body.get("path").and_then(Value::as_str).unwrap_or("").trim();
    let path = PathBuf::from(raw);
    let is_json = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"));
    if !is_json || is_unc(raw) || !path.is_absolute() || !path.is_file() {
        return Err(bad("找不到這個檔，可能被搬走或刪掉了"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // 路徑有空白時 /select, 後面要整段加引號，不能交給 Command 自己加（explorer 認不得）
        std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .map_err(|e| bad(format!("打不開檔案總管：{e}")))?;
    }
    ok(json!({"ok": true}))
}

fn offline(e: crate::comfy::ComfyError) -> ApiError {
    ApiError(StatusCode::SERVICE_UNAVAILABLE, e.0)
}

// ---------------------------------------------------------------- 列出
/// {"workflows": [{id, name, added, family, source, options, modes, model_file, summary: {<模式>: …}, errors: {<模式>: 原因}, info|error}],
///  "prefix": "wf:", "modes": [{id, name}], "feature_flows": {"<家族>.<模式>": 代號}}
pub async fn list(State(app): State<Shared>) -> ApiResult {
    let p = app.paths.clone();
    let workflows = blocking(move || list_sets(&p)).await;
    let flows = settings::load(&app.paths.settings).get("feature_flows").cloned().unwrap_or_else(|| json!({}));
    let modes: Vec<Value> = MODES.iter().map(|m| json!({"id": m, "name": image_wf::mode_name(m)})).collect();
    ok(json!({"workflows": workflows, "prefix": image_wf::PREFIX, "modes": modes, "feature_flows": flows}))
}

// ---------------------------------------------------------------- 匯入
/// 看起來是 JSON 物件就解析（生圖站自己匯出的工作流組檔要在轉換前認出來）
/// 網頁送來的檔案內容（可以帶 data:…;base64, 開頭）
pub fn decode_b64(b64: &str) -> Result<Vec<u8>, ApiError> {
    let b64 = b64.split_once("base64,").map_or(b64, |(_, rest)| rest).trim();
    base64::engine::general_purpose::STANDARD.decode(b64).map_err(|_| bad("檔案內容不是 base64"))
}

/// 檔案（API／UI 格式的 JSON、ComfyUI 生成的 PNG／WebP）→ API 格式的節點圖（wf_import）。
/// 一般「儲存」的工作流（UI 格式）要靠 ComfyUI 的 /object_info 才轉得了：要用到才讀。影片工作流的匯入也用這個
pub async fn file_to_api(app: &Shared, file_name: &str, bytes: Vec<u8>) -> Result<wf_import::Imported, ApiError> {
    let (b, f) = (bytes.clone(), file_name.to_string());
    match blocking(move || wf_import::to_api(&b, &f, &Value::Null)).await {
        Ok(i) => Ok(i),
        Err(e) if e.contains("/object_info") => {
            let oi = app.comfy.object_info_all().await.map_err(offline)?;
            let f = file_name.to_string();
            blocking(move || wf_import::to_api(&bytes, &f, &oi)).await.map_err(bad)
        }
        Err(e) => Err(bad(e)),
    }
}

fn parse_object(bytes: &[u8]) -> Option<Value> {
    let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    if body.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return None;
    }
    serde_json::from_slice(body).ok()
}

/// 整組匯入（生圖站匯出的工作流組檔）：每個模式各存一次，看不懂的模式跳過並說明。回傳 (最後一筆, 警告)
fn import_set(p: &Paths, name: &str, set: &Value) -> Result<(Value, Vec<String>), String> {
    let set = image_wf::normalize(set.clone());
    let name = if name.is_empty() { set["name"].as_str().unwrap_or("").to_string() } else { name.to_string() };
    let roots = model_roots(p);
    let dir = image_wf::dir(&p.data);
    let (mut last, mut warns) = (None, vec![]);
    for mode in image_wf::modes_of(&set) {
        let g = &set["graphs"][mode];
        let meta = Meta {
            family: Some(pick_family(&roots, image_wf::graph_model_file(g, mode).as_deref(), set["family"].as_str().unwrap_or(""))).filter(|f| f != families::OTHER),
            source: set["source"].as_str().map(str::to_string),
        };
        match image_wf::save(&dir, &name, None, mode, g, &meta) {
            Ok(e) => last = Some(e),
            Err(e) => warns.push(format!("{}：{e}", image_wf::mode_name(mode))),
        }
    }
    let mut entry = last.ok_or_else(|| warns.first().cloned().unwrap_or_else(|| "這個工作流組裡沒有任何工作流".into()))?;
    if set["options"]["page_sampler"] == json!(true) {
        entry = image_wf::set_options(&dir, entry["id"].as_str().unwrap_or(""), true)?;
    }
    Ok((entry, warns))
}

/// 匯入一個模式的工作流。body：
/// - name：工作流組的名字（set 有給就不用）；mode：txt2img｜img2img｜inpaint｜upscale｜multiref（預設 txt2img）
/// - set：加進這個既有的組（代號）；沒給就用名字，同名的組已經存在就加進去、同一個模式換掉，其他模式保留
/// - graph：API 格式的節點圖；或 file_name＋data_b64：檔案內容（API／UI 格式的 JSON、ComfyUI 生成的 PNG／WebP，由 wf_import 轉成 API 格式）
/// - family、source（"import"｜"builtin"）：選填
/// - marks：上次匯入失敗後，使用者在網頁上指定的節點（wf_import::apply_marks 的格式：{"6": "{{image}}", "12.text": "{{prompt}}"}）
///
/// 生圖站自己匯出的工作流組檔（有 graphs）也收：整組匯入。
/// mode 是 auto（或沒給）＝自動判斷：生圖站匯出的檔照 Save Image 標題的 {{mode:…}}（image_wf::mode_mark），沒有就當文生圖。
/// 沒有 {{mode:…}} 又沒選模式時，看節點圖猜（wf_import::guess_mode），猜不出來當文生圖；都會寫進 warnings。
/// 回傳 {"ok", "workflow": 一筆（同 list）, "warnings": [轉換時的提醒], "source": "api"|"ui"|"png"|"webp"|"set", "mode": 收成哪個模式（整組匯入是 null）}。
/// 分析失敗、而且認得出缺的是哪一種時，400 多帶 {"need": "prompt"|"image"|"mask"|"ref1"|"model"|"sampler", "candidates": wf_import::candidates, "mode"}，
/// 網頁讓使用者選節點、帶 marks 再送一次
pub async fn import(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let text = |k: &str| body.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
    let picked = Some(text("mode")).filter(|m| !m.is_empty() && m != "auto");
    let mut mode = picked.clone().unwrap_or_else(|| "txt2img".into());
    if !MODES.contains(&mode.as_str()) {
        return Err(bad(format!("模式只能是這幾種：自動判斷、{}", MODES.join("、"))));
    }
    let file_name = text("file_name");
    let set_id = Some(text("set")).filter(|s| !s.is_empty());
    let mut name = text("name");
    if name.is_empty() && set_id.is_none() {
        name = std::path::Path::new(&file_name).file_stem().map(|s| s.to_string_lossy().trim().to_string()).unwrap_or_default();
    }

    // 取得 API 格式的節點圖（或整組）
    let (mut graph, mut warnings, source): (Value, Vec<String>, &str) = if let Some(g) = body.get("graph").cloned() {
        (g, vec![], "api")
    } else if let Some(b64) = body.get("data_b64").and_then(Value::as_str) {
        let bytes = decode_b64(b64)?;
        if let Some(v) = parse_object(&bytes).filter(image_wf::is_set_file) {
            (v, vec![], "set")
        } else {
            let imported = file_to_api(&app, &file_name, bytes).await?;
            (imported.graph, imported.warnings, imported.source)
        }
    } else {
        return Err(bad("送來的資料格式不對：要有 graph，或 file_name 加 data_b64"));
    };

    let is_set = image_wf::is_set_file(&graph);
    // 上次匯入失敗後使用者指定的節點：先寫進節點圖，後面的分析照標記走
    if !is_set {
        let notes = wf_import::apply_marks(&mut graph, &body["marks"]).map_err(bad)?;
        if !notes.is_empty() {
            warnings.push(format!("照你指定的：{}", notes.join("；")));
        }
    }
    // 生圖站「拿去 ComfyUI 改」匯出的：認得出是哪個模式
    let marked = if is_set { None } else { image_wf::mode_mark(&graph) };
    match (marked, &picked) {
        (Some(m), None) => mode = m.to_string(),
        (None, None) if !is_set => match wf_import::guess_mode(&graph) {
            Some((m, why)) => {
                mode = m.to_string();
                warnings.push(format!(
                    "看不出是哪個功能（沒有生圖站的模式標記）：{why}，當成「{}」收。不對的話，把這個模式移除，選對模式再匯入一次",
                    image_wf::mode_name(m)
                ));
            }
            None => warnings.push("看不出是哪個功能（沒有生圖站的模式標記），當成文生圖收。不對的話，把這個模式移除，選對模式再匯入一次".into()),
        },
        (Some(m), Some(p)) if m != p => warnings.push(format!(
            "這個檔是生圖站匯出的「{}」，你選了「{}」：照你選的收。選錯的話，把這個模式移除，模式選「自動判斷」再匯入一次",
            image_wf::mode_name(m),
            image_wf::mode_name(p)
        )),
        _ => {}
    }
    let p = app.paths.clone();
    // 家族：自己指定的 → 生圖站匯出時記的 {{family:…}}（放大沒有主模型，只能靠它）；找得到主模型檔時還是以檔頭為準（pick_family）
    let family = Some(text("family")).filter(|f| !f.is_empty()).or_else(|| if is_set { None } else { image_wf::family_mark(&graph) });
    let src = Some(text("source")).filter(|s| !s.is_empty()).or_else(|| marked.map(|_| "builtin".to_string()));
    let mode2 = mode.clone();
    // 分析失敗時要列「可以選的節點」：先留一份節點圖
    let probe = (!is_set).then(|| graph.clone());
    let saved = blocking(move || -> Result<(Value, Vec<String>), String> {
        let mode = mode2;
        if image_wf::is_set_file(&graph) {
            return import_set(&p, &name, &graph);
        }
        let roots = model_roots(&p);
        let stored = family.clone().unwrap_or_default();
        let fam = pick_family(&roots, image_wf::graph_model_file(&graph, &mode).as_deref(), &stored);
        let meta = Meta { family: Some(fam).filter(|f| f != families::OTHER).or(family), source: src };
        image_wf::save(&image_wf::dir(&p.data), &name, set_id.as_deref(), &mode, &graph, &meta).map(|e| (e, vec![]))
    })
    .await;
    let (mut entry, warns2) = match saved {
        Ok(v) => v,
        Err(e) => {
            // 分析失敗（找不到提示詞、輸入圖…）：連同可以選的節點回給網頁，讓使用者自己指定
            let need = wf_import::need_of(&e).filter(|_| probe.as_ref().is_some_and(|g| image_wf::analyze(g, &mode).is_err()));
            return match (need, probe) {
                (Some(need), Some(g)) => Ok((
                    StatusCode::BAD_REQUEST,
                    axum::Json(json!({"error": e, "need": need, "candidates": wf_import::candidates(&g), "mode": mode})),
                )
                    .into_response()),
                _ => Err(bad(e)),
            };
        }
    };
    let roots = model_roots(&app.paths);
    entry["family"] = json!(pick_family(&roots, entry["model_file"].as_str(), entry["family"].as_str().unwrap_or("")));
    let warnings: Vec<String> = warnings.into_iter().chain(warns2).collect();
    // 用到、這台電腦沒有的模型：網頁每個給一顆「到下載中心找」（整組匯入不看）
    let missing = match probe {
        Some(g) => {
            let a = app.clone();
            blocking(move || super::api_hub::missing_models(&a, &g)).await
        }
        None => vec![],
    };
    ok(json!({"ok": true, "workflow": entry, "warnings": warnings, "source": source, "mode": if is_set { Value::Null } else { json!(mode) }, "missing_models": missing}))
}

// ---------------------------------------------------------------- 選項、刪除
/// {"page_sampler": bool}：「用生成頁的步數、CFG、取樣器…」；{"prompt_mode": "replace"|"append"}：沒標 {{prompt}} 時整格換掉、或接在工作流原本的文字後面。
/// 沒給的不動
pub async fn set_options(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Body(body): Body) -> ApiResult {
    let on = body.get("page_sampler").map(|v| crate::py::truthy(Some(v)));
    let pm = body.get("prompt_mode").and_then(Value::as_str).map(str::to_string);
    let dir = image_wf::dir(&app.paths.data);
    let entry = blocking(move || image_wf::set_options_all(&dir, &id, on, pm.as_deref())).await.map_err(bad)?;
    ok(json!({"ok": true, "workflow": entry}))
}

/// DELETE /api/image/workflows/{id}?mode=inpaint：刪一個模式（最後一個就整組刪）；沒給 mode 整組刪。
/// 指向被刪掉的圖的「取代內建功能」一起拿掉
pub async fn remove(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let mode = q.get("mode").map(String::as_str).filter(|m| !m.is_empty());
    if let Some(m) = mode.filter(|m| !MODES.contains(m)) {
        return Err(bad(format!("模式只能是這幾種：{}（送來的是 {m}）", MODES.join("、"))));
    }
    let _g = app.settings_lock.lock().await;
    let dir = image_wf::dir(&app.paths.data);
    let (id2, mode2) = (id.clone(), mode.map(str::to_string));
    let whole = blocking(move || image_wf::remove(&dir, &id2, mode2.as_deref())).await.map_err(bad)?;
    let mut s = settings::load(&app.paths.settings);
    if let Some(flows) = s.get_mut("feature_flows").and_then(Value::as_object_mut) {
        let before = flows.len();
        flows.retain(|k, v| {
            let hit = v.as_str() == Some(id.as_str()) && (whole || mode.is_some_and(|m| k.ends_with(&format!(".{m}"))));
            !hit
        });
        if flows.len() != before {
            settings::save(&app.paths.settings, &s).map_err(internal)?;
        }
    }
    ok(json!({"ok": true, "removed_set": whole}))
}

// ---------------------------------------------------------------- 參數欄位
/// GET /api/image/workflows/{id}/fields?mode=…（沒給 mode 用第一個模式）：
/// {"id", "name", "mode", "page_sampler", "fields": [{key: "<節點>.<欄位>", node, class, title, input, value, type, group: common|advanced,
///  auto, missing, node_missing, min?, max?, step?, default?, multiline?, options?}]}
/// 要靠 ComfyUI 的 /object_info；ComfyUI 沒開回 503
pub async fn fields(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let dir = image_wf::dir(&app.paths.data);
    let id2 = id.clone();
    let set = blocking(move || image_wf::load(&dir, &id2)).await.map_err(bad)?;
    let mode = match q.get("mode").filter(|m| !m.is_empty()) {
        Some(m) => m.clone(),
        None => image_wf::modes_of(&set).first().map(|m| m.to_string()).ok_or_else(|| bad("這個工作流組裡沒有任何工作流"))?,
    };
    if !image_wf::has_mode(&set, &mode) {
        return Err(bad(format!("工作流「{id}」沒有{}的工作流", image_wf::mode_name(&mode))));
    }
    let oi = app.comfy.object_info_all().await.map_err(offline)?;
    let (graph, m2) = (set["graphs"][&mode].clone(), mode.clone());
    let fields = blocking(move || image_wf::fields(&graph, &m2, &oi)).await;
    ok(json!({"id": id, "name": set["name"], "mode": mode, "page_sampler": set["options"]["page_sampler"], "fields": fields}))
}

// ---------------------------------------------------------------- 匯出
/// GET /api/image/workflows/{id}/export：整組的 JSON（下載），之後可以用匯入收回來
pub async fn export(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult {
    let dir = image_wf::dir(&app.paths.data);
    let id2 = id.clone();
    let set = blocking(move || image_wf::load(&dir, &id2)).await.map_err(bad)?;
    json_download(&format!("anima_workflow_{id}.json"), &set)
}

/// POST /api/image/workflows/{id}/export {"dir"?: 從哪個資料夾開始}：同上，改跳「另存新檔」讓使用者選位置（json_save）
/// → {"ok", "path"}｜{"cancelled": true}。會跳視窗，所以用 POST（要過同源檢查，別的網站用 <img> 之類的 GET 觸發不了）
pub async fn export_save(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Body(body): Body) -> ApiResult {
    let dir = image_wf::dir(&app.paths.data);
    let id2 = id.clone();
    let set = blocking(move || image_wf::load(&dir, &id2)).await.map_err(bad)?;
    let name = format!("anima_workflow_{id}.json");
    match json_save("匯出工作流組", &name, &set, body.get("dir").and_then(Value::as_str)).await? {
        Some(path) => ok(json!({"ok": true, "path": path})),
        None => ok(json!({"cancelled": true})),
    }
}

// ---------------------------------------------------------------- 取代內建功能
/// GET /api/feature_flows → {"feature_flows": {"<家族>.<模式>": 工作流組代號}}
pub async fn flows_get(State(app): State<Shared>) -> ApiResult {
    let flows = settings::load(&app.paths.settings).get("feature_flows").cloned().unwrap_or_else(|| json!({}));
    ok(json!({"feature_flows": flows}))
}

/// POST /api/feature_flows {"key": "anima.inpaint", "id": "代號" | null}：id 給 null（或空字串）＝切回內建
pub async fn flows_set(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let key = body.get("key").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if !image_wf::flow_key_ok(&key) {
        return Err(bad(format!("鍵要寫成「家族.模式」，例如 anima.inpaint；模式只能是 {}", MODES.join("、"))));
    }
    let Some(id) = body.get("id") else {
        return Err(bad("送來的資料格式不對：缺少 id（要切回內建就給 null）"));
    };
    let id = match id {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        _ => return Err(bad("送來的資料格式不對：id")),
    };
    if !id.is_empty() {
        let mode = key.split_once('.').map_or("", |(_, m)| m).to_string();
        let (dir, id2) = (image_wf::dir(&app.paths.data), id.clone());
        let set = blocking(move || image_wf::load(&dir, &id2)).await.map_err(bad)?;
        if !image_wf::has_mode(&set, &mode) {
            return Err(bad(format!("工作流「{id}」沒有{}的工作流，沒辦法拿來取代這個功能", image_wf::mode_name(&mode))));
        }
    }
    let _g = app.settings_lock.lock().await;
    let mut s = settings::load(&app.paths.settings);
    let mut flows = s.get("feature_flows").and_then(Value::as_object).cloned().unwrap_or_default();
    if id.is_empty() {
        flows.shift_remove(&key);
    } else {
        flows.insert(key, json!(id));
    }
    s.insert("feature_flows".into(), Value::Object(flows.clone()));
    settings::save(&app.paths.settings, &s).map_err(internal)?;
    ok(json!({"ok": true, "feature_flows": flows}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_name_is_header_safe() {
        let d = attachment("anima_workflow_我的 SDXL.json");
        assert!(d.is_ascii(), "{d}");
        assert!(d.starts_with("attachment; filename=\"anima_workflow___SDXL.json\"") || d.contains("filename*=UTF-8''anima_workflow_%E6%88%91"), "{d}");
        assert!(d.contains("%20SDXL"), "{d}");
        assert!(axum::http::HeaderValue::from_str(&d).is_ok());
    }

    #[test]
    fn family_prefers_header_then_stored() {
        // 檔案不在這台電腦：用存檔裡記的；記的是空的就是 other
        assert_eq!(pick_family(&[], Some("nope.safetensors"), "sdxl"), "sdxl");
        assert_eq!(pick_family(&[], Some("nope.safetensors"), ""), families::OTHER);
        assert_eq!(pick_family(&[], None, "flux"), "flux");
    }

    fn tiny_graph() -> Value {
        json!({
            "4": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "sdxl.safetensors"}},
            "5": {"class_type": "EmptyLatentImage", "inputs": {"width": 1024, "height": 1024, "batch_size": 4}},
            "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "a cat", "clip": ["4", 1]}},
            "3": {"class_type": "KSampler", "inputs": {"seed": 1, "steps": 20, "cfg": 7, "model": ["4", 0], "positive": ["6", 0], "negative": ["6", 0], "latent_image": ["5", 0]}},
            "8": {"class_type": "VAEDecode", "inputs": {"samples": ["3", 0], "vae": ["4", 2]}},
            "9": {"class_type": "SaveImage", "inputs": {"images": ["8", 0], "filename_prefix": "ComfyUI"}}
        })
    }

    /// 匯出的工作流組檔可以整組匯回來（換電腦）：每個模式都在、選項和來源保留、家族用存檔裡記的（這台沒有那個模型檔）
    #[test]
    fn set_file_round_trip() {
        let root = std::env::temp_dir().join(format!("anima_api_wf_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let p = Paths::new(&root.join("app"), &root.join("web"), Some(root.join("comfy")));
        let exported = json!({"name": "我的 SDXL", "added": 1.0, "family": "sdxl", "source": "builtin", "options": {"page_sampler": true},
                              "graphs": {"txt2img": tiny_graph(), "upscale": {"bad": 1}}});
        assert!(image_wf::is_set_file(&exported));
        let (entry, warns) = import_set(&p, "", &exported).unwrap();
        assert_eq!(entry["modes"], json!(["txt2img"]));
        assert_eq!(entry["source"], "builtin");
        assert_eq!(entry["options"]["page_sampler"], true);
        assert_eq!(warns.len(), 1, "{warns:?}");
        assert!(warns[0].starts_with("放大："), "{warns:?}");
        let sets = list_sets(&p);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0]["family"], "sdxl");
        let loaded = image_wf::load(&image_wf::dir(&p.data), "我的 SDXL").unwrap();
        assert_eq!(set_family(&p, &loaded), "sdxl");
        // 全部看不懂：整組不收，講原因
        let e = import_set(&p, "壞的", &json!({"graphs": {"txt2img": {"x": 1}}})).unwrap_err();
        assert!(!e.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn saved_names_end_in_json() {
        let j = |s: &str| with_json_ext(PathBuf::from(s)).to_string_lossy().to_string();
        assert_eq!(j(r"C:\a\x.json"), r"C:\a\x.json");
        assert_eq!(j(r"C:\a\x.JSON"), r"C:\a\x.JSON");
        assert_eq!(j(r"C:\a\x"), r"C:\a\x.json");
        assert_eq!(j(r"C:\a\my.flow"), r"C:\a\my.flow.json");
        assert!(is_unc(r"\\nas\share\x.json") && is_unc("//nas/share"));
        assert!(!is_unc(r"C:\x.json") && !is_unc(""));
    }

    #[test]
    fn plain_object_detection() {
        assert!(parse_object(b"\xEF\xBB\xBF {\"a\": 1}").is_some());
        assert!(parse_object(b"[1]").is_none());
        assert!(parse_object(b"\x89PNG\r\n").is_none());
    }
}
