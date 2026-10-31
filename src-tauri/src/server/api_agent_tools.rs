//! 生圖助手的工具執行（清單、分級、參數檢查在 crate::agent_tools）。
//! 工具大多是「把請求丟給自己的 API」：在程式裡直接呼叫 axum router（tower oneshot），不另開網路連線、不用知道 port，
//! 跟網頁和 MCP 走同一套檢查。會產生圖的工具送生成後等跑完，回傳圖庫編號（呼叫端記成 result 事件，下一輪給助手看）。
use super::api_agent::render_local;
use super::Shared;
use crate::agent_tools::{self as at, clip, RESULT_CHARS};
use crate::director::tx;
use crate::families::{ANIMA, MOSS, QWEN21};
use axum::http::{header, Method, Request};
use serde_json::{json, Map, Value};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// 一次工具呼叫的結果
#[derive(Debug, Default)]
pub struct Outcome {
    /// 給模型看（英文或 JSON）
    pub text: String,
    /// 給使用者看的一句中文
    pub zh: String,
    /// 新做出來的圖（或 look 要看的圖）：圖庫編號
    pub ids: Vec<i64>,
    /// ids 只是拿來看的（look）
    pub look: bool,
    /// set_model 換好的生圖設定（呼叫端寫回對話設定）
    pub base: Option<Value>,
    /// 實際送出的生成參數（「讓我看」時主視窗填進生成頁）
    pub params: Option<Value>,
}

// 生成頁 gen.js 的預設負面詞（anima-tools.js 也有一份；改的時候三份一起改）
const QWEN_NEG: &str = "artifacts, gpt-image, washed-out colors, low quality, low resolution, AI slop, deviantart, sloppy lines, rough sketch, blurry, indistinct, missing fingers, badly drawn hands, wrong number of fingers";
const QWEN_ANIME_NEG: &str = "low quality, low resolution, blurry, jpeg artifacts, washed-out colors, sloppy lines, messy line art, extra fingers, missing fingers, badly drawn hands, deformed anatomy, 3d render, photograph, photorealistic";
const MOSS_NEG: &str = "worst quality, bad anatomy, watermark, jpeg artifact";

/// 生成參數裡跟「這次」無關、不能沿用的欄位（跟助手 generate 拿掉的一樣）
const STALE: &[&str] = &[
    "init_image", "refs", "mask", "inpaint", "upscale", "outpaint", "control", "idea", "pe_prompt", "prompt_resolved", "size_mode", "pixels",
    "ref_pixels", "denoise", "family", "hires", "sheet", "strip", "diff", "agent", "layout", "video_seconds", "video_res",
];

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// 網址參數：字串照用、布林寫 1／0、其他寫 JSON
pub fn query_string(q: &Value) -> String {
    let Some(o) = q.as_object().filter(|o| !o.is_empty()) else { return String::new() };
    let pairs: Vec<String> = o
        .iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| {
            let s = match v {
                Value::String(s) => s.clone(),
                Value::Bool(b) => (if *b { "1" } else { "0" }).to_string(),
                other => other.to_string(),
            };
            format!("{}={}", enc(k), enc(&s))
        })
        .collect();
    if pairs.is_empty() {
        String::new()
    } else {
        format!("?{}", pairs.join("&"))
    }
}

/// 呼叫自己的 API（跟網頁送來的一樣過 only_local 等檢查）。HTTP 400 以上回傳伺服器給的錯誤訊息
pub async fn call_api(app: &Shared, method: &str, path: &str, query: &Value, body: &Value) -> Result<Value, String> {
    use tower::ServiceExt;
    let m = Method::from_bytes(method.to_ascii_uppercase().as_bytes()).map_err(|_| format!("不認得的方法 {method}"))?;
    let bytes = if m == Method::GET { vec![] } else { serde_json::to_vec(body).unwrap_or_default() };
    let req = Request::builder()
        .method(m)
        .uri(format!("{path}{}", query_string(query)))
        .header(header::HOST, "127.0.0.1:1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(bytes))
        .map_err(|e| format!("請求格式不對：{e}"))?;
    let resp = super::router(app.clone()).oneshot(req).await.map_err(|e| format!("{e:?}"))?;
    let status = resp.status();
    let ctype = resp.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let raw = axum::body::to_bytes(resp.into_body(), 32 << 20).await.map_err(|e| format!("讀不了回應：{e}"))?;
    let v = if ctype.contains("json") {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    } else if ctype.starts_with("text/") || ctype.contains("event-stream") || ctype.is_empty() {
        json!(String::from_utf8_lossy(&raw))
    } else {
        json!(format!("（{ctype}，{} bytes）", raw.len()))
    };
    if status.as_u16() >= 400 {
        return Err(v["error"].as_str().map(str::to_string).unwrap_or_else(|| format!("HTTP {status}：{}", clip(&tx(&v), 300))));
    }
    Ok(v)
}

/// 家族預設（anima-tools.js withDefaults 的 Rust 版；寬高由助手自己決定）
pub fn family_defaults(fam: &str, model: &str, anima_neg: &str) -> Map<String, Value> {
    let turbo = model.to_lowercase().contains("turbo");
    let (steps, cfg, sampler, scheduler, neg) = match fam {
        MOSS => (25, 3.0, "euler", "sgm_uniform", MOSS_NEG.to_string()),
        QWEN21 => (if turbo { 8 } else { 25 }, if turbo { 1.0 } else { 3.0 }, "euler", "simple", (if model.to_lowercase().contains("anime") { QWEN_ANIME_NEG } else { QWEN_NEG }).to_string()),
        ANIMA if turbo => (8, 1.0, "euler", "simple", anima_neg.to_string()),
        ANIMA => (30, 4.0, "er_sde", "simple", anima_neg.to_string()),
        _ => (25, 4.0, "euler", "simple", String::new()),
    };
    Map::from_iter([
        ("model".into(), json!(model)),
        ("family".into(), json!(fam)),
        ("steps".into(), json!(steps)),
        ("cfg".into(), json!(cfg)),
        ("sampler".into(), json!(sampler)),
        ("scheduler".into(), json!(scheduler)),
        ("negative".into(), json!(neg)),
    ])
}

/// 生成頁設定換成另一顆模型：家族變了就拿掉固定 LoRA（家族不合會被丟掉）和加速 LoRA
pub fn switch_base(base: &Map<String, Value>, model: &str, fam: &str, anima_neg: &str) -> Map<String, Value> {
    let mut b = base.clone();
    let same = tx(base.get("family").unwrap_or(&Value::Null)) == fam;
    for (k, v) in family_defaults(fam, model, anima_neg) {
        b.insert(k, v);
    }
    if !same {
        b.insert("loras".into(), json!([]));
        b.shift_remove("turbo_lora");
    }
    b
}

/// 模型名稱對到 ComfyUI 的檔名：完整 → 不分大小寫 → 包含（只有一個符合才算）
pub fn match_model<'a>(names: &'a [(String, String)], want: &str) -> Option<&'a (String, String)> {
    let w = want.trim().to_lowercase().replace('/', "\\");
    names
        .iter()
        .find(|(n, _)| n.to_lowercase() == w)
        .or_else(|| names.iter().find(|(n, _)| n.to_lowercase().rsplit('\\').next() == w.rsplit('\\').next()))
        .or_else(|| {
            let hits: Vec<&(String, String)> = names.iter().filter(|(n, _)| n.to_lowercase().contains(&w)).collect();
            (hits.len() == 1).then(|| hits[0])
        })
}

/// 只講了家族（「Qwen 2.1」「qwen」「MOSS」「Anima」）：挑那個家族的一顆，Turbo 優先（2026-10-27 實測 9B 會這樣寫）
pub fn match_family<'a>(names: &'a [(String, String)], want: &str) -> Option<&'a (String, String)> {
    let w = want.to_lowercase();
    let fam = if w.contains("qwen") { QWEN21 } else if w.contains("moss") { MOSS } else if w.contains("anima") { ANIMA } else { return None };
    let of: Vec<&(String, String)> = names.iter().filter(|(_, f)| f == fam).collect();
    of.iter().find(|(n, _)| n.to_lowercase().contains("turbo")).or(of.first()).copied()
}

pub(super) async fn models(app: &Shared) -> Result<(Vec<(String, String)>, String), String> {
    let st = call_api(app, "GET", "/api/state", &json!({}), &json!({})).await?;
    let mut list: Vec<(String, String)> = st["models"]["unet_family"]
        .as_object()
        .map(|o| o.iter().map(|(k, v)| (k.clone(), tx(v))).collect())
        .unwrap_or_default();
    list.sort();
    Ok((list, tx(&st["settings"]["negative"])))
}

/// 換成 Qwen 2.1 來改圖：目前就是 Qwen 2.1 照用，不然挑一顆（Turbo 優先）
async fn qwen_base(app: &Shared, base: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    if tx(base.get("family").unwrap_or(&Value::Null)) == QWEN21 {
        return Ok(base.clone());
    }
    let (list, neg) = models(app).await?;
    let q: Vec<&(String, String)> = list.iter().filter(|(_, f)| f == QWEN21).collect();
    let pick = q.iter().find(|(n, _)| n.to_lowercase().contains("turbo")).or(q.first()).ok_or("這台電腦沒有 Qwen 2.1（編輯）模型，改圖和角色多視圖都要它：到「安裝與修復」或模型頁下載")?;
    Ok(switch_base(base, &pick.0, QWEN21, &neg))
}

/// 拿掉跟這次無關的欄位
fn clean(mut q: Map<String, Value>) -> Map<String, Value> {
    for k in STALE {
        q.shift_remove(*k);
    }
    q
}

/// 圖庫的圖或使用者的圖 → ComfyUI input 裡的檔名和尺寸
async fn input_of(app: &Shared, args: &Value, user_images: &[Value]) -> Result<(String, i64, i64), String> {
    let user_ref = |n: i64| user_images.iter().find(|i| i["n"].as_i64() == Some(n))
        .map(|im| (tx(&im["name"]), im["width"].as_i64().unwrap_or(1024), im["height"].as_i64().unwrap_or(1024)));
    if let Some(r) = args["ref"].as_i64() {
        return user_ref(r).ok_or(format!("沒有第 {r} 張使用者的圖"));
    }
    let id = args["id"].as_i64().ok_or("缺少圖的編號")?;
    let r = match call_api(app, "POST", "/api/to_input", &json!({}), &json!({"id": id})).await {
        Ok(r) => r,
        // 9B 曾把「附的第 1 張圖」寫成 id:1；圖庫沒有此編號時才退回附圖 ref:1。
        Err(e) if e.contains("找不到這張圖") => return user_ref(id).ok_or(e),
        Err(e) => return Err(e),
    };
    Ok((tx(&r["name"]), r["width"].as_i64().unwrap_or(1024), r["height"].as_i64().unwrap_or(1024)))
}

/// 圖庫一張圖當初的生成參數（修臉修手、擴圖照原圖的模型和提示詞）
async fn params_of(app: &Shared, args: &Value) -> Option<Map<String, Value>> {
    let id = args["id"].as_i64()?;
    let r = call_api(app, "GET", &format!("/api/image/{id}"), &json!({}), &json!({})).await.ok()?;
    r["params"].as_object().filter(|p| p.get("model").is_some_and(|m| !tx(m).is_empty())).cloned()
}

fn idea(sid: &str, tool: &str, ask: &str) -> (Value, Value) {
    (json!(format!("助手：{}", ask.chars().take(200).collect::<String>())), json!({"id": sid, "tool": tool}))
}

/// 送生成、等全部跑完（跟助手 generate 同一套）
async fn render(app: &Shared, mut q: Map<String, Value>, count: i64, token: &CancellationToken, sid: &str, tool: &str, ask: &str) -> Result<(Vec<i64>, Value), String> {
    let (i, a) = idea(sid, tool, ask);
    q.insert("idea".into(), i);
    q.insert("agent".into(), a);
    // 生成頁可能固定了種子：工具每次都換新的（跟助手 generate 一樣）
    q.insert("seed".into(), json!(rand::random_range(0..=(1i64 << 48))));
    let params = Value::Object(q.clone());
    let (ids, errors) = render_local(app, q, count, token).await?;
    if ids.is_empty() {
        return Err(format!("一張都沒做出來：{}", errors.join("；")));
    }
    Ok((ids, params))
}

/// 等 ComfyUI 任務（/api/detail 回傳的 jobs）跑完、收圖庫編號
async fn wait_jobs(app: &Shared, jobs: &Value, token: &CancellationToken) -> Result<Vec<i64>, String> {
    let pids: Vec<String> = jobs.as_array().into_iter().flatten().map(|j| tx(&j["job"])).filter(|p| !p.is_empty()).collect();
    let errors = super::api_agent::wait_all(app, &pids, token).await?;
    let mut ids = vec![];
    for pid in &pids {
        let rows = app.db.q("SELECT id FROM images WHERE job=? AND deleted=0 ORDER BY id", [pid]).map_err(|e| e.to_string())?;
        ids.extend(rows.iter().filter_map(|r| r["id"].as_i64()));
    }
    if ids.is_empty() {
        return Err(format!("一張都沒做出來：{}", errors.join("；")));
    }
    Ok(ids)
}

/// 等漫畫台的背景工作（排分鏡、畫）結束
async fn wait_comic(app: &Shared, id: &str, token: &CancellationToken) -> Result<Value, String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(1800);
    loop {
        if token.is_cancelled() {
            let _ = call_api(app, "POST", "/api/comic/stop", &json!({}), &json!({"id": id})).await;
            return Err("已停止".into());
        }
        let p = call_api(app, "GET", &format!("/api/comic/project/{id}"), &json!({}), &json!({})).await?;
        if p["running"] != true {
            return Ok(p);
        }
        if std::time::Instant::now() > deadline {
            return Err("漫畫台跑太久（超過 30 分鐘），先不等了".into());
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
}

fn short_rows(rows: &[Value], keys: &[&str], text_key: &str, n: usize) -> String {
    let list: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut o = Map::new();
            for k in keys {
                if !r[*k].is_null() {
                    o.insert((*k).into(), r[*k].clone());
                }
            }
            let t = tx(&r[text_key]);
            if !t.is_empty() {
                o.insert(text_key.into(), json!(t.chars().take(n).collect::<String>()));
            }
            Value::Object(o)
        })
        .collect();
    clip(&serde_json::to_string(&list).unwrap_or_default(), RESULT_CHARS)
}

/// 工作環境：這個對話的設定和使用者的圖
pub struct Ctx<'a> {
    pub sid: &'a str,
    pub base: &'a Map<String, Value>,
    pub count: i64,
    pub brain: &'a str,
    pub user_images: &'a [Value],
    pub ask: &'a str,
}

/// 執行一次工具（args 已經過 agent_tools::check_call）
pub async fn exec(app: &Shared, cx: &Ctx<'_>, tool: &str, args: &Value, token: &CancellationToken) -> Result<Outcome, String> {
    let mut out = Outcome::default();
    let base = clean(cx.base.clone());
    match tool {
        "models" => {
            let (list, _) = models(app).await?;
            let mut by: Map<String, Value> = Map::new();
            for (n, f) in &list {
                if let Some(a) = by.entry(f.clone()).or_insert(json!([])).as_array_mut() {
                    a.push(json!(n));
                }
            }
            out.text = clip(&json!({"current": cx.base.get("model"), "by_family": by}).to_string(), RESULT_CHARS);
            out.zh = format!("這台電腦有 {} 個生圖模型", list.len());
        }
        "set_model" => {
            let (list, neg) = models(app).await?;
            let want = tx(&args["model"]);
            let (name, fam) = match_model(&list, &want).or_else(|| match_family(&list, &want)).cloned().ok_or_else(|| {
                format!("No model named \"{want}\". Models: {}", clip(&list.iter().map(|(n, f)| format!("{n} ({f})")).collect::<Vec<_>>().join(", "), 1500))
            })?;
            // 跟網頁的換模型（api_agent::base_model）同一條規則：別的家族會配錯文字編碼器和 VAE（2026-10-29 盤點）
            if !crate::families::is_builtin(&fam) {
                return Err(format!("{name} is a {fam} model; you can only use Anima, Qwen 2.1 and MOSS-Image models. Pick one of those, or stay on the current model."));
            }
            let b = switch_base(cx.base, &name, &fam, &neg);
            // 9B 換完模型會把上一個請求的提示詞整段搬過來（2026-10-27 實測：要橘貓，畫成上一張的銀髮女孩）：把這次的要求貼回去
            out.text = format!(
                "Your generate action now uses {name} (family {fam}, {} steps, CFG {}). The user's current request is: \"{}\". Write a NEW prompt for exactly that, from scratch, in the way this family needs; do not reuse a prompt written for an earlier request.",
                b["steps"], b["cfg"], cx.ask.lines().last().unwrap_or("").trim()
            );
            out.zh = format!("換成 {name}（{fam}）");
            out.base = Some(Value::Object(b));
        }
        "gallery" => {
            let mut q = json!({"limit": args["limit"], "q": args["q"], "mode": args["mode"], "model": args["model"]});
            if args["fav"] == true {
                q["fav"] = json!("1");
            }
            let r = call_api(app, "GET", "/api/gallery", &q, &json!({})).await?;
            let rows = r["items"].as_array().cloned().unwrap_or_default();
            out.text = format!("{} images match. {}", r["total"], short_rows(&rows, &["id", "mode", "width", "height", "fav", "missing"], "prompt", 90));
            out.zh = format!("圖庫找到 {} 張", r["total"]);
        }
        "image_info" => {
            let id = args["id"].as_i64().unwrap_or(0);
            let r = call_api(app, "GET", &format!("/api/image/{id}"), &json!({}), &json!({})).await?;
            let p = &r["params"];
            let or = |a: &Value, b: &Value| if tx(a).is_empty() { b.clone() } else { a.clone() };
            let pick = json!({"id": id, "mode": r["mode"], "model": or(&p["model"], &r["model"]), "prompt": or(&p["prompt"], &r["prompt"]), "negative": p["negative"], "loras": p["loras"],
                "seed": p["seed"], "steps": p["steps"], "cfg": p["cfg"], "width": r["width"], "height": r["height"], "fav": r["fav"], "idea": r["idea"]});
            out.text = clip(&pick.to_string(), RESULT_CHARS);
            out.zh = format!("看了第 {id} 張的參數");
        }
        "look" => {
            let mut ids = vec![];
            for id in args["ids"].as_array().into_iter().flatten().filter_map(Value::as_i64) {
                if app.db.image(id).ok().flatten().is_some() {
                    ids.push(id);
                }
            }
            if ids.is_empty() {
                return Err("圖庫裡沒有這些編號".into());
            }
            out.text = format!("Images {ids:?} are shown to you below.");
            out.zh = format!("看第 {} 張", ids.iter().map(i64::to_string).collect::<Vec<_>>().join("、"));
            out.ids = ids;
            out.look = true;
        }
        "edit" => {
            let (name, w, h) = input_of(app, args, cx.user_images).await?;
            let mut q = clean(qwen_base(app, cx.base).await?);
            q.insert("mode".into(), json!("multiref"));
            q.insert("prompt".into(), args["instruction"].clone());
            q.insert("refs".into(), json!([{"image": name, "role": "base", "width": w, "height": h}]));
            q.insert("size_mode".into(), json!("auto"));
            q.insert("pixels".into(), json!((w * h).clamp(512 * 512, 1024 * 1024)));
            let (ids, p) = render(app, q, cx.count, token, cx.sid, tool, cx.ask).await?;
            out.text = format!("Edited with Qwen 2.1: new image ids {ids:?}.");
            out.zh = format!("改好了 {} 張", ids.len());
            (out.ids, out.params) = (ids, Some(p));
        }
        "upscale" | "remove_bg" => {
            let (name, _, _) = input_of(app, args, cx.user_images).await?;
            let mut q = base;
            q.insert("init_image".into(), json!(name));
            if tool == "upscale" {
                q.insert("mode".into(), json!("upscale"));
                q.insert("upscale".into(), json!({"scale": args["scale"], "refine": false}));
            } else {
                q.insert("mode".into(), json!("rembg"));
            }
            let (ids, p) = render(app, q, 1, token, cx.sid, tool, cx.ask).await?;
            out.text = format!("Done: new image ids {ids:?}.");
            out.zh = if tool == "upscale" { format!("放大 {} 倍", args["scale"]) } else { "去背好了".into() };
            (out.ids, out.params) = (ids, Some(p));
        }
        "fix" => {
            let (name, _, _) = input_of(app, args, cx.user_images).await?;
            let mut p = params_of(app, args).await.unwrap_or_else(|| base.clone());
            p.insert("init_image".into(), json!(name));
            let (i, a) = idea(cx.sid, tool, cx.ask);
            p.insert("idea".into(), i);
            p.insert("agent".into(), a);
            let r = call_api(app, "POST", "/api/detail", &json!({}), &json!({"params": p, "init_image": name, "what": args["what"]})).await?;
            let ids = wait_jobs(app, &r["jobs"], token).await?;
            out.text = format!("Repainted {} part(s): new image ids {ids:?}.", r["found"]);
            out.zh = format!("修好了（找到 {} 處）", r["found"]);
            out.ids = ids;
        }
        "outpaint" => {
            let (name, _, _) = input_of(app, args, cx.user_images).await?;
            let mut q = clean(params_of(app, args).await.unwrap_or(base));
            q.insert("mode".into(), json!("outpaint"));
            q.insert("init_image".into(), json!(name));
            q.insert("outpaint".into(), json!({"top": args["top"], "bottom": args["bottom"], "left": args["left"], "right": args["right"], "feather": 40}));
            q.insert("denoise".into(), json!(1.0));
            let (ids, p) = render(app, q, cx.count, token, cx.sid, tool, cx.ask).await?;
            out.text = format!("Extended: new image ids {ids:?}.");
            out.zh = format!("擴圖好了 {} 張", ids.len());
            (out.ids, out.params) = (ids, Some(p));
        }
        "video" => {
            let mut q = Map::from_iter([("mode".into(), json!("video")), ("prompt".into(), args["prompt"].clone()), ("video_seconds".into(), args["seconds"].clone())]);
            // 設定選了 H3：明講要本機 H3（不帶的話 submit_jobs 只會回「到 H3 影片台做」，2026-10-29 盤點）
            if super::api_gen::video_engine(&app.paths, &q) == crate::video_wf::H3 {
                q.insert("video_engine".into(), json!(crate::video_wf::H3));
            }
            if args["id"].is_i64() || args["ref"].is_i64() {
                let (name, _, _) = input_of(app, args, cx.user_images).await?;
                q.insert("init_image".into(), json!(name));
            }
            let (ids, p) = render(app, q, 1, token, cx.sid, tool, cx.ask).await?;
            out.text = format!("Video made: gallery ids {ids:?}. You cannot watch videos; tell the user where it is.");
            out.zh = format!("影片做好了（{} 秒）", args["seconds"]);
            (out.ids, out.params) = (ids, Some(p));
        }
        "character_sheet" => {
            let (name, w, h) = input_of(app, args, cx.user_images).await?;
            let kind = tx(&args["kind"]);
            let sp = call_api(app, "POST", "/api/charsheet/prompt", &json!({}), &json!({"kind": kind, "family": QWEN21})).await?;
            let mut q = clean(qwen_base(app, cx.base).await?);
            q.insert("mode".into(), json!("multiref"));
            q.insert("prompt".into(), sp["prompt"].clone());
            q.insert("refs".into(), json!([{"image": name, "role": "base", "width": w, "height": h}]));
            q.insert("size_mode".into(), json!("fixed"));
            q.insert("width".into(), sp["width"].clone());
            q.insert("height".into(), sp["height"].clone());
            let (ids, p) = render(app, q, cx.count, token, cx.sid, tool, cx.ask).await?;
            out.text = format!("Character {kind} drawn with Qwen 2.1: new image ids {ids:?}.");
            out.zh = format!("{}畫好了", match kind.as_str() { "face" => "臉部特寫表", "portrait" => "正面特寫", _ => "轉身三視圖" });
            (out.ids, out.params) = (ids, Some(p));
        }
        "comic" => {
            let p = call_api(app, "POST", "/api/comic/project/new", &json!({}), &json!({"title": args["title"]})).await?;
            let id = tx(&p["id"]);
            let brain = if cx.brain == "9b" { "9b" } else { "35b" };
            call_api(app, "POST", "/api/comic/script", &json!({}), &json!({"id": id, "story": args["story"], "mode": "all", "brain": brain})).await?;
            wait_comic(app, &id, token).await?;
            let before = call_api(app, "GET", &format!("/api/comic/project/{id}"), &json!({}), &json!({})).await?["runs"].as_array().map(Vec::len).unwrap_or(0);
            call_api(app, "POST", "/api/comic/run", &json!({}), &json!({"id": id, "count": cx.count.clamp(1, 2)})).await?;
            let done = wait_comic(app, &id, token).await?;
            let ids: Vec<i64> = done["runs"].as_array().into_iter().flatten().skip(before).filter_map(|r| r["page"].as_i64()).collect();
            if ids.is_empty() {
                return Err(format!("漫畫作品 {id} 分鏡排好了，但畫頁的時候失敗：到漫畫台看錯誤訊息"));
            }
            out.text = format!("Comic project {id}: panels written and the page drawn, image ids {ids:?}.");
            out.zh = format!("漫畫畫好了（作品 {}）", tx(&args["title"]));
            out.ids = ids;
        }
        "assets" => {
            let r = call_api(app, "GET", "/api/lib", &json!({}), &json!({})).await?;
            let rows: Vec<Value> = r.as_array().cloned().unwrap_or_else(|| r["items"].as_array().cloned().unwrap_or_default());
            let want = tx(&args["kind"]);
            let pick: Vec<Value> = rows
                .into_iter()
                .filter(|x| if want.is_empty() { !["set", "snippet"].contains(&x["kind"].as_str().unwrap_or("")) } else { x["kind"] == want.as_str() })
                .map(|x| json!({"id": x["id"], "kind": x["kind"], "name": x["name"], "look": x["data"]["look_zh"].as_str().or(x["data"]["look_en"].as_str()).unwrap_or("")}))
                .collect();
            out.text = format!("{} assets. {}", pick.len(), short_rows(&pick, &["id", "kind", "name"], "look", 80));
            out.zh = format!("資產庫有 {} 筆", pick.len());
        }
        "trash" => {
            let r = call_api(app, "POST", "/api/images/batch", &json!({}), &json!({"action": "trash", "ids": args["ids"]})).await?;
            out.text = format!("Moved {} image(s) to the recycle bin. {}", r["count"], if tx(&r["error"]).is_empty() { String::new() } else { format!("Some failed: {}", tx(&r["error"])) });
            out.zh = format!("{} 張移到回收區了", r["count"]);
        }
        "guide" => {
            let name = tx(&args["name"]);
            let id = crate::agent::guide_text_id(&name).ok_or_else(|| format!("沒有「{name}」這份寫法"))?;
            let skill = crate::skills::for_guide(&app.paths.data, &app.paths.settings, &name);
            // 外掛寫法的第幾段
            if let Some(n) = args["part"].as_u64() {
                let s = skill.ok_or_else(|| format!("「{}」沒有接外掛寫法（到設定頁的「外掛寫法」綁）", crate::agent::guide_zh(&name)))?;
                out.text = s.part_reply(n as usize)?;
                out.zh = format!("讀外掛寫法「{}」第 {n} 段", s.name);
            } else {
                out.text = format!("Prompt-writing guide \"{name}\". Follow it for everything you write for this model and task; if you were about to write one, write it again following this guide.\n\n{}", app.pe.agent_text(id));
                if let Some(s) = &skill {
                    out.text += "\n\n";
                    out.text += &s.agent_block(&name, "");
                }
                out.zh = format!("讀「{}」的寫法", crate::agent::guide_zh(&name));
            }
        }
        "docs" => {
            let guide = std::fs::read_to_string(app.paths.res.join("extras").join("fablecut").join("anima-guide.md")).map_err(|e| format!("讀不了說明書：{e}"))?;
            out.text = clip(&at::docs_section(&guide, &tx(&args["section"])), RESULT_CHARS);
            out.zh = if tx(&args["section"]).is_empty() { "看說明書目錄".into() } else { format!("讀說明書「{}」", tx(&args["section"])) };
        }
        "api" => {
            let (m, p) = (tx(&args["method"]), tx(&args["path"]));
            let r = call_api(app, &m, &p, &args["query"], &args["body"]).await?;
            out.text = clip(&r.to_string(), RESULT_CHARS);
            out.zh = format!("{m} {p}");
        }
        "open_page" => {
            out.text = format!("The main window shows {}.", tx(&args["page"]));
            out.zh = format!("切到「{}」頁", tx(&args["page"]));
        }
        other => return Err(format!("沒有這個工具：{other}")),
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_strings_are_encoded() {
        assert_eq!(query_string(&json!({})), "");
        assert_eq!(query_string(&json!({"q": "貓 cat", "fav": true, "limit": 3, "x": null})), "?q=%E8%B2%93%20cat&fav=1&limit=3");
    }

    #[test]
    fn switching_model_family_resets_loras() {
        let base = json!({"model": "anima-turbo-v1.1.safetensors", "family": "anima", "steps": 8, "loras": [{"name": "x"}], "turbo_lora": "t", "negative": "bad"});
        let b = switch_base(base.as_object().unwrap(), "qwen\\Qwen-Image-2.1-Turbo.safetensors", QWEN21, "neg");
        assert_eq!((b["family"].clone(), b["steps"].clone(), b["cfg"].clone(), b["loras"].clone()), (json!(QWEN21), json!(8), json!(1.0), json!([])));
        assert!(b.get("turbo_lora").is_none() && tx(&b["negative"]).contains("gpt-image"));
        let same = switch_base(base.as_object().unwrap(), "anima-base.safetensors", ANIMA, "anima neg");
        assert_eq!((same["steps"].clone(), same["sampler"].clone(), same["loras"].clone(), same["negative"].clone()), (json!(30), json!("er_sde"), json!([{"name": "x"}]), json!("anima neg")));
    }

    #[test]
    fn model_names_match_loosely() {
        let l = vec![("anima-turbo-v1.1.safetensors".to_string(), ANIMA.to_string()), ("qwen\\Qwen-Image-2.1-Turbo.safetensors".to_string(), QWEN21.to_string()), ("moss.safetensors".to_string(), MOSS.to_string())];
        assert_eq!(match_model(&l, "Qwen-Image-2.1-Turbo.safetensors").map(|x| x.1.as_str()), Some(QWEN21));
        assert_eq!(match_model(&l, "moss").map(|x| x.0.as_str()), Some("moss.safetensors"));
        assert!(match_model(&l, "safetensors").is_none(), "好幾個符合就不猜");
        assert_eq!(match_family(&l, "Qwen 2.1").map(|x| x.0.as_str()), Some("qwen\\Qwen-Image-2.1-Turbo.safetensors"));
        assert_eq!(match_family(&l, "MOSS-Image").map(|x| x.0.as_str()), Some("moss.safetensors"));
        assert!(match_family(&l, "flux").is_none());
    }
}
