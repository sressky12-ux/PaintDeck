//! 狀態、設定、生成、任務面板、系統狀態（對照舊版 server.py 同名的函式）。
use super::{api_bundle, api_wf, bad, blocking, internal, not_found, ok, ApiResult, Body, Shared, VERSION};
use crate::comfy::ComfyError;
use crate::families::{self, ANIMA, FAMILY_NAMES};
use crate::graphs;
use crate::py::{self, now, truthy};
use crate::settings;
use crate::{image_wf, video_wf};
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

// ---------------------------------------------------------------- 狀態
pub async fn state(State(app): State<Shared>) -> ApiResult {
    let live = app.comfy.live();
    let mut models = Map::new();
    if live.online {
        let c = &app.comfy;
        let wanted = [
            ("unets", "UNETLoader", "unet_name"),
            ("loras_all", "LoraLoaderModelOnly", "lora_name"),
            ("upscalers", "UpscaleModelLoader", "model_name"),
            ("samplers", "KSampler", "sampler_name"),
            ("schedulers", "KSampler", "scheduler"),
        ];
        for (k, node, field) in wanted {
            match c.options(node, field).await {
                Ok(v) => {
                    models.insert(k.into(), Value::Array(v));
                }
                Err(e) => {
                    models.insert("error".into(), json!(e.0));
                    break;
                }
            }
        }
    }
    let names = |k: &str| -> Vec<String> {
        models
            .get(k)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect()
    };
    let (unets, loras_all) = (names("unets"), names("loras_all"));
    let meta = app.db.lora_meta()?;
    let p = &app.paths;
    let (unet_roots, lora_roots) = (p.unet_roots(), p.lora_roots());
    let lfam: HashMap<&str, &str> = loras_all
        .iter()
        .map(|n| {
            (
                n.as_str(),
                families::lora_family(&lora_roots, n, meta.get(n)),
            )
        })
        .collect();
    let unet_family: Map<String, Value> = unets
        .iter()
        .map(|n| (n.clone(), json!(families::family_of(&unet_roots, n))))
        .collect();
    let turbo = |n: &str| graphs::is_turbo_lora(n) && lfam[n] == ANIMA;
    let loras: Vec<Value> = loras_all
        .iter()
        .filter(|n| !turbo(n))
        .map(|n| {
            let mut m = Map::new();
            m.insert("name".into(), json!(n));
            if let Some(Value::Object(x)) = meta.get(n) {
                m.extend(x.clone());
            }
            if !m.get("preview").and_then(Value::as_str).is_some_and(|name| app.paths.previews.join(super::basename(name)).is_file()) {
                m.remove("preview");
                if super::api_misc::local_lora_preview(&lora_roots, n).is_some() {
                    m.insert("local_preview".into(), json!(true));
                }
            }
            m.insert("family".into(), json!(lfam[n.as_str()]));
            Value::Object(m)
        })
        .collect();
    let turbo_loras: Vec<Value> = loras_all
        .iter()
        .filter(|n| turbo(n))
        .map(|n| {
            let m = meta.get(n);
            let g = |k: &str| m.and_then(|x| x.get(k)).cloned().unwrap_or(Value::Null);
            json!({"name": n, "title": g("title"), "version": g("version")})
        })
        .collect();
    // 加進來的外部模型資料夾，開著的 ComfyUI 還沒讀到（沒重開、或 ComfyUI 不是生圖站開的）：網頁在生成頁提醒
    let pending = crate::model_dirs::pending_restart();
    if !pending.is_empty() {
        // 同一個資料夾只列一次（整包模型資料夾同時也算主模型資料夾，見 model_dirs::effective）
        let mut seen: Vec<&std::path::Path> = vec![];
        let mut list: Vec<Value> = vec![];
        for d in &pending {
            if seen.iter().any(|s| crate::model_dirs::same(s, &d.path)) {
                continue;
            }
            seen.push(&d.path);
            list.push(json!({"kind": d.kind, "label": crate::model_dirs::kind_label(&d.kind), "path": d.path.to_string_lossy()}));
        }
        models.insert("dirs_pending".into(), json!({"list": list, "can_restart": app.restart_comfy.get().is_some()}));
        models.insert("comfy_updated".into(), json!({"list": crate::setup::updated(), "can_restart": app.restart_comfy.get().is_some()}));
    }
    models.insert("unet_family".into(), Value::Object(unet_family));
    models.insert(
        "family_names".into(),
        Value::Object(
            FAMILY_NAMES
                .iter()
                .map(|(k, v)| (k.to_string(), json!(v)))
                .collect(),
        ),
    );
    models.insert("loras".into(), json!(loras));
    models.insert("turbo_loras".into(), json!(turbo_loras));
    // 匯入的生圖工作流組（生成頁模型選單最後面「⚙ 工作流：…」，值是 wf:<代號>）。modes＝它有哪些模式（模式鍵只亮這些）、
    // family＝這台電腦偵測到的模型家族（LoRA 照它篩）
    let paths = p.clone();
    let flows: Vec<Value> = blocking(move || api_wf::list_sets(&paths))
        .await
        .into_iter()
        .filter(|w| w.get("error").is_none())
        .map(|w| {
            json!({"id": format!("{}{}", image_wf::PREFIX, w["id"].as_str().unwrap_or("")), "name": w["name"],
                   "modes": w["modes"], "family": w["family"], "options": w["options"]})
        })
        .collect();
    models.insert("image_workflows".into(), json!(flows));
    let s = settings::load(&p.settings);
    let presets: Vec<Value> = app.db.presets()?.into_iter().map(Value::Object).collect();
    ok(json!({
        "online": live.online, "remaining": live.queue_remaining, "running": live.running, "progress": live.progress,
        "models": models, "presets": presets, "tagger": app.tagger.available(),
        "settings": settings::public_view(&s), "pe": app.pe.status(), "version": VERSION,
    }))
}

// ---------------------------------------------------------------- 設定
/// 匯出所有客製化：設定、提示詞庫（連縮圖）、舊的預設組、LoRA 備註，加上生圖工作流組、影片工作流、改過的 AI 改寫系統提示詞。
/// 不含 Civitai API key 和任何金鑰、路徑（格式和放什麼、不放什麼見 api_bundle.rs）
pub async fn settings_export(State(app): State<Shared>) -> ApiResult {
    let s = api_bundle::export_settings(&settings::load(&app.paths.settings));
    let presets: Vec<Value> = app
        .db
        .presets()?
        .into_iter()
        .map(|p| json!({"name": p["name"], "data": p["data"]}))
        .collect();
    let mut data = json!({"app": "anima_ui", "version": VERSION, "exported": now(), "settings": s, "presets": presets, "prompt_lib": app.db.lib_export()?, "lora_meta": app.db.lora_meta()?});
    let dir = app.paths.data.clone();
    if let Some(m) = data.as_object_mut() {
        m.extend(blocking(move || api_bundle::export_parts(&dir)).await);
    }
    let fname = format!(
        "attachment; filename=anima_settings_{}.json",
        chrono::Local::now().format("%Y%m%d")
    );
    Ok((
        [
            (
                header::CONTENT_TYPE,
                "application/json; charset=utf-8".to_string(),
            ),
            (header::CONTENT_DISPOSITION, fname),
        ],
        settings::to_text(&data),
    )
        .into_response())
}

/// 匯入：設定只收認得的欄位、永遠不動 API key；預設組同名覆蓋；LoRA 備註合併；
/// 提示詞庫同類型、同資料夾、同名的覆蓋（舊版匯出的檔沒有提示詞庫：預設組也抄進提示詞庫）。
/// 生圖工作流組、影片工作流、自訂 AI 改寫提示詞：同代號內容不同，舊檔先搬進 待刪除\匯入前_<時間>\ 再寫新的，內容一樣略過
/// （細節在 api_bundle.rs）。回傳 {ok, presets, prompt_lib, lora_meta, image_workflows, video_workflows, pe_prompts,
/// unchanged: {…}, settings, feature_flows, backed_up, backup_dir, warnings: [...]}
pub async fn settings_import(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    if body.get("app").and_then(Value::as_str) != Some("anima_ui") {
        return Err(bad("這不是生圖站匯出的設定檔"));
    }
    let body = std::sync::Arc::new(body);
    let report = {
        // 設定檔、工作流檔「讀 → 改 → 寫」要排隊（刪工作流、改取代內建功能也走同一把鎖）
        let _g = app.settings_lock.lock().await;
        let (data, root, sp, b) = (app.paths.data.clone(), app.paths.root.clone(), app.paths.settings.clone(), body.clone());
        blocking(move || api_bundle::import_bundle(&data, &root, &sp, &b))
            .await
            .map_err(internal)?
    };
    let has_lib = body.get("prompt_lib").is_some_and(Value::is_array);
    let mut n_p = 0;
    for p in body
        .get("presets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let (Some(name), Some(data)) = (
            p.get("name").filter(|n| truthy(Some(n))),
            p.get("data").filter(|d| d.is_object()),
        ) {
            let name: String = name
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| name.to_string())
                .chars()
                .take(100)
                .collect();
            app.db.save_preset(&name, data)?;
            if !has_lib {
                super::api_lib::import_preset(&app.db, &name, data)?;
            }
            n_p += 1;
        }
    }
    let meta = app.db.lora_meta()?;
    let mut n_l = 0;
    for (name, m) in body
        .get("lora_meta")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if let Value::Object(m) = m {
            let mut merged = meta
                .get(name)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            merged.extend(m.clone());
            app.db.set_lora_meta(name, &Value::Object(merged))?;
            n_l += 1;
        }
    }
    let n_lib = {
        let _g = app.assets_lock.lock().await;
        super::api_lib::import(&app.db, body.get("prompt_lib"))?
    };
    let mut out = json!({"ok": true, "presets": n_p, "prompt_lib": n_lib, "lora_meta": n_l});
    if let (Some(o), Value::Object(r)) = (out.as_object_mut(), report.to_json()) {
        o.extend(r);
    }
    ok(out)
}

pub async fn set_settings(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let _g = app.settings_lock.lock().await;
    let mut s = settings::load(&app.paths.settings);
    for k in ["blur_nsfw", "negative"] {
        if let Some(v) = body.get(k) {
            s.insert(k.into(), v.clone());
        }
    }
    let fmt = || bad("設定值格式不對");
    if let Some(m) = body.get("pe_mode") {
        if !matches!(m.as_str(), Some("gpu" | "partial" | "cpu")) {
            return Err(bad("改寫執行方式只能是 gpu／partial／cpu"));
        }
        s.insert("pe_mode".into(), m.clone());
    }
    if let Some(m) = body.get("pe_general") {
        // auto／35b／9b，或直接指定 LLM 資料夾裡的某個 .gguf（只收檔名，不收路徑）
        let ok = match m.as_str() {
            Some("auto" | "35b" | "9b") => true,
            Some(f) => f.len() <= 255 && f.to_lowercase().ends_with(".gguf") && !f.contains(['/', '\\', ':']) && !f.contains(".."),
            None => false,
        };
        if !ok {
            return Err(bad("通用模型只能是 auto／35b／9b，或 LLM 資料夾裡某個 .gguf 的檔名"));
        }
        s.insert("pe_general".into(), m.clone());
    }
    // pe_engine（以前選官方 PE）2026-10-30 拿掉了：舊網頁送來就不理，存檔裡留著的舊值也清掉
    s.remove("pe_engine");
    if let Some(v) = body.get("pe_partial_layers") {
        s.insert(
            "pe_partial_layers".into(),
            json!(py::int(v).ok_or_else(fmt)?.clamp(1, 40)),
        );
    }
    if let Some(v) = body.get("pe_idle") {
        s.insert(
            "pe_idle".into(),
            json!(py::int(v).ok_or_else(fmt)?.clamp(10, 3600)),
        );
    }
    if let Some(v) = body.get("pe_temperature") {
        s.insert(
            "pe_temperature".into(),
            json!(py::float(v).ok_or_else(fmt)?.clamp(0.1, 1.5)),
        );
    }
    for k in ["pe_close_after", "pe_think", "pe_zh"] {
        if let Some(v) = body.get(k) {
            s.insert(k.into(), json!(truthy(Some(v))));
        }
    }
    match body.get(settings::KEY) {
        None | Some(Value::Null) => {}
        Some(Value::String(k)) => {
            if k.trim().is_empty() {
                settings::clear_key(&mut s);
            } else {
                s.insert(settings::KEY.into(), json!(k.trim()));
            }
        }
        Some(_) => return Err(fmt()),
    }
    // 影片模型：內建，或已經匯入的工作流
    if let Some(v) = body.get("video_engine") {
        let id = v.as_str().ok_or_else(fmt)?;
        if !video_wf::is_builtin(id) {
            video_wf::load(&video_wf::dir(&app.paths.data), id).map_err(bad)?;
        }
        s.insert("video_engine".into(), json!(id));
    }
    settings::save(&app.paths.settings, &s).map_err(internal)?;
    ok(json!({"ok": true}))
}

// ---------------------------------------------------------------- 生成
/// 一次最多送幾張（網頁最多 16）
const MAX_COUNT: i64 = 32;

/// 外部直接打 API 最常漏的參數：沒給不會報錯，只會畫出 256 寬、0 步、CFG 0 的圖。
/// 只管文生圖：網頁一定帶齊；其他模式的尺寸跟著輸入圖走，有的模式（去背、放大、差分）也不用步數
fn missing_txt2img(p: &Map<String, Value>) -> Vec<&'static str> {
    if p.get("mode").and_then(Value::as_str).unwrap_or("txt2img") != "txt2img" {
        return vec![];
    }
    ["width", "height", "steps", "cfg"]
        .into_iter()
        .filter(|k| p.get(*k).is_none_or(Value::is_null))
        .collect()
}

fn random_seed() -> i64 {
    rand::random_range(0..=(1i64 << 48))
}

/// 種子：沒給、空白、-1 都是隨機；其他要是整數
fn resolve_seed(p: &mut Map<String, Value>) -> Result<(), super::ApiError> {
    let seed = p.get("seed").cloned().unwrap_or(Value::Null);
    let random = match &seed {
        Value::Null => true,
        Value::String(s) => s.is_empty() || s == "-1",
        Value::Number(n) => n.as_f64() == Some(-1.0),
        _ => false,
    };
    let seed = if random {
        random_seed()
    } else {
        py::int(&seed).ok_or_else(|| bad("種子要是整數"))?
    };
    p.insert("seed".into(), json!(seed));
    Ok(())
}

pub async fn generate(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let Some(Value::Object(mut p)) = body.get("params").cloned() else {
        return Err(bad("送來的資料格式不對：缺少 params"));
    };
    let count = match body.get("count") {
        None => 1,
        Some(v) => py::int(v).ok_or_else(|| bad("送來的資料格式不對：count"))?,
    };
    if !(1..=MAX_COUNT).contains(&count) {
        return Err(bad(format!("count 要在 1～{MAX_COUNT} 之間（送來的是 {count}）")));
    }
    let missing = missing_txt2img(&p);
    if !missing.is_empty() {
        return Err(bad(format!("文生圖缺少參數：{}", missing.join("、"))));
    }
    resolve_seed(&mut p)?;
    submit(&app, p, count).await
}

/// 送出 count 張（每張種子 +1）。家族由伺服器看檔頭決定，不同家族的 LoRA 直接略過
pub async fn submit(app: &Shared, p: Map<String, Value>, count: i64) -> ApiResult {
    let jobs = submit_jobs(app, p, count).await?;
    ok(json!({"jobs": jobs}))
}

/// 這次要用的流程；沒有＝內建的 graphs::build
#[derive(Clone)]
enum Flow {
    /// 使用者匯入的影片工作流（video_wf.rs）
    Video(Value),
    /// 生圖工作流組（image_wf.rs）的某個模式：生成頁選了「⚙ 工作流」，或內建功能被它取代（settings 的 feature_flows）
    Image { set: Value, mode: String },
}

/// submit_jobs 送出前整理好的東西
struct Prepared {
    p: Map<String, Value>,
    video: bool,
    flow: Option<Flow>,
    /// 記進 last_family 的家族（工作流組＝custom）
    fam: String,
    /// 家族不合而略過的 LoRA 檔名
    skipped: Vec<String>,
}

/// 決定用哪個流程、家族，並照家族篩 LoRA。不碰 ComfyUI（匯出內建工作流也用它）
/// 影片要用哪個模型：請求指定的（重跑、放大成正式版）→ 設定裡選的
pub(crate) fn video_engine(paths: &crate::paths::Paths, p: &Map<String, Value>) -> String {
    let e = match p.get("video_engine").and_then(Value::as_str).filter(|e| !e.is_empty()) {
        Some(e) => e.to_string(),
        None => settings::load(&paths.settings).get("video_engine").and_then(Value::as_str).unwrap_or(video_wf::BUILTIN).to_string(),
    };
    video_wf::normalize_engine(&e).to_string()
}

fn prepare(app: &Shared, mut p: Map<String, Value>) -> Result<Prepared, super::ApiError> {
    let paths = &app.paths;
    // 這兩個每次重新判斷（重跑舊任務時 params 裡留著上一次的）
    p.shift_remove("image_workflow");
    p.shift_remove("wf_model_from_page");
    let mode = p.get("mode").and_then(Value::as_str).unwrap_or("txt2img").to_string();
    let video = mode == "video";
    let mut flow = None;
    // 影片模型：請求指定的（重跑、放大成正式版）→ 設定裡選的。記進 params，重跑時用同一個
    if video {
        let engine = video_engine(paths, &p);
        // 內建 H3 在 submit_jobs 就轉去 api_h3，不經過這裡（匯出內建工作流這類呼叫也不支援 H3）
        if engine == video_wf::BUILTIN {
            return Err(bad("內建影片（MiniMax H3）不經過生圖流程：從「動起來」或導演台送出"));
        }
        flow = Some(Flow::Video(video_wf::load(&video_wf::dir(&paths.data), &engine).map_err(bad)?));
        p.insert("video_engine".into(), json!(engine));
    }
    let model = p.get("model").and_then(Value::as_str).unwrap_or("").to_string();
    let fam: String;
    // 篩 LoRA 用的家族；None＝不篩（工作流組認不出家族）
    let lora_fam: Option<String>;
    let from_set = model.strip_prefix(image_wf::PREFIX).map(str::to_string);
    if let Some(id) = &from_set {
        // 生成頁模型選單選了「⚙ 工作流：…」：模型照工作流，模式限它有的
        let modes = || image_wf::MODES.map(image_wf::mode_name).join("、");
        if !image_wf::MODES.contains(&mode.as_str()) {
            return Err(bad(format!("匯入的工作流只能做{}：這個功能請換回一般模型", modes())));
        }
        let set = image_wf::load(&image_wf::dir(&paths.data), id).map_err(bad)?;
        if !image_wf::has_mode(&set, &mode) {
            let have: Vec<&str> = image_wf::modes_of(&set).into_iter().map(image_wf::mode_name).collect();
            return Err(bad(format!(
                "工作流「{}」沒有{}的工作流（只有：{}）。到「設定 → 模型與工作流」補一個，或換回一般模型",
                set["name"].as_str().unwrap_or(id),
                image_wf::mode_name(&mode),
                have.join("、")
            )));
        }
        lora_fam = Some(api_wf::set_family(paths, &set)).filter(|f| f != families::OTHER);
        p.insert("image_workflow".into(), json!(id));
        flow = Some(Flow::Image { set, mode: mode.clone() });
        fam = "custom".to_string();
    } else {
        if !video {
            if let Some(e) = families::missing_model(&paths.unet_roots(), &model) {
                return Err(bad(e));
            }
        }
        fam = families::family_of(&paths.unet_roots(), &model).to_string();
        lora_fam = Some(fam.clone());
        // 內建功能被使用者的工作流取代了（「取代內建功能」）：這個家族的這個模式有指定工作流組、而且組裡有這個模式
        if !video && flow.is_none() {
            let flows = settings::load(&paths.settings).get("feature_flows").cloned().unwrap_or(Value::Null);
            if let Some(id) = image_wf::pick_flow(&flows, &fam, &mode, truthy(p.get("builtin"))) {
                match image_wf::load(&image_wf::dir(&paths.data), id) {
                    Ok(set) if image_wf::has_mode(&set, &mode) => {
                        p.insert("image_workflow".into(), json!(id));
                        p.insert("wf_model_from_page".into(), json!(true));
                        flow = Some(Flow::Image { set, mode: mode.clone() });
                    }
                    // 工作流組被刪了、或那個模式被刪了：回到內建
                    _ => eprintln!("取代內建功能：找不到工作流組「{id}」的{}，改用內建的", image_wf::mode_name(&mode)),
                }
            }
        }
    }
    p.insert("family".into(), json!(fam));
    // 一鍵脫衣：改圖指令和精修 tag 是外部內容補丁（feature_strip），這裡照 target／keep 算好塞進 params，
    // graph 本身不含露骨文字；沒裝補丁就擋下。
    if mode == "strip" {
        let pack = crate::feature_strip::load(&paths.data, &paths.res).ok_or_else(|| {
            bad("還沒安裝「一鍵脫衣」的內容補丁。到「設定 → 外掛功能」匯入，或把 pack.json 放進 <資料>\\plugins\\strip\\")
        })?;
        let st = p.get("strip").cloned().unwrap_or_else(|| json!({}));
        let g = |k: &str, d: &str| st.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
        let (target, keep) = (g("target", "female"), g("keep", "all"));
        let mut stm = st.as_object().cloned().unwrap_or_default();
        stm.insert("qwen_prompt".into(), json!(pack.qwen_prompt(&target, &keep, &g("extra", ""))));
        stm.insert("refine_tags".into(), json!(pack.refine_tags(&target, &keep, &g("tags", ""))));
        p.insert("strip".into(), Value::Object(stm));
    }
    // LoRA 照家族篩。工作流組認不出家族就全留；認得出家族時，家族看不出來（other）的 LoRA 也留（可能只是檔頭格式沒見過）
    let lora_roots = paths.lora_roots();
    let lora_meta = app.db.lora_meta().unwrap_or_default();
    let (mut kept, mut skipped) = (vec![], vec![]);
    for l in p.get("loras").and_then(Value::as_array).into_iter().flatten() {
        let Some(name) = l["name"].as_str() else { continue };
        let fits = match &lora_fam {
            None => true,
            Some(f) => {
                let lf = families::lora_family(&lora_roots, name, lora_meta.get(name));
                lf == f || (from_set.is_some() && lf == families::OTHER)
            }
        };
        if fits {
            kept.push(l.clone());
        } else {
            skipped.push(name.to_string());
        }
    }
    p.insert("loras".into(), json!(kept));
    // 以前內建 LTX 的影片 LoRA 欄位（舊網頁、重跑舊任務會帶來）：LTX 退場，丟掉
    p.shift_remove("video_loras");
    Ok(Prepared { p, video, flow, fam, skipped })
}

/// 同 submit，回傳 [{"job": 任務編號, "seed": 種子}]（導演台自己等任務跑完）。
/// 有因為家族不合而略過的 LoRA，每個 job 多一個 skipped_loras（檔名清單）
pub async fn submit_jobs(
    app: &Shared,
    p: Map<String, Value>,
    count: i64,
) -> Result<Vec<Value>, super::ApiError> {
    // 影片用內建的 H3（請求指定、或設定選的；舊的 ltx23 也算）：換成 H3 參數排進本機 ComfyUI。匯入的影片工作流照舊走 prepare
    if p.get("mode").and_then(Value::as_str) == Some("video") && video_engine(&app.paths, &p) == video_wf::BUILTIN {
        return super::api_h3::submit_from_gen(app, &p, count).await;
    }
    if !app.comfy.live().online {
        return Err(bad("ComfyUI 沒有連線。解法：重新開啟「繪匣 PaintDeck」。"));
    }
    // 站剛開、還沒問過 ComfyUI 模型放哪些資料夾（每分鐘問一次）就找不到模型檔：先問一次再判斷家族（第一百九十一筆）
    let model = p.get("model").and_then(Value::as_str).unwrap_or("").to_string();
    if !model.starts_with(image_wf::PREFIX) && families::missing_model(&app.paths.unet_roots(), &model).is_some() {
        let u = app.comfy.url().to_string();
        let _ = blocking(move || crate::model_dirs::refresh_live(&u)).await;
    }
    let Prepared { p, video, flow, fam, skipped } = prepare(app, p)?;
    // AI 改寫的 llama-server 在 GPU 上會占掉 5～6GB，ComfyUI 會爆顯存：先關掉（正在改寫就等它寫完）
    app.pe.release_for_generation().await;
    if video {
        // 影片模型＋文字編碼器約 42GB，比 32GB 記憶體大：上一個任務留下的模型不清掉，取樣每步會慢 5 倍（2026-10-11 實測）。
        // /free 回傳是空的，失敗也照送（只是會比較慢）
        if let Err(e) = app
            .comfy
            .post(
                "/free",
                &json!({"unload_models": true, "free_memory": true}),
            )
            .await
        {
            eprintln!("影片前 ComfyUI /free：{}", e.0);
        }
    }
    let p = Value::Object(p);
    let mut jobs: Vec<Value> = vec![];
    for i in 0..count {
        match submit_one(app, &p, i, video, &fam, flow.as_ref()).await {
            Ok(j) => jobs.push(j),
            Err(e) => {
                // 送到一半失敗：前面已經排進 ComfyUI 的幾張也收回，免得網頁說失敗、背景卻還在畫（使用者再按一次就重複了）
                for j in &jobs {
                    let Some(pid) = j["job"].as_str() else { continue };
                    let _ = app.comfy.post("/queue", &json!({"delete": [pid]})).await;
                    let _ = app.db.set_job(pid, "cancelled", None);
                    app.hub
                        .send(json!({"type": "finish", "job": pid, "status": "cancelled"}));
                }
                return Err(e);
            }
        }
    }
    if !skipped.is_empty() {
        for j in &mut jobs {
            j["skipped_loras"] = json!(skipped);
        }
    }
    Ok(jobs)
}

/// 組圖失敗 → 回給網頁的錯誤（找不到輸入圖片的補上解法）
fn build_error(e: graphs::GraphError) -> super::ApiError {
    if e.0.starts_with("找不到輸入圖片：") {
        return bad(format!("{}。解法：重新放一次圖片（上傳暫存可能被清掉了）。", e.0));
    }
    bad(e.0)
}

/// 送出第 i 張（種子 +i），回傳 {"job", "seed"}。flow＝自訂的影片工作流或生圖工作流組；沒有就用內建的
async fn submit_one(
    app: &Shared,
    p: &Value,
    i: i64,
    video: bool,
    fam: &str,
    flow: Option<&Flow>,
) -> Result<Value, super::ApiError> {
    let paths = &app.paths;
    {
        let mut pp = p.clone();
        if video {
            // 封面（第一格）的檔名：縮圖 API 看到 mp4 會去找 AnimaWeb/_poster/<poster>_*.png
            pp["poster"] = json!(format!("{:016x}", random_seed() as u64 ^ (i as u64) << 48));
        }
        let p = pp;
        let (pp, input) = (p.clone(), paths.input.clone());
        let built = match flow.cloned() {
            Some(Flow::Image { set, mode }) => blocking(move || image_wf::build(&pp, &set, &mode, &input, i)).await,
            Some(Flow::Video(wf)) => blocking(move || video_wf::build(&pp, &wf, &input, i)).await,
            None => blocking(move || graphs::build(&pp, &input, i)).await,
        };
        let (nodes, mut info) = built.map_err(build_error)?;
        info["idea"] = if truthy(p.get("idea")) {
            p["idea"].clone()
        } else {
            json!("")
        };
        let mut params = p.clone();
        params["seed"] = info["seed"].clone();
        params["prompt_resolved"] = info["prompt"].clone();
        let pid = app
            .comfy
            .submit(&Value::Object(nodes), &json!({"anima_params": params}))
            .await
            .map_err(|e: ComfyError| bad(e.0))?;
        app.db.add_job(&pid, &params, &info)?;
        app.comfy.sync_job(&pid).await;
        *app.last_family.lock().unwrap_or_else(|e| e.into_inner()) = fam.to_string();
        Ok(json!({"job": pid, "seed": info["seed"]}))
    }
}

/// POST /api/gen/export_graph {"params": 生成頁的參數, "save"?: true, "dir"?: 從哪個資料夾開始}：把內建功能（照這些參數）組成的節點圖加上槽位標記，
/// 存成 ComfyUI 的 API 格式 JSON（anima_<模式>_<日期>.json）。不送進 ComfyUI。
/// save：跳「另存新檔」讓使用者選位置 → {"ok", "path", "name", "mode", "family", "filled": 補了哪些佔位圖}｜{"cancelled": true}；沒給就照舊下載。
/// 標記（{{prompt}}、{{negative}}、{{image}}、{{mask}}、{{refN}}、{{model}}、{{sampler}}、Save Image 的 {{mode:…}}）寫在節點標題或文字欄位，
/// 使用者在 ComfyUI 改好再匯入，生圖站才知道要把提示詞、圖、遮罩、模型放哪裡（image_wf::mark_for_export）。
/// 還沒放輸入圖、遮罩、參考圖的，用佔位圖代替（image_wf::export_placeholders）
pub async fn export_graph(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let Some(Value::Object(mut p)) = body.get("params").cloned() else {
        return Err(bad("送來的資料格式不對：缺少 params"));
    };
    let missing = missing_txt2img(&p);
    if !missing.is_empty() {
        return Err(bad(format!("文生圖缺少參數：{}", missing.join("、"))));
    }
    // 只匯得出工作流組也有的模式（匯回去才有地方放）
    let m = p.get("mode").and_then(Value::as_str).unwrap_or("txt2img");
    if !image_wf::MODES.contains(&m) {
        let names: Vec<&str> = image_wf::MODES.iter().map(|m| image_wf::mode_name(m)).collect();
        return Err(bad(format!("這個功能沒辦法存成工作流（可以的：{}）", names.join("、"))));
    }
    resolve_seed(&mut p)?;
    // 一定是內建的那份，不管這個功能有沒有被別的工作流取代
    p.insert("builtin".into(), json!(true));
    let prep = prepare(&app, p)?;
    if prep.flow.is_some() {
        return Err(bad("這是匯入的工作流，本來就是 ComfyUI 的工作流：到「設定 → 模型與工作流」按它的「匯出」"));
    }
    let (mut pm, input) = (prep.p, app.paths.input.clone());
    let (pm, filled) = blocking(move || {
        let r = image_wf::export_placeholders(&mut pm, &input);
        (pm, r)
    })
    .await;
    let filled = filled.map_err(bad)?;
    let p = Value::Object(pm);
    let mode = p.get("mode").and_then(Value::as_str).unwrap_or("txt2img").to_string();
    let (pp, input) = (p.clone(), app.paths.input.clone());
    let (nodes, info) = blocking(move || graphs::build(&pp, &input, 0)).await.map_err(build_error)?;
    let mut marked = p;
    marked["prompt_resolved"] = info["prompt"].clone();
    marked["negative_resolved"] = info["negative"].clone();
    // 放大這類沒有主模型的流程，匯回來要靠這個才知道是哪個家族的
    marked["family"] = json!(&prep.fam);
    let graph = image_wf::mark_for_export(&nodes, &marked);
    let name = format!("anima_{mode}_{}.json", chrono::Local::now().format("%Y%m%d"));
    if !truthy(body.get("save")) {
        return api_wf::json_download(&name, &graph);
    }
    match api_wf::json_save("存成 ComfyUI 工作流", &name, &graph, body.get("dir").and_then(Value::as_str)).await? {
        Some(path) => ok(json!({"ok": true, "path": path, "name": name, "mode": mode, "family": prep.fam, "filled": filled})),
        None => ok(json!({"cancelled": true})),
    }
}

// ---------------------------------------------------------------- 任務面板
pub async fn jobs_list(
    State(app): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let limit = match q.get("limit") {
        Some(s) => s.trim().parse::<i64>().map_err(|_| bad("limit 要是整數"))?,
        None => 60,
    }
    .clamp(1, 300);
    let db = &app.db;
    let mut rows = db.jobs(limit)?;
    let mut avg: HashMap<(String, String), Value> = HashMap::new();
    for r in rows.iter_mut() {
        if !matches!(r["status"].as_str(), Some("queued" | "running")) {
            continue;
        }
        let model = r["summary"]["model"].as_str().map(str::to_owned);
        let mode = r["summary"]["mode"].as_str().map(str::to_owned);
        let k = (
            model.clone().unwrap_or_default(),
            mode.clone().unwrap_or_default(),
        );
        if !avg.contains_key(&k) {
            let a = db
                .avg_seconds(model.as_deref(), mode.as_deref(), 20)?
                .or(db.avg_seconds(model.as_deref(), None, 20)?)
                .or(db.avg_seconds(None, None, 20)?);
            avg.insert(k.clone(), json!(a));
        }
        r.insert("avg_seconds".into(), avg[&k].clone());
    }
    let l = app.comfy.live();
    ok(json!({"jobs": rows, "running": l.running, "progress": l.progress, "now": now()}))
}

/// 用同樣的參數（含種子、已展開的 {a|b}）再跑一次
pub async fn jobs_retry(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let id = body.get("job").and_then(Value::as_str).unwrap_or("");
    let job = app.db.job(id)?.ok_or_else(|| not_found("找不到這個任務"))?;
    let mut p = job["params"].as_object().cloned().unwrap_or_default();
    if truthy(p.get("prompt_resolved")) {
        if let Some(r) = p.shift_remove("prompt_resolved") {
            p.insert("prompt".into(), r);
        }
    }
    if truthy(body.get("new_seed")) {
        p.insert("seed".into(), json!(random_seed()));
    }
    submit(&app, p, 1).await
}

/// 「清掉已結束的」：只刪任務紀錄，圖不動
pub async fn jobs_clear(State(app): State<Shared>) -> ApiResult {
    app.db.run(
        "DELETE FROM jobs WHERE status NOT IN ('queued','running')",
        [],
    )?;
    ok(json!({"ok": true}))
}

pub async fn interrupt(State(app): State<Shared>) -> ApiResult {
    app.comfy
        .post("/interrupt", &json!({}))
        .await
        .map_err(internal)?;
    ok(json!({"ok": true}))
}

pub async fn clear_queue(State(app): State<Shared>) -> ApiResult {
    app.comfy
        .post("/queue", &json!({"clear": true}))
        .await
        .map_err(internal)?;
    app.comfy
        .post("/interrupt", &json!({}))
        .await
        .map_err(internal)?;
    // 把資料庫裡還沒結束的任務也收掉；不然它們永遠停在 queued，恢復原廠會一直被擋、導演台／助手的等待也不會停
    for job in app.db.open_jobs().unwrap_or_default() {
        let _ = app.db.set_job(&job, "cancelled", None);
        app.hub
            .send(json!({"type": "finish", "job": job, "status": "cancelled"}));
    }
    ok(json!({"ok": true}))
}

pub async fn cancel(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let job = body
        .get("job")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("缺少 job"))?
        .to_string();
    if app.comfy.live().running.as_deref() == Some(job.as_str()) {
        app.comfy
            .post("/interrupt", &json!({}))
            .await
            .map_err(internal)?;
    } else {
        app.comfy
            .post("/queue", &json!({"delete": [job]}))
            .await
            .map_err(internal)?;
        app.db.set_job(&job, "cancelled", None)?;
        app.hub
            .send(json!({"type": "finish", "job": job, "status": "cancelled"}));
    }
    ok(json!({"ok": true}))
}

// ---------------------------------------------------------------- 系統狀態
pub async fn system(State(app): State<Shared>) -> ApiResult {
    let live = app.comfy.live();
    let mut out = json!({"version": VERSION, "online": live.online, "pe": app.pe.status(), "comfy_url": app.comfy.url()});
    // 自己開的 ComfyUI 沒開起來或自己關掉了：從 comfyui.log 找出來的原因（launcher.rs），離線時才帶
    if !live.online {
        out["comfy_note"] = json!(crate::launcher::note());
    }
    let output = app.paths.output.clone();
    if let Some((free, total)) = crate::maint::disk_usage(&output) {
        let s = output.to_string_lossy().to_string();
        let drive = if s.len() >= 2 && s.as_bytes()[1] == b':' {
            s[..2].to_string()
        } else {
            s
        };
        out["disk"] = json!({"path": drive, "free": free, "total": total});
    }
    if live.online {
        match app.comfy.get("/system_stats").await {
            Ok(st) => {
                let sys = &st["system"];
                out["ram"] = json!({"total": sys["ram_total"], "free": sys["ram_free"]});
                out["comfy_version"] = sys["comfyui_version"].clone();
                let devs = st["devices"].as_array().cloned().unwrap_or_default();
                if let Some(d) = devs.iter().find(|d| d["type"] == "cuda").or(devs.first()) {
                    let name = d["name"]
                        .as_str()
                        .unwrap_or("")
                        .split(" : ")
                        .next()
                        .unwrap_or("")
                        .to_string();
                    out["vram"] = json!({"name": name, "total": d["vram_total"], "free": d["vram_free"],
                                         "torch_total": d["torch_vram_total"], "torch_free": d["torch_vram_free"]});
                }
            }
            Err(e) => out["error"] = json!(e.0),
        }
    }
    ok(out)
}

/// 叫 ComfyUI 卸載模型、清顯存（下一張會重新載入，比較慢）
pub async fn free_vram(State(app): State<Shared>) -> ApiResult {
    app.comfy
        .post(
            "/free",
            &json!({"unload_models": true, "free_memory": true}),
        )
        .await
        .map_err(|e| internal(format!("ComfyUI 沒有回應：{e}")))?;
    let stopped = app.pe.stop("網頁按了釋放顯存").await.map_err(internal)?;
    ok(json!({"ok": true, "pe_stopped": stopped}))
}
