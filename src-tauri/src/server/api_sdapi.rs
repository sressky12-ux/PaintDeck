//! Stable Diffusion WebUI（AUTOMATIC1111）相容 API：/sdapi/v1/*。
//! 給 SillyTavern 這類只會接 A1111 的程式用：txt2img 轉成繪匣自己的生成（家族判斷、模型預設、LoRA 篩選照舊），
//! 等任務跑完把圖讀回來轉 base64。只做 SillyTavern 會打的端點，img2img、extras 這些沒有。
//!
//!   GET  /sdapi/v1/options                 {"sd_model_checkpoint": 目前的模型, …}（SillyTavern 也拿它當 ping）
//!   POST /sdapi/v1/options                 {"sd_model_checkpoint": 模型}：換模型（只記在記憶體，不改設定）
//!   GET  /sdapi/v1/sd-models               Anima／Qwen 2.1／MOSS 的模型（其他家族內建流程畫不了，不列）
//!   GET  /sdapi/v1/samplers｜schedulers    ComfyUI 的名稱，第一個是 Automatic（＝照模型預設）
//!   GET  /sdapi/v1/upscalers               None＋ComfyUI 的放大模型（高解析修復用）
//!   GET  /sdapi/v1/loras                   ComfyUI 的 LoRA（A1111 格式，酒館的生圖外掛拿來給人挑）；POST refresh-loras 是空的
//!   LoRA 沒用上（找不到、跟模型家族不合）會廣播 {type:"sdapi", warnings} 讓繪匣跳提醒：酒館不顯示 info
//!   GET  /sdapi/v1/progress                目前 ComfyUI 的進度
//!   POST /sdapi/v1/interrupt               停掉從這裡送出、還沒畫完的任務（網頁送的不動）
//!   POST /sdapi/v1/txt2img                 → {"images": [base64 PNG], "parameters": 原請求, "info": JSON 字串}
//!
//! 沒給或給 0 的參數（寬高、步數、CFG、取樣器、排程、負面詞）用模型預設（api_agent_tools::family_defaults），
//! 不會像直接打 /api/generate 那樣畫出 256 寬、0 步的圖。提示詞裡的 <lora:名稱:強度> 會拿出來換成繪匣的 LoRA。
use super::api_agent_tools::{family_defaults, match_family, match_model};
use super::{api_gallery, api_gen, bad, internal, ok, ApiError, ApiResult, Body, Shared};
use crate::director::tx;
use crate::families;
use crate::py::truthy;
use crate::settings;
use axum::extract::State;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// 目前選的模型（POST /sdapi/v1/options 換的）；空的＝還沒換過，用 pick_current 挑的那顆。只放記憶體，不寫進設定
static CURRENT: Mutex<String> = Mutex::new(String::new());
/// 從這裡送出、還在等的任務（/sdapi/v1/interrupt 只停這些）
static ACTIVE: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// 取樣器、排程清單的第一個：照模型預設
const AUTO: &str = "Automatic";
/// 一次最多幾張（batch_size × n_iter）
const MAX_IMAGES: i64 = 8;
const WAIT_LIMIT: Duration = Duration::from_secs(1800);

/// A1111 的取樣器名稱 → ComfyUI（小寫比對；DPM++ 2M 這類照規則轉得出來的不用列）
const SAMPLER_ALIASES: [(&str, &str); 7] = [
    ("euler a", "euler_ancestral"),
    ("dpm2", "dpm_2"),
    ("dpm2 a", "dpm_2_ancestral"),
    ("dpm++ 2s a", "dpmpp_2s_ancestral"),
    ("unipc", "uni_pc"),
    ("dpm fast", "dpm_fast"),
    ("dpm adaptive", "dpm_adaptive"),
];
/// A1111 舊版把排程寫在取樣器名稱後面（「DPM++ 2M Karras」）
const SCHEDULER_SUFFIXES: [&str; 3] = [" karras", " exponential", " sgm uniform"];

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn names(v: Vec<Value>) -> Vec<String> {
    v.iter().filter_map(Value::as_str).map(str::to_string).collect()
}

/// 這台電腦內建流程畫得了的模型：[(檔名, 家族)]
async fn image_models(app: &Shared) -> Result<Vec<(String, String)>, ApiError> {
    if !app.comfy.live().online {
        return Err(bad("ComfyUI 沒有連線。解法：重新開啟「繪匣 PaintDeck」。"));
    }
    let list = app.comfy.options("UNETLoader", "unet_name").await.map_err(|e| bad(e.0))?;
    let roots = app.paths.unet_roots();
    Ok(names(list)
        .into_iter()
        .map(|n| {
            let f = families::family_of(&roots, &n).to_string();
            (n, f)
        })
        .filter(|(_, f)| families::is_builtin(f))
        .collect())
}

/// A1111 的模型標題是「檔名 [雜湊]」：拿掉雜湊
fn strip_hash(title: &str) -> &str {
    let t = title.trim();
    t.rsplit_once(" [").filter(|(_, h)| h.ends_with(']')).map_or(t, |(n, _)| n.trim())
}

fn lookup<'a>(list: &'a [(String, String)], want: &str) -> Option<&'a (String, String)> {
    match_model(list, strip_hash(want))
}

/// 目前的模型：換過的那顆（還在的話）→ Anima（Turbo 優先）→ 清單第一顆
fn pick_current<'a>(list: &'a [(String, String)], chosen: &str) -> Option<&'a (String, String)> {
    (!chosen.is_empty())
        .then(|| list.iter().find(|(n, _)| n == chosen))
        .flatten()
        .or_else(|| match_family(list, "anima"))
        .or(list.first())
}

fn model_entry(name: &str, fam: &str) -> Value {
    let base = name.rsplit(['\\', '/']).next().unwrap_or(name);
    let stem = base.rsplit_once('.').map_or(base, |(s, _)| s);
    json!({"title": name, "model_name": stem, "hash": null, "sha256": null, "filename": name, "config": null, "family": fam})
}

/// 取樣器名稱 → ComfyUI 的；Automatic、空白、認不得都回 None（用模型預設）
fn sampler_of(want: &str, list: &[String]) -> Option<String> {
    let mut w = want.trim().to_lowercase();
    if w.is_empty() || w == AUTO.to_lowercase() {
        return None;
    }
    if let Some(hit) = list.iter().find(|n| n.to_lowercase() == w) {
        return Some(hit.clone());
    }
    for s in SCHEDULER_SUFFIXES {
        if let Some(x) = w.strip_suffix(s) {
            w = x.to_string();
        }
    }
    let alias = SAMPLER_ALIASES.iter().find(|(a, _)| *a == w).map(|(_, c)| c.to_string());
    let guess = alias.unwrap_or_else(|| w.replace("++", "pp").replace([' ', '-'], "_"));
    list.iter().find(|n| n.to_lowercase() == guess).cloned()
}

/// 舊式的取樣器名稱把排程接在尾巴（「DPM++ 2M Karras」）：回那個排程（karras、exponential、sgm_uniform）。
/// 2026-11-10 以前 sampler_of 只把尾巴切掉丟了，沒另外送 scheduler 的舊客戶端排程會掉回模型預設（讀圖片參數那邊 imgmeta 有拆）
fn embedded_scheduler(sampler: &str) -> Option<&'static str> {
    let w = sampler.trim().to_lowercase();
    SCHEDULER_SUFFIXES.iter().find(|s| w.ends_with(**s)).map(|s| s.trim())
}

/// 排程名稱 → ComfyUI 的；Automatic、空白、認不得都回 None。A1111 的 Uniform＝ComfyUI 的 normal
fn scheduler_of(want: &str, list: &[String]) -> Option<String> {
    let w = want.trim().to_lowercase().replace([' ', '-'], "_");
    if w.is_empty() || w == AUTO.to_lowercase() {
        return None;
    }
    let w = if w == "uniform" { "normal".to_string() } else { w };
    list.iter().find(|n| n.to_lowercase() == w).cloned()
}

/// <lora:名稱:強度>，A1111 的 LyCORIS 寫法 <lyco:…> 也收；第三段（text encoder 強度）繪匣用不到，略過
fn lora_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)<\s*(?:lora|lyco)\s*:([^:>]+)(?::([^:>]*))?(?::[^>]*)?>").expect("LoRA 標籤的正規式"))
}

/// 比對用的鍵：只留字母數字（中日韓文也算）、轉小寫。
/// 酒館叫 LLM 寫提示詞時會把 \ + @ & 中文以外的符號、還有不是英數的字元換成空白（SillyTavern processReply），
/// 所以「anima\foo+bar」到繪匣會變成「anima foo bar」：比鍵才對得上
fn loose(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// ComfyUI 的 LoRA 名稱 → 可以拿來比對的寫法：完整路徑、去副檔名的路徑、檔名、去副檔名的檔名（全部小寫、/ 換成 \）
fn lora_forms(name: &str) -> [String; 4] {
    let full = name.to_lowercase().replace('/', "\\");
    let no_ext = full.rsplit_once('.').filter(|(_, ext)| !ext.contains('\\')).map_or(full.clone(), |(s, _)| s.to_string());
    let base = full.rsplit('\\').next().unwrap_or(&full).to_string();
    let stem = base.rsplit_once('.').map_or(base.clone(), |(s, _)| s.to_string());
    [full, no_ext, base, stem]
}

/// 名稱 → ComfyUI 的 LoRA 檔名。先比完整寫法（檔名、去副檔名、含資料夾），再比繪匣 LoRA 庫取的標題，
/// 都沒有才比鬆的鍵（大小寫、空白、底線、符號都不管）；鬆的對到好幾顆就挑第一顆並回提醒
fn find_lora(want: &str, list: &[String], titles: &[(String, String)]) -> (Option<String>, Option<String>) {
    let w = want.trim().to_lowercase().replace('/', "\\");
    if w.is_empty() {
        return (None, None);
    }
    if let Some(n) = list.iter().find(|n| lora_forms(n).contains(&w)) {
        return (Some(n.clone()), None);
    }
    if let Some((n, _)) = titles.iter().find(|(n, t)| t.trim().to_lowercase() == w && list.contains(n)) {
        return (Some(n.clone()), None);
    }
    let k = loose(&w);
    if k.is_empty() {
        return (None, None);
    }
    let title_of = |n: &str| titles.iter().find(|(x, _)| x == n).map(|(_, t)| loose(t)).unwrap_or_default();
    let hits: Vec<&String> = list
        .iter()
        .filter(|n| lora_forms(n)[1..].iter().any(|f| loose(f) == k) || title_of(n) == k)
        .collect();
    match hits.as_slice() {
        [] => (None, None),
        [one] => (Some((*one).clone()), None),
        [first, ..] => (
            Some((*first).clone()),
            Some(format!("LoRA「{}」對到 {} 顆，用了 {first}", want.trim(), hits.len())),
        ),
    }
}

/// 提示詞裡的 <lora:名稱:強度>：拿出來對 ComfyUI 的 LoRA 清單（find_lora）。
/// 回傳（拿掉標籤的提示詞、[{name, strength}]、找不到的名稱、提醒）
fn extract_loras(prompt: &str, list: &[String], titles: &[(String, String)]) -> (String, Vec<Value>, Vec<String>, Vec<String>) {
    let (mut loras, mut unknown, mut notes) = (Vec::<Value>::new(), vec![], vec![]);
    for c in lora_re().captures_iter(prompt) {
        let strength = c
            .get(2)
            .and_then(|m| m.as_str().trim().replace(',', ".").parse::<f64>().ok())
            .filter(|x| x.is_finite())
            .unwrap_or(1.0);
        match find_lora(&c[1], list, titles) {
            (Some(n), note) => {
                notes.extend(note);
                // 同一顆寫兩次：後面的強度蓋掉前面的，不要掛兩次
                match loras.iter_mut().find(|l| l["name"] == json!(n)) {
                    Some(l) => l["strength"] = json!(strength),
                    None => loras.push(json!({"name": n, "strength": strength, "enabled": true})),
                }
            }
            (None, _) => unknown.push(c[1].trim().to_string()),
        }
    }
    if loras.is_empty() && unknown.is_empty() {
        return (prompt.to_string(), loras, unknown, notes);
    }
    // 標籤拿掉後留下的「, ,」收乾淨
    let stripped = lora_re().replace_all(prompt, "");
    let cleaned: Vec<&str> = stripped.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    (cleaned.join(", "), loras, unknown, notes)
}

/// ComfyUI 的清單（txt2img 換名稱用）
#[derive(Default)]
struct Lists {
    samplers: Vec<String>,
    schedulers: Vec<String>,
    loras: Vec<String>,
    /// 繪匣 LoRA 庫裡取的標題 [(檔名, 標題)]：酒館寫標題也認
    lora_titles: Vec<(String, String)>,
    upscalers: Vec<String>,
}

/// A1111 的 txt2img 請求 → 繪匣 /api/generate 的 params、張數、要附在 info 的提醒。
/// 沒給或給 0 的參數用模型預設；種子 -1／沒給留給 api_gen::resolve_seed 抽
fn to_params(req: &Value, model: &str, fam: &str, anima_neg: &str, lists: &Lists) -> (Map<String, Value>, i64, Vec<String>) {
    let d = family_defaults(fam, model, anima_neg);
    let num = |k: &str| req.get(k).and_then(Value::as_f64).filter(|x| x.is_finite() && *x > 0.0);
    let int = |k: &str, lo: i64, hi: i64, def: i64| num(k).map_or(def, |x| (x.round() as i64).clamp(lo, hi));
    let mut notes = vec![];
    let (prompt, loras, unknown, lora_notes) = extract_loras(&tx(&req["prompt"]), &lists.loras, &lists.lora_titles);
    if !unknown.is_empty() {
        notes.push(format!("找不到這些 LoRA，略過：{}", unknown.join("、")));
    }
    notes.extend(lora_notes);
    let neg = tx(&req["negative_prompt"]);
    let negative = if neg.trim().is_empty() { d["negative"].clone() } else { json!(neg) };
    let sampler_want = tx(&req["sampler_name"]);
    let sampler = sampler_of(&sampler_want, &lists.samplers);
    if sampler.is_none() && !sampler_want.trim().is_empty() && !sampler_want.eq_ignore_ascii_case(AUTO) {
        notes.push(format!("ComfyUI 沒有取樣器「{sampler_want}」，改用模型預設"));
    }
    // 請求自己送了 scheduler 就照它；沒送才看取樣器名稱尾巴帶的
    let scheduler = scheduler_of(&tx(&req["scheduler"]), &lists.schedulers)
        .or_else(|| embedded_scheduler(&sampler_want).and_then(|s| scheduler_of(s, &lists.schedulers)));
    let seed = req.get("seed").and_then(Value::as_i64).filter(|s| *s >= 0).map_or(json!(-1), |s| json!(s));
    let mut p = Map::new();
    p.insert("mode".into(), json!("txt2img"));
    p.insert("model".into(), json!(model));
    p.insert("prompt".into(), json!(prompt));
    p.insert("negative".into(), negative);
    p.insert("width".into(), json!(int("width", 64, 4096, 1024)));
    p.insert("height".into(), json!(int("height", 64, 4096, 1024)));
    p.insert("steps".into(), json!(int("steps", 1, 150, d["steps"].as_i64().unwrap_or(25))));
    p.insert("cfg".into(), num("cfg_scale").map_or(d["cfg"].clone(), |c| json!(c)));
    p.insert("sampler".into(), sampler.map_or(d["sampler"].clone(), |s| json!(s)));
    p.insert("scheduler".into(), scheduler.map_or(d["scheduler"].clone(), |s| json!(s)));
    p.insert("seed".into(), seed);
    p.insert("loras".into(), json!(loras));
    if truthy(req.get("enable_hr")) {
        // 高解析修復要一顆放大模型：指定的有就用，沒有就挑第一顆；一顆都沒有就不做
        let want = tx(&req["hr_upscaler"]);
        match lists.upscalers.iter().find(|u| **u == want).or(lists.upscalers.first()) {
            Some(up) => {
                let scale = num("hr_scale").unwrap_or(1.5).clamp(1.0, 4.0);
                let denoise = num("denoising_strength").unwrap_or(0.4).min(1.0);
                p.insert("hires".into(), json!({"enabled": true, "scale": scale, "denoise": denoise, "upscaler": up}));
            }
            None => notes.push("ComfyUI 沒有放大模型，高解析修復略過".into()),
        }
    }
    let count = (int("batch_size", 1, MAX_IMAGES, 1) * int("n_iter", 1, MAX_IMAGES, 1)).clamp(1, MAX_IMAGES);
    (p, count, notes)
}

/// 任務還在排或在畫
fn open(app: &Shared, pid: &str) -> bool {
    matches!(
        app.db.job(pid).ok().flatten().as_ref().and_then(|j| j["status"].as_str()),
        Some("queued" | "running")
    )
}

/// 等任務一張一張跑完；被 /sdapi/v1/interrupt 拿掉就算中斷
async fn wait_all(app: &Shared, pids: &[String]) -> Result<(), ApiError> {
    let deadline = Instant::now() + WAIT_LIMIT;
    for pid in pids {
        loop {
            if !lock(&ACTIVE).contains(pid) {
                return Err(bad("已中斷"));
            }
            let job = app.db.job(pid)?;
            match job.as_ref().and_then(|j| j["status"].as_str()) {
                Some("done") => break,
                Some("queued" | "running") => {}
                Some(s) => {
                    let e = job.as_ref().map(|j| tx(&j["error"])).unwrap_or_default();
                    return Err(internal(format!("ComfyUI 任務沒有完成（{s}）{e}")));
                }
                None => return Err(internal("任務紀錄不見了")),
            }
            if Instant::now() > deadline {
                return Err(internal("等超過 30 分鐘還沒畫完，先不等了（任務還在繪匣的佇列裡）"));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- 端點
pub async fn options_get(State(app): State<Shared>) -> ApiResult {
    let chosen = lock(&CURRENT).clone();
    let model = match image_models(&app).await {
        Ok(list) => pick_current(&list, &chosen).map(|m| m.0.clone()).unwrap_or_default(),
        Err(_) => chosen,
    };
    ok(json!({"sd_model_checkpoint": model, "sd_checkpoint_hash": null, "sd_vae": AUTO,
              "CLIP_stop_at_last_layers": 1, "samples_format": "png"}))
}

pub async fn options_set(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    if let Some(want) = body.get("sd_model_checkpoint").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
        let list = image_models(&app).await?;
        let (name, _) = lookup(&list, want)
            .ok_or_else(|| bad(format!("找不到模型「{want}」（只有 Anima、Qwen 2.1、MOSS 的模型能用，清單在 /sdapi/v1/sd-models）")))?;
        *lock(&CURRENT) = name.clone();
    }
    ok(Value::Null)
}

pub async fn sd_models(State(app): State<Shared>) -> ApiResult {
    let list = image_models(&app).await?;
    ok(Value::Array(list.iter().map(|(n, f)| model_entry(n, f)).collect()))
}

pub async fn samplers(State(app): State<Shared>) -> ApiResult {
    let list = names(app.comfy.options("KSampler", "sampler_name").await.map_err(|e| bad(e.0))?);
    let all = std::iter::once(AUTO.to_string()).chain(list);
    ok(Value::Array(all.map(|n| json!({"name": n, "aliases": [], "options": {}})).collect()))
}

pub async fn schedulers(State(app): State<Shared>) -> ApiResult {
    let list = names(app.comfy.options("KSampler", "scheduler").await.map_err(|e| bad(e.0))?);
    let all = std::iter::once(AUTO.to_string()).chain(list);
    ok(Value::Array(
        all.map(|n| json!({"name": n, "label": n, "aliases": null, "default_rho": -1, "need_inner_model": false}))
            .collect(),
    ))
}

pub async fn upscalers(State(app): State<Shared>) -> ApiResult {
    let list = names(app.comfy.options("UpscaleModelLoader", "model_name").await.unwrap_or_default());
    let none = json!({"name": "None", "model_name": null, "model_path": null, "model_url": null, "scale": 4});
    let ups = list.iter().map(|n| json!({"name": n, "model_name": n, "model_path": null, "model_url": null, "scale": 4}));
    ok(Value::Array(std::iter::once(none).chain(ups).collect()))
}

/// ComfyUI 看得到的 LoRA 檔名
async fn lora_names(app: &Shared) -> Vec<String> {
    names(app.comfy.options("LoraLoaderModelOnly", "lora_name").await.unwrap_or_default())
}

/// 繪匣 LoRA 庫裡有取標題的 [(檔名, 標題)]
fn lora_titles(app: &Shared) -> Vec<(String, String)> {
    app.db
        .lora_meta()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(n, m)| m.get("title").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()).map(|t| (n.clone(), t.to_string())))
        .collect()
}

/// A1111 的 LoRA 清單：一筆 {name: 去副檔名的檔名, alias: 繪匣 LoRA 庫的標題（沒有就同 name）, path: ComfyUI 的檔名, metadata: {}}。
/// 酒館的生圖外掛、提示詞助手這類會先抓這份清單讓人挑，挑完照 <lora:name:強度> 寫進提示詞
fn lora_entries(list: &[String], titles: &[(String, String)]) -> Vec<Value> {
    list.iter()
        .map(|n| {
            let base = n.rsplit(['\\', '/']).next().unwrap_or(n);
            let stem = base.rsplit_once('.').map_or(base, |(s, _)| s);
            let alias = titles.iter().find(|(x, _)| x == n).map_or(stem, |(_, t)| t.as_str());
            json!({"name": stem, "alias": alias, "path": n, "metadata": {}})
        })
        .collect()
}

pub async fn loras(State(app): State<Shared>) -> ApiResult {
    if !app.comfy.live().online {
        return Err(bad("ComfyUI 沒有連線。解法：重新開啟「繪匣 PaintDeck」。"));
    }
    ok(Value::Array(lora_entries(&lora_names(&app).await, &lora_titles(&app))))
}

/// 潛空間放大、VAE 清單：繪匣照模型家族自己接，不給選
pub async fn empty_list() -> ApiResult {
    ok(json!([]))
}

pub async fn refresh() -> ApiResult {
    ok(Value::Null)
}

pub async fn progress(State(app): State<Shared>) -> ApiResult {
    let l = app.comfy.live();
    let g = |k: &str| l.progress.as_ref().and_then(|p| p[k].as_f64()).unwrap_or(0.0);
    let (v, m) = (g("value"), g("max"));
    let frac = if m > 0.0 { (v / m).clamp(0.0, 1.0) } else { 0.0 };
    ok(json!({
        "progress": frac, "eta_relative": 0.0,
        "state": {"skipped": false, "interrupted": false, "job": l.running.unwrap_or_default(), "job_count": l.queue_remaining,
                  "job_timestamp": "", "job_no": 0, "sampling_step": v, "sampling_steps": m},
        "current_image": null, "textinfo": null,
    }))
}

pub async fn interrupt(State(app): State<Shared>) -> ApiResult {
    let pids = std::mem::take(&mut *lock(&ACTIVE));
    // 工作節點：每件找它所在的那台
    let live: Vec<String> = pids.iter().filter(|p| open(&app, p)).cloned().collect();
    app.pool.cancel(&live).await;
    ok(Value::Null)
}

pub async fn txt2img(State(app): State<Shared>, Body(req): Body) -> ApiResult {
    let list = image_models(&app).await?;
    let (model, fam) = match req["override_settings"]["sd_model_checkpoint"].as_str().filter(|s| !s.trim().is_empty()) {
        Some(w) => lookup(&list, w).cloned().ok_or_else(|| bad(format!("找不到模型「{w}」（清單在 /sdapi/v1/sd-models）")))?,
        None => pick_current(&list, &lock(&CURRENT).clone())
            .cloned()
            .ok_or_else(|| bad("ComfyUI 裡沒有 Anima、Qwen 2.1、MOSS 的模型：先到繪匣的「安裝與修復」或模型頁下載"))?,
    };
    let c = &app.comfy;
    let lists = Lists {
        samplers: names(c.options("KSampler", "sampler_name").await.unwrap_or_default()),
        schedulers: names(c.options("KSampler", "scheduler").await.unwrap_or_default()),
        loras: lora_names(&app).await,
        lora_titles: lora_titles(&app),
        upscalers: names(c.options("UpscaleModelLoader", "model_name").await.unwrap_or_default()),
    };
    let anima_neg = tx(settings::load(&app.paths.settings).get("negative").unwrap_or(&Value::Null));
    let (mut p, count, mut notes) = to_params(&req, &model, &fam, &anima_neg, &lists);
    api_gen::resolve_seed(&mut p)?;
    let jobs = api_gen::submit_jobs(&app, p.clone(), count).await?;
    let pids: Vec<String> = jobs.iter().filter_map(|j| j["job"].as_str().map(str::to_string)).collect();
    if let Some(s) = jobs.first().and_then(|j| j["skipped_loras"].as_array()) {
        let s: Vec<String> = s.iter().map(tx).collect();
        notes.push(format!("跟模型「{model}」的家族不合，略過的 LoRA：{}", s.join("、")));
    }
    // 酒館不會顯示 info 裡的提醒：LoRA 沒用上這種事要在繪匣跳出來講，不然使用者只會覺得「LoRA 沒傳過來」
    if !notes.is_empty() {
        eprintln!("酒館接口（/sdapi/v1/txt2img）：{}", notes.join("；"));
        app.hub.send(json!({"type": "sdapi", "warnings": notes}));
    }
    {
        // SillyTavern 斷線時這個請求會被丟掉、來不及收尾：順便清掉已經結束的舊任務
        let old = lock(&ACTIVE).clone();
        let done: Vec<String> = old.into_iter().filter(|p| !open(&app, p)).collect();
        let mut a = lock(&ACTIVE);
        a.retain(|p| !done.contains(p));
        a.extend(pids.iter().cloned());
    }
    let waited = wait_all(&app, &pids).await;
    lock(&ACTIVE).retain(|x| !pids.contains(x));
    waited?;
    let mut images = vec![];
    for pid in &pids {
        for r in app.db.q("SELECT * FROM images WHERE job=? AND deleted=0 ORDER BY id", [pid])? {
            let path = api_gallery::file_of(&app.paths, &r);
            let data = tokio::fs::read(&path)
                .await
                .map_err(|e| internal(format!("讀不到生成的圖 {}：{e}", path.display())))?;
            images.push(json!(STANDARD.encode(data)));
        }
    }
    if images.is_empty() {
        return Err(internal("任務跑完了，但找不到生成的圖"));
    }
    let seeds: Vec<Value> = jobs.iter().map(|j| j["seed"].clone()).collect();
    let info = json!({
        "prompt": p["prompt"], "negative_prompt": p["negative"], "seed": seeds.first().cloned().unwrap_or(Value::Null), "all_seeds": seeds,
        "width": p["width"], "height": p["height"], "steps": p["steps"], "cfg_scale": p["cfg"],
        "sampler_name": p["sampler"], "scheduler": p["scheduler"], "sd_model_name": model, "family": fam,
        "loras": p["loras"], "jobs": pids, "warnings": notes,
    });
    ok(json!({"images": images, "parameters": req, "info": info.to_string()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn comfy_lists() -> Lists {
        Lists {
            samplers: s(&["euler", "euler_ancestral", "dpmpp_2m", "dpmpp_2m_sde_gpu", "er_sde", "uni_pc"]),
            schedulers: s(&["normal", "karras", "simple", "sgm_uniform", "beta"]),
            loras: s(&["anima\\jgdxstyle_v2.safetensors", "qwen\\Pruna-8step.safetensors"]),
            lora_titles: vec![],
            upscalers: s(&["4x-AnimeSharp.safetensors"]),
        }
    }

    fn lora_names_real() -> Vec<String> {
        s(&[
            "2000s Game Aesthetic 00008_anima.safetensors",
            "anima\\Anima_colorfix_v1_by_Volnovik.safetensors",
            "anima\\x+y@style.safetensors",
            "anima\\中文畫風.safetensors",
            "a\\dup_style.safetensors",
            "b\\dup-style.safetensors",
        ])
    }

    #[test]
    fn lora_names_survive_tavern_prompt_processing() {
        let l = lora_names_real();
        let t = vec![("anima\\中文畫風.safetensors".to_string(), "Ink Wash 水墨".to_string())];
        let find = |w: &str| find_lora(w, &l, &t).0;
        // 檔名、去副檔名、含資料夾（/ 或 \）、去副檔名的完整路徑都認
        assert_eq!(find("2000s Game Aesthetic 00008_anima").as_deref(), Some("2000s Game Aesthetic 00008_anima.safetensors"));
        assert_eq!(find("anima/Anima_colorfix_v1_by_Volnovik").as_deref(), Some("anima\\Anima_colorfix_v1_by_Volnovik.safetensors"));
        assert_eq!(find("ANIMA_COLORFIX_V1_BY_VOLNOVIK.safetensors").as_deref(), Some("anima\\Anima_colorfix_v1_by_Volnovik.safetensors"));
        // 酒館 LLM 寫的提示詞：\ + @ 換成空白、底線變空白、大小寫亂掉
        assert_eq!(find("anima x y style").as_deref(), Some("anima\\x+y@style.safetensors"));
        assert_eq!(find("x y style").as_deref(), Some("anima\\x+y@style.safetensors"));
        assert_eq!(find("anima colorfix v1 by volnovik").as_deref(), Some("anima\\Anima_colorfix_v1_by_Volnovik.safetensors"));
        // 中文檔名、繪匣 LoRA 庫的標題
        assert_eq!(find("中文畫風").as_deref(), Some("anima\\中文畫風.safetensors"));
        assert_eq!(find("ink wash 水墨").as_deref(), Some("anima\\中文畫風.safetensors"));
        // 鬆的比對對到兩顆：挑第一顆、回提醒；完全沒字母數字的不亂猜
        let (hit, note) = find_lora("dup style", &l, &t);
        assert_eq!(hit.as_deref(), Some("a\\dup_style.safetensors"));
        assert!(note.is_some());
        assert_eq!(find("  "), None);
        assert_eq!(find("+++"), None);
        assert_eq!(find("nope"), None);
    }

    #[test]
    fn lyco_tags_spaces_and_duplicates() {
        let l = lora_names_real();
        let (p, loras, unknown, _) =
            extract_loras("1girl, < lora : 2000s Game Aesthetic 00008_anima:0,6>, <lyco:x y style:0.5:1>, <lora:x y style:0.8>", &l, &[]);
        assert_eq!(p, "1girl");
        assert!(unknown.is_empty());
        assert_eq!(loras.len(), 2, "同一顆寫兩次只掛一次：{loras:?}");
        assert_eq!(loras[0]["strength"], json!(0.6));
        assert_eq!(loras[1], json!({"name": "anima\\x+y@style.safetensors", "strength": 0.8, "enabled": true}));
    }

    #[test]
    fn a1111_lora_list() {
        let l = lora_names_real();
        let t = vec![("anima\\中文畫風.safetensors".to_string(), "Ink Wash".to_string())];
        let e = lora_entries(&l, &t);
        assert_eq!(e.len(), l.len());
        assert_eq!(e[1], json!({"name": "Anima_colorfix_v1_by_Volnovik", "alias": "Anima_colorfix_v1_by_Volnovik",
                                "path": "anima\\Anima_colorfix_v1_by_Volnovik.safetensors", "metadata": {}}));
        assert_eq!(e[3]["alias"], json!("Ink Wash"));
        // 清單給的 name 寫回提示詞一定對得到同一顆
        // （dup_style／dup-style 鬆的鍵一樣，但完整寫法先比，各自對到自己）
        for (x, n) in e.iter().zip(&l) {
            assert_eq!(find_lora(x["name"].as_str().unwrap(), &l, &t).0.as_deref(), Some(n.as_str()));
        }
    }

    #[test]
    fn samplers_map_a1111_names() {
        let l = comfy_lists().samplers;
        assert_eq!(sampler_of("Euler a", &l).as_deref(), Some("euler_ancestral"));
        assert_eq!(sampler_of("euler", &l).as_deref(), Some("euler"));
        assert_eq!(sampler_of("DPM++ 2M", &l).as_deref(), Some("dpmpp_2m"));
        assert_eq!(sampler_of("DPM++ 2M Karras", &l).as_deref(), Some("dpmpp_2m"));
        assert_eq!(sampler_of("DPM++ 2M SDE GPU", &l).as_deref(), Some("dpmpp_2m_sde_gpu"));
        assert_eq!(sampler_of("UniPC", &l).as_deref(), Some("uni_pc"));
        assert_eq!(sampler_of("Automatic", &l), None);
        assert_eq!(sampler_of("", &l), None);
        assert_eq!(sampler_of("PLMS", &l), None);
    }

    #[test]
    fn sampler_name_tail_gives_scheduler() {
        let l = comfy_lists().schedulers;
        let via = |s: &str| embedded_scheduler(s).and_then(|x| scheduler_of(x, &l));
        assert_eq!(via("DPM++ 2M Karras").as_deref(), Some("karras"));
        assert_eq!(embedded_scheduler("DPM++ 2M SDE Exponential"), Some("exponential"));
        assert_eq!(via("Euler a SGM Uniform").as_deref(), Some("sgm_uniform"));
        assert_eq!(via("Euler a"), None);
        assert_eq!(via(""), None);
    }

    #[test]
    fn schedulers_map_a1111_names() {
        let l = comfy_lists().schedulers;
        assert_eq!(scheduler_of("Karras", &l).as_deref(), Some("karras"));
        assert_eq!(scheduler_of("Uniform", &l).as_deref(), Some("normal"));
        assert_eq!(scheduler_of("SGM Uniform", &l).as_deref(), Some("sgm_uniform"));
        assert_eq!(scheduler_of("Automatic", &l), None);
        assert_eq!(scheduler_of("align_your_steps", &l), None);
    }

    #[test]
    fn lora_tags_become_loras() {
        let l = comfy_lists().loras;
        let (p, loras, unknown, _) = extract_loras("1girl, <lora:JGDXstyle_v2:0.7>, smile, <lora:nope:1>", &l, &[]);
        assert_eq!(p, "1girl, smile");
        assert_eq!(loras, vec![json!({"name": "anima\\jgdxstyle_v2.safetensors", "strength": 0.7, "enabled": true})]);
        assert_eq!(unknown, vec!["nope".to_string()]);
        // 沒給強度＝1；完整路徑也認
        let (_, loras, _, _) = extract_loras("<lora:qwen/Pruna-8step.safetensors>", &l, &[]);
        assert_eq!(loras[0]["strength"], json!(1.0));
        // 沒有標籤：提示詞原封不動（換行、逗號都不動）
        let (p, loras, _, _) = extract_loras("a cat,\nsitting", &l, &[]);
        assert_eq!((p.as_str(), loras.len()), ("a cat,\nsitting", 0));
    }

    #[test]
    fn missing_params_use_model_defaults() {
        // 只給提示詞：寬高 1024、步數和 CFG 照 Anima Turbo 預設，不會變成 256 寬、0 步
        let (p, count, notes) = to_params(&json!({"prompt": "1girl"}), "anima-turbo-v1.1.safetensors", "anima", "bad hands", &comfy_lists());
        assert_eq!((p["width"].clone(), p["height"].clone()), (json!(1024), json!(1024)));
        assert_eq!((p["steps"].clone(), p["cfg"].clone()), (json!(8), json!(1.0)));
        assert_eq!((p["sampler"].clone(), p["scheduler"].clone()), (json!("euler"), json!("simple")));
        assert_eq!(p["negative"], json!("bad hands"));
        assert_eq!(p["seed"], json!(-1));
        assert_eq!(p["mode"], json!("txt2img"));
        assert!(p.get("hires").is_none());
        assert_eq!(count, 1);
        assert!(notes.is_empty());
        // 給 0、Automatic 也算沒給
        let req = json!({"prompt": "x", "width": 0, "steps": 0, "cfg_scale": 0, "sampler_name": "Automatic", "scheduler": "Automatic"});
        let (p, _, _) = to_params(&req, "moss.safetensors", "moss", "", &comfy_lists());
        assert_eq!((p["width"].clone(), p["steps"].clone(), p["cfg"].clone()), (json!(1024), json!(25), json!(3.0)));
        assert_eq!(p["scheduler"], json!("sgm_uniform"));
    }

    #[test]
    fn given_params_are_kept() {
        let req = json!({
            "prompt": "1girl", "negative_prompt": "lowres", "width": 832, "height": 1216, "steps": 12, "cfg_scale": 3.5,
            "sampler_name": "Euler a", "scheduler": "Karras", "seed": 42, "batch_size": 2, "n_iter": 3,
            "enable_hr": true, "hr_scale": 2, "denoising_strength": 0.3, "hr_upscaler": "Latent",
        });
        let (p, count, notes) = to_params(&req, "anima-base.safetensors", "anima", "bad hands", &comfy_lists());
        assert_eq!((p["width"].clone(), p["height"].clone(), p["steps"].clone()), (json!(832), json!(1216), json!(12)));
        assert_eq!(p["cfg"], json!(3.5));
        assert_eq!((p["sampler"].clone(), p["scheduler"].clone()), (json!("euler_ancestral"), json!("karras")));
        assert_eq!(p["negative"], json!("lowres"));
        assert_eq!(p["seed"], json!(42));
        assert_eq!(count, 6);
        // 指定的放大模型沒有：挑第一顆
        assert_eq!(p["hires"], json!({"enabled": true, "scale": 2.0, "denoise": 0.3, "upscaler": "4x-AnimeSharp.safetensors"}));
        assert!(notes.is_empty());
        // 張數上限、認不得的取樣器回提醒
        let req = json!({"prompt": "x", "batch_size": 10, "n_iter": 10, "sampler_name": "PLMS", "enable_hr": true});
        let (p, count, notes) = to_params(&req, "anima-base.safetensors", "anima", "", &Lists::default());
        assert_eq!(count, MAX_IMAGES);
        assert_eq!(p["sampler"], json!("er_sde"));
        assert!(p.get("hires").is_none());
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn model_titles_and_current() {
        let list = vec![
            ("qwen\\qwen-image-2.1.safetensors".to_string(), "qwen21".to_string()),
            ("anima-base.safetensors".to_string(), "anima".to_string()),
            ("anima-turbo-v1.1.safetensors".to_string(), "anima".to_string()),
        ];
        assert_eq!(strip_hash("anima-base.safetensors [a1b2c3d4]"), "anima-base.safetensors");
        assert_eq!(lookup(&list, "anima-base.safetensors [a1b2c3d4]").map(|m| m.0.as_str()), Some("anima-base.safetensors"));
        assert_eq!(lookup(&list, "qwen-image-2.1.safetensors").map(|m| m.1.as_str()), Some("qwen21"));
        // 沒換過：Anima Turbo 優先；換過而且還在：用換的那顆；換的那顆不見了：退回預設
        assert_eq!(pick_current(&list, "").map(|m| m.0.as_str()), Some("anima-turbo-v1.1.safetensors"));
        assert_eq!(pick_current(&list, "anima-base.safetensors").map(|m| m.0.as_str()), Some("anima-base.safetensors"));
        assert_eq!(pick_current(&list, "gone.safetensors").map(|m| m.0.as_str()), Some("anima-turbo-v1.1.safetensors"));
        assert_eq!(pick_current(&[], ""), None);
        let e = model_entry("qwen\\qwen-image-2.1.safetensors", "qwen21");
        assert_eq!((e["title"].clone(), e["model_name"].clone()), (json!("qwen\\qwen-image-2.1.safetensors"), json!("qwen-image-2.1")));
    }
}
