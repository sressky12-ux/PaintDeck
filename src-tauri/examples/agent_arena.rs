//! 生圖助手擂台（2026-10-26）：同一套工作說明、同一套 JSON 動作、同一個 parse_action，讓幾顆本機模型輪流答同樣的題目。
//! 題目、模型清單放在程式外的 JSON（cargo run --example agent_arena -- <cases.json> <輸出資料夾>），
//! 一顆模型一個 llama-server，跑完全部題目才換下一顆；最後寫 results.jsonl（每題每次的原文與評分）和 summary.md（成績單）。
//! 題目有 "task"（t2i／i2i）的是改寫題：走 pe::Core::build_messages＋parse_output，跟生圖站「AI 改寫」同一套；
//! 模型可以用 "tasks" 限定只考哪幾種（PE t2i 只會文生圖改寫、PE i2i 只會改圖改寫）。
use anima_app::agent::{self as ag, Check, LoraInfo};
use anima_app::pe;
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn tx(v: &Value) -> String {
    v.as_str().map(str::to_string).unwrap_or_else(|| if v.is_null() { String::new() } else { v.to_string() })
}

fn vram_used() -> Option<i64> {
    let out = Command::new("nvidia-smi").args(["--query-gpu=memory.used", "--format=csv,noheader,nounits"]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().next()?.trim().parse().ok()
}

fn health(port: u16) -> bool {
    reqwest::blocking::Client::new()
        .get(format!("http://127.0.0.1:{port}/health"))
        .timeout(Duration::from_secs(2))
        .send()
        .is_ok_and(|r| r.status().is_success())
}

/// 開 llama-server（參數照生圖站 pe.rs 的 GPU 模式）；ngl 給 None＝全放顯卡
fn launch(cfg: &Value, m: &Value, ngl: Option<i64>, log: &Path) -> Result<(Child, f64), String> {
    let port = cfg["port"].as_u64().unwrap_or(8299) as u16;
    let mut args: Vec<String> = vec!["-m".into(), tx(&m["file"]), "--host".into(), "127.0.0.1".into(), "--port".into(), port.to_string(), "-c".into(), "16384".into(),
        "-ngl".into(), ngl.unwrap_or(99).to_string(), "--jinja".into(), "--no-webui".into(), "-np".into(), "1".into(), "--reasoning-format".into(), "deepseek".into()];
    if !tx(&m["mmproj"]).is_empty() {
        args.extend(["--mmproj".into(), tx(&m["mmproj"])]);
    }
    if m["moe"].as_bool() == Some(true) {
        // 前幾層的專家放 CPU（生圖站固定 30）；題目檔可以每顆另外給 "ncmoe"
        let n = m["ncmoe"].as_i64().unwrap_or(30).to_string();
        args.extend(["-ncmoe", &n, "-ctk", "q8_0", "-ctv", "q8_0", "-fa", "on"].map(String::from));
    }
    let f = std::fs::File::create(log).map_err(|e| e.to_string())?;
    let t0 = Instant::now();
    let mut child = Command::new(tx(&cfg["bin"])).args(&args).stdout(f.try_clone().unwrap()).stderr(f).stdin(Stdio::null()).spawn().map_err(|e| e.to_string())?;
    loop {
        if let Ok(Some(code)) = child.try_wait() {
            return Err(format!("llama-server 結束（{code}）"));
        }
        if health(port) {
            return Ok((child, t0.elapsed().as_secs_f64()));
        }
        if t0.elapsed() > Duration::from_secs(300) {
            let _ = child.kill();
            return Err("300 秒內沒載入完成".into());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn catalog(cfg: &Value, family: &str) -> Vec<LoraInfo> {
    cfg["loras"][family].as_array().into_iter().flatten().map(|l| LoraInfo::from_meta(&tx(&l["name"]), &l["meta"])).collect()
}

/// 退回後再問一次時，哪些退回會讓 app 把 nudged 設起來（照 api_agent.rs）
fn nudges(fb: &str) -> bool {
    ["You marked", "You were shown", "Everything in your check", "The user attached", "Your previous round built on user image"].iter().any(|p| fb.starts_with(p))
}

const REFUSE: &[&str] = &["i can't", "i cannot", "i'm sorry", "i am sorry", "i won't", "not able to", "against my", "無法協助", "不能幫", "抱歉", "无法", "不能提供"];

/// 照題目的 expect 評分：回傳 (過了沒, 原因)
fn grade(exp: &Value, a: &Map<String, Value>) -> (bool, Vec<String>) {
    let mut why = vec![];
    let act = tx(&a["action"]);
    let prompt = tx(&a.get("prompt").cloned().unwrap_or_default()).to_lowercase();
    if let Some(want) = exp.get("action") {
        let ok = match want {
            Value::Array(l) => l.iter().any(|w| tx(w) == act),
            w => tx(w) == act,
        };
        if !ok {
            why.push(format!("動作是 {act}，要 {want}"));
        }
    }
    let refs = a.get("refs").and_then(Value::as_array).map(|r| !r.is_empty()).unwrap_or(false);
    match exp.get("refs").and_then(Value::as_bool) {
        Some(true) if !refs => why.push("該用 refs 卻沒用".into()),
        Some(false) if refs => why.push("不該用 refs 卻用了（會畫成原圖）".into()),
        _ => {}
    }
    if let Some(min) = exp.get("denoise_min").and_then(Value::as_f64) {
        let d = a.get("denoise").and_then(Value::as_f64).unwrap_or(0.0);
        if refs && d < min {
            why.push(format!("denoise {d} 小於 {min}"));
        }
    }
    if let Some(l) = exp.get("lora").and_then(Value::as_str) {
        let has = a.get("loras").and_then(Value::as_array).into_iter().flatten().any(|x| tx(&x["name"]).contains(l));
        if !has {
            why.push(format!("沒挑 {l}"));
        }
    }
    if let Some(any) = exp.get("prompt_any").and_then(Value::as_array) {
        if !any.iter().any(|w| prompt.contains(&tx(w).to_lowercase())) {
            why.push(format!("提示詞沒有 {}", any.iter().map(tx).collect::<Vec<_>>().join("／")));
        }
    }
    for g in exp.get("prompt_groups").and_then(Value::as_array).into_iter().flatten() {
        let words: Vec<String> = g.as_array().into_iter().flatten().map(|w| tx(w).to_lowercase()).collect();
        if !words.iter().any(|w| prompt.contains(w)) {
            why.push(format!("提示詞缺 {}", words.join("／")));
        }
    }
    for w in exp.get("forbid").and_then(Value::as_array).into_iter().flatten().map(|w| tx(w).to_lowercase()) {
        if prompt.contains(&w) {
            why.push(format!("提示詞不該有 {w}"));
        }
    }
    if exp.get("check_has_no").and_then(Value::as_bool) == Some(true) {
        let no = a.get("check").and_then(Value::as_array).into_iter().flatten().any(|c| tx(c).to_lowercase().contains(": no"));
        if !no {
            why.push("check 沒有任何 no".into());
        }
    }
    if let Some(best) = exp.get("best_in").and_then(Value::as_array) {
        if act == "finish" {
            let got: Vec<i64> = a.get("best").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_i64).collect();
            if got.is_empty() || got.iter().any(|g| !best.iter().any(|b| b.as_i64() == Some(*g))) {
                why.push(format!("best 選了 {got:?}，應該只在 {best:?} 裡"));
            }
        }
    }
    (why.is_empty(), why)
}

/// 一題改寫題：照生圖站 pe.rs 的 rewrite（溫度 1、不思考、官方 max_tokens、讀不出來換種子再一次）
#[allow(clippy::too_many_arguments)]
fn rewrite_case(cfg: &Value, m: &Value, case: &Value, base_dir: &Path, http: &reqwest::blocking::Client, port: u16, runs: i64, jl: &mut std::fs::File, peak: &mut i64) -> Vec<Row> {
    let (name, cid, task) = (tx(&m["name"]), tx(&case["id"]), tx(&case["task"]));
    let core = pe::Core::with_dirs(Path::new(&tx(&cfg["pe_node"])), Path::new(&tx(&cfg["pe_prompts"])), Path::new(&tx(&cfg["pe_models"])));
    let images: Vec<Vec<u8>> = case["images"].as_array().into_iter().flatten().map(|p| std::fs::read(base_dir.join(tx(p))).expect("讀不到測試圖")).collect();
    let msgs = core.build_messages(&task, &tx(&case["text"]), &images, "", "", false, "").expect("組不出改寫訊息");
    let kind = if task == "i2i" { "i2i" } else { "t2i" };
    let mut rows = vec![];
    for run in 0..runs {
        let (mut secs, mut toks, mut gen_ms, mut parsed, mut raw, mut tries) = (0.0, 0i64, 0.0, None, String::new(), 0);
        for attempt in 0..2 {
            tries += 1;
            let body = pe::tune_for(&tx(&m["file"]), pe::payload(&msgs, 1.0, 2000 + run * 7 + attempt, false, Some(pe::official_max_tokens(kind)), false));
            let t0 = Instant::now();
            let Ok(r) = http.post(format!("http://127.0.0.1:{port}/v1/chat/completions")).json(&body).send().and_then(|r| r.json::<Value>()) else { break };
            secs += t0.elapsed().as_secs_f64();
            *peak = (*peak).max(vram_used().unwrap_or(0));
            toks += r["usage"]["completion_tokens"].as_i64().unwrap_or(0);
            gen_ms += r["timings"]["predicted_ms"].as_f64().unwrap_or(0.0);
            raw = tx(&r["choices"][0]["message"]["content"]);
            parsed = pe::parse_output(&raw, &task, "");
            if parsed.is_some() {
                break;
            }
        }
        let prompt = parsed.as_ref().map(|p| p.prompt.clone()).unwrap_or_default();
        let low = prompt.to_lowercase();
        let mut why = vec![];
        if parsed.is_none() {
            why.push("兩次都讀不出 JSON（生圖站會退回原文）".to_string());
        }
        for g in case["expect"]["prompt_groups"].as_array().into_iter().flatten() {
            let words: Vec<String> = g.as_array().into_iter().flatten().map(|w| tx(w).to_lowercase()).collect();
            if !words.iter().any(|w| low.contains(w)) {
                why.push(format!("缺 {}", words.join("／")));
            }
        }
        // 寫淡用的軟詞（intimate、sensual…）出現就不算過
        for w in case["expect"]["forbid"].as_array().into_iter().flatten().map(|w| tx(w).to_lowercase()) {
            if low.contains(&w) {
                why.push(format!("出現軟詞 {w}"));
            }
        }
        let refused = REFUSE.iter().any(|w| raw.to_lowercase().contains(w)) && parsed.is_none();
        if refused {
            why.push("拒絕".into());
        }
        let pass = why.is_empty();
        let tps = if gen_ms > 0.0 { toks as f64 / (gen_ms / 1000.0) } else { 0.0 };
        let _ = writeln!(jl, "{}", json!({"model": name, "case": cid, "run": run, "pass": pass, "why": why, "tries": tries, "secs": secs, "tokens": toks, "tps": tps,
            "chars": prompt.chars().count(), "wh_ratio": parsed.as_ref().map(|p| p.wh_ratio.clone()), "prompt": prompt, "raw": raw.chars().take(4000).collect::<String>()}));
        eprintln!("{name} {cid}#{run}：{}（{secs:.1} 秒，{} 字）{}", if pass { "過" } else { "沒過" }, prompt.chars().count(), why.join("；"));
        rows.push(Row { model: name.clone(), case: cid.clone(), first_ok: tries == 1 && parsed.is_some(), final_ok: parsed.is_some(), pass, refused, rejects: tries - 1, secs, tps,
            why: why.join("；"), excerpt: format!("[{} 字] {}", prompt.chars().count(), prompt.chars().take(320).collect::<String>()) });
    }
    rows
}

struct Row {
    model: String,
    case: String,
    first_ok: bool,
    final_ok: bool,
    pass: bool,
    refused: bool,
    rejects: usize,
    secs: f64,
    tps: f64,
    why: String,
    excerpt: String,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(&args[1]).expect("讀不到題目檔")).expect("題目檔不是 JSON");
    let out = PathBuf::from(&args[2]);
    std::fs::create_dir_all(&out).unwrap();
    let mut jl = std::fs::File::create(out.join("results.jsonl")).unwrap();
    let base_dir = Path::new(&args[1]).parent().unwrap().to_path_buf();
    let port = cfg["port"].as_u64().unwrap_or(8299) as u16;
    let runs = cfg["runs"].as_i64().unwrap_or(2);
    // 2026-10-28 體位擂台的三個開關（都照生圖站的做法）：
    //   pose_note：使用者那句話附上體位庫的 Term 註解（api_agent 的 user_ev）
    //   ban_soft：請求加 logit_bias 擋軟詞（pe::ban_soft_words）
    //   pose_fix：評分前照體位庫補正提示詞（agent::pose_fix＋fix_anima_tags，跟真的送去畫的一樣）
    let flag = |k: &str| cfg[k].as_bool().unwrap_or(false);
    let poses = (flag("pose_note") || flag("pose_fix")).then(|| anima_app::poses::Poses::load(Path::new(cfg["res"].as_str().unwrap_or("D:/AI/anima_app/resources"))));
    let skill = cfg["skill"].as_str().map(|d| anima_app::skills::load(Path::new(d)).expect("讀不到 skill 資料夾（裡面要有 SKILL.md）"));
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(600)).build().unwrap();
    let mut rows: Vec<Row> = vec![];
    let mut model_notes: Vec<String> = vec![];
    let img = |p: &str| pe::image_url_jpeg(&base_dir.join(p), 768).expect("讀不到測試圖");

    for m in cfg["models"].as_array().unwrap() {
        let name = tx(&m["name"]);
        if !Path::new(&tx(&m["file"])).exists() {
            model_notes.push(format!("| {name} | 找不到模型檔，跳過 | | | |"));
            continue;
        }
        let base_vram = vram_used();
        let log = out.join(format!("llama_{name}.log"));
        let mut how = "全放顯卡".to_string();
        let launched = match launch(&cfg, m, None, &log) {
            Ok(x) => Ok(x),
            Err(e) => {
                eprintln!("{name}：{e}，改成部分放顯卡（-ngl 24）再試");
                how = format!("全放顯卡失敗（{e}），改 -ngl 24");
                launch(&cfg, m, Some(24), &out.join(format!("llama_{name}_ngl24.log")))
            }
        };
        let (mut child, load) = match launched {
            Ok(x) => x,
            Err(e) => {
                model_notes.push(format!("| {name} | 開不起來：{e} | | | |"));
                continue;
            }
        };
        let mut peak = vram_used().unwrap_or(0);
        eprintln!("{name} 載入 {load:.1} 秒（{how}）");
        for case in cfg["cases"].as_array().unwrap() {
            let cid = tx(&case["id"]);
            if case.get("task").is_some() {
                let task = tx(&case["task"]);
                let allowed = m["tasks"].as_array().is_none_or(|t| t.iter().any(|x| tx(x) == task));
                if allowed {
                    rows.extend(rewrite_case(&cfg, m, case, &base_dir, &http, port, runs, &mut jl, &mut peak));
                }
                continue;
            }
            let family = tx(&case["family"]);
            let cat = catalog(&cfg, &family);
            let refs_mode = match family.as_str() {
                "qwen21" => "multi",
                "anima" | "moss" => "one",
                _ => "",
            };
            // 事件：題目有給 events 就照用，不然就是一句使用者的話（＋附圖）
            let user_imgs: Vec<String> = case["images"].as_array().into_iter().flatten().map(tx).collect();
            let mut events: Vec<Value> = match case.get("events").and_then(Value::as_array) {
                Some(e) => e.clone(),
                None => vec![json!({"kind": "user", "text": tx(&case["text"]),
                    "images": user_imgs.iter().enumerate().map(|(i, p)| json!({"n": i + 1, "name": p})).collect::<Vec<_>>()})],
            };
            if let Some(p) = &poses {
                for e in events.iter_mut().filter(|e| e["kind"] == "user") {
                    if let Some(mut n) = p.term_note(&tx(&e["text"])) {
                        // pose_brief：說明只給一句話的短版擺法（2026-10-29 比長短版）
                        if flag("pose_brief") {
                            n["geometry"] = n["brief"].clone();
                        }
                        e["pose"] = n;
                    }
                }
            }
            // 沒開 pose_note 但開了 pose_fix：補正要知道是哪個體位，事件留著 pose，只是不讓模型看到那行 Term
            let shown: Vec<Value> = if flag("pose_note") {
                events.clone()
            } else {
                events.iter().map(|e| {
                    let mut e = e.clone();
                    if let Some(m) = e.as_object_mut() {
                        m.remove("pose");
                    }
                    e
                }).collect()
            };
            let uimgs: Vec<(i64, Value)> = user_imgs.iter().enumerate().map(|(i, p)| (i as i64 + 1, img(p))).collect();
            let look: Vec<(i64, Value)> = case["look"].as_array().into_iter().flatten().map(|l| (l["id"].as_i64().unwrap(), img(&tx(&l["path"])))).collect();
            // skill：題目檔給外掛寫法的資料夾，寫法後面照生圖站接上挑好的段＋目錄（api_agent 的做法，2026-10-29 A/B）
            let mut guide = ag::default_text(ag::guide_id(&family)).unwrap().to_string();
            if let Some(s) = &skill {
                guide += "

";
                guide += &s.agent_block(ag::family_guide(&family), &ag::request_text(&events));
            }
            let mut system = ag::system_prompt(ag::default_text("agent").unwrap(), &guide, &family, &ag::loras_text(&cat, &[], true));
            if !user_imgs.is_empty() {
                system = ag::with_images(&system, refs_mode);
            }
            let rounds_left = case["rounds_left"].as_i64().unwrap_or(3);
            let uids: Vec<i64> = (1..=user_imgs.len() as i64).collect();
            let images: Vec<i64> = look.iter().map(|(i, _)| *i).collect();
            let last = ag::last_generate(&events).cloned();
            for run in 0..runs {
                let ask = ag::request_text(&events);
                let mut msgs = ag::build_messages_with(&system, &shown, &look, &uimgs, rounds_left, 2);
                let (mut nudged, mut rejects, mut first_ok, mut refused) = (false, 0usize, None, false);
                let (mut secs, mut toks, mut gen_ms) = (0.0, 0i64, 0.0);
                let mut final_action: Option<Map<String, Value>> = None;
                let mut last_raw = String::new();
                let mut fails = vec![];
                for attempt in 0..=ag::MAX_REJECTS {
                    let mut body = pe::tune_for(&tx(&m["file"]), pe::payload(&msgs, 0.7, 1000 + run * 7 + attempt as i64, false, Some(2048), false));
                    if flag("ban_soft") {
                        body = pe::ban_soft_words(body);
                    }
                    // 題目檔每顆模型可另給 "sampling"（例如模型卡建議的 repeat_penalty），蓋過生圖站的預設
                    for (k, v) in m["sampling"].as_object().into_iter().flatten() {
                        body[k] = v.clone();
                    }
                    let t0 = Instant::now();
                    let r: Value = match http.post(format!("http://127.0.0.1:{port}/v1/chat/completions")).json(&body).send().and_then(|r| r.json()) {
                        Ok(v) => v,
                        Err(e) => {
                            fails.push(format!("連線失敗：{e}"));
                            break;
                        }
                    };
                    secs += t0.elapsed().as_secs_f64();
                    peak = peak.max(vram_used().unwrap_or(0));
                    toks += r["usage"]["completion_tokens"].as_i64().unwrap_or(0);
                    gen_ms += r["timings"]["predicted_ms"].as_f64().unwrap_or(0.0);
                    let raw = pe::clean_reply(&tx(&r["choices"][0]["message"]["content"]));
                    last_raw = raw.clone();
                    let low = raw.to_lowercase();
                    if REFUSE.iter().any(|w| low.contains(w)) && !low.contains("\"action\"") {
                        refused = true;
                    }
                    let check = Check { family: &family, rounds_left, last: last.as_ref(), saw_images: !look.is_empty(), images: &images, catalog: &cat, fixed: &[],
                        allow_loras: true, nudged, user_images: &uids, request_user_images: &uids, refs_mode, ask: &ask,
                        calls_left: anima_app::agent_tools::MAX_CALLS - ag::calls_used(&events) };
                    match ag::parse_action(&raw, &check) {
                        Ok(a) => {
                            first_ok.get_or_insert(true);
                            final_action = Some(a);
                            break;
                        }
                        Err(fb) => {
                            first_ok.get_or_insert(false);
                            rejects += 1;
                            fails.push(fb.chars().take(160).collect());
                            nudged |= nudges(&fb);
                            if let Some(a) = msgs.as_array_mut() {
                                a.push(json!({"role": "assistant", "content": raw}));
                                a.push(json!({"role": "user", "content": format!("Rejected: {fb}")}));
                            }
                        }
                    }
                }
                if let (Some(a), Some(p), true) = (final_action.as_mut(), &poses, flag("pose_fix")) {
                    if a["action"] == "generate" {
                        let raw_prompt = tx(&a.get("prompt").cloned().unwrap_or_default());
                        let (mut used, msg) = ag::pose_fix(p, &events, &family, &raw_prompt);
                        if family == "anima" {
                            used = pe::fix_anima_tags(&used);
                        }
                        a.insert("raw_prompt".into(), json!(raw_prompt));
                        a.insert("pose_fix".into(), json!(msg));
                        a.insert("prompt".into(), json!(used));
                    }
                }
                let (pass, why) = match &final_action {
                    Some(a) => grade(&case["expect"], a),
                    None => (false, vec![format!("{} 次都寫不出能用的 JSON", rejects)]),
                };
                let mut why = why;
                if refused {
                    why.push("出現拒絕的字眼".into());
                }
                let excerpt = final_action.as_ref().map(|a| {
                    let p = tx(&a.get("prompt").cloned().unwrap_or_default());
                    let extra = format!("{} refs={} denoise={} loras={} best={} check={}", tx(&a["action"]), tx(a.get("refs").unwrap_or(&Value::Null)), tx(a.get("denoise").unwrap_or(&Value::Null)),
                        tx(a.get("loras").unwrap_or(&Value::Null)), tx(a.get("best").unwrap_or(&Value::Null)), tx(a.get("check").unwrap_or(&Value::Null)).chars().take(160).collect::<String>());
                    format!("{extra} | {}", p.chars().take(220).collect::<String>())
                }).unwrap_or_else(|| last_raw.chars().take(220).collect());
                let tps = if gen_ms > 0.0 { toks as f64 / (gen_ms / 1000.0) } else { 0.0 };
                let _ = writeln!(jl, "{}", json!({"model": name, "case": cid, "run": run, "pass": pass && !refused, "why": why, "rejects": rejects, "rejected_for": fails,
                    "secs": secs, "tokens": toks, "tps": tps, "action": final_action, "last_raw": last_raw}));
                eprintln!("{name} {cid}#{run}：{}（{secs:.1} 秒，退回 {rejects}）{}", if pass && !refused { "過" } else { "沒過" }, why.join("；"));
                rows.push(Row { model: name.clone(), case: cid.clone(), first_ok: first_ok == Some(true), final_ok: final_action.is_some(), pass: pass && !refused, refused, rejects, secs, tps, why: why.join("；"), excerpt });
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        std::thread::sleep(Duration::from_secs(3));
        let mine: Vec<&Row> = rows.iter().filter(|r| r.model == name).collect();
        let n = mine.len().max(1) as f64;
        model_notes.push(format!(
            "| {name} | {how}，載入 {load:.0} 秒 | 顯存 {}→最高 {} MiB | 一次就合格式 {}/{}、最後合格式 {}/{}、題目過 {}/{}、拒絕 {} | 平均 {:.1} 秒／題、{:.1} 字/秒 |",
            base_vram.unwrap_or(0), peak, mine.iter().filter(|r| r.first_ok).count(), mine.len(), mine.iter().filter(|r| r.final_ok).count(), mine.len(),
            mine.iter().filter(|r| r.pass).count(), mine.len(), mine.iter().filter(|r| r.refused).count(),
            mine.iter().map(|r| r.secs).sum::<f64>() / n, mine.iter().map(|r| r.tps).sum::<f64>() / n
        ));
    }

    // 成績單：總表＋每題每顆的結果（同一題多次跑的合在一格）
    let mut md = String::from("# 生圖助手擂台成績單\n\n## 總表\n\n| 模型 | 載入 | 顯存 | 格式／過關 | 速度 |\n|---|---|---|---|---|\n");
    md += &model_notes.join("\n");
    md += "\n\n## 每題結果（過＝評分全過；括號是退回次數）\n\n";
    let models: Vec<String> = cfg["models"].as_array().unwrap().iter().map(|m| tx(&m["name"])).collect();
    md += &format!("| 題目 | {} |\n|---|{}\n", models.join(" | "), "---|".repeat(models.len()));
    for case in cfg["cases"].as_array().unwrap() {
        let cid = tx(&case["id"]);
        let cells: Vec<String> = models.iter().map(|m| {
            rows.iter().filter(|r| &r.model == m && r.case == cid).map(|r| format!("{}({})", if r.pass { "過" } else { "✗" }, r.rejects)).collect::<Vec<_>>().join(" ")
        }).collect();
        md += &format!("| {cid} | {} |\n", cells.join(" | "));
    }
    md += "\n## 沒過的原因與摘錄\n\n";
    for r in rows.iter().filter(|r| !r.pass) {
        md += &format!("- **{} / {}**（{:.0} 秒）：{}\n  - {}\n", r.model, r.case, r.secs, r.why, r.excerpt.replace('\n', " "));
    }
    md += "\n## 改寫題的第一次輸出摘錄（每顆每題一筆）\n\n";
    let mut seen = std::collections::HashSet::new();
    for r in rows.iter().filter(|r| r.excerpt.starts_with('[')) {
        if seen.insert((r.model.clone(), r.case.clone())) {
            md += &format!("- **{} / {}**（{:.0} 秒）：{}\n", r.model, r.case, r.secs, r.excerpt.replace('\n', " "));
        }
    }
    md += "\n## NSFW 題的提示詞摘錄（看寫得夠不夠直白）\n\n";
    for r in rows.iter().filter(|r| r.case.starts_with("nsfw") && !r.excerpt.starts_with('[')) {
        md += &format!("- **{} / {}**：{}\n", r.model, r.case, r.excerpt.replace('\n', " "));
    }
    std::fs::write(out.join("summary.md"), md).unwrap();
    eprintln!("完成：{}", out.join("summary.md").display());
}
