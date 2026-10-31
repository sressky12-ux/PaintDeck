//! 拍攝單（2026-10-24，子專案 2）：影片台上面那張表。角色、首幀、參考圖、場景、鏡位、聲音、每段的時間軸，
//! 程式照固定結構組成 H3 提示詞（中文或英文），送出前健檢，開拍前預估。這裡只有純邏輯；API 在 server\api_shotsheet.rs。
//! 設計在 design\資產庫與影片台合併_設計.md 第 5 節，計畫在 design\影片台合併_實作計畫.md。

use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

pub const MIN_SECS: f64 = 5.0;
pub const MAX_SECS: f64 = 15.0;
pub const MAX_SEGS: usize = 30;
pub const MAX_CAST: usize = 4;
pub const MAX_REFS: usize = 8;
const MAX_BEATS: usize = 40;
/// 第 2 段起，開頭這段是上一段尾巴的重疊（接片時切掉），不安排新動作
pub const TAIL_SECS: f64 = 1.6;
/// 後面還有段的話，最後這段留給下一段接手，不安排新動作
pub const HANDOVER_SECS: f64 = 0.7;
/// 「提早」：H3 動作常比提示詞晚一點出來，時間整體往前挪
pub const EARLY_SECS: f64 = 1.0;

/// 場景的物件位置：鍵、中文、英文
const LAYOUT_LABELS: &[(&str, &str, &str)] = &[
    ("top", "上方", "top"),
    ("bottom", "下方", "bottom"),
    ("left", "左邊", "left"),
    ("right", "右邊", "right"),
    ("fg", "前景", "foreground"),
    ("mid", "中景", "midground"),
    ("bg", "遠景", "background"),
    ("light", "光線", "light"),
];

/// 物件位置那一行：中文「位置：上方 書桌；左邊 窗戶」，英文「Position: top: desk; left: window」；沒填回空字串
pub fn layout_line(layout: &Value, zh: bool) -> String {
    let parts: Vec<String> = LAYOUT_LABELS
        .iter()
        .filter_map(|(k, z, e)| {
            let v = layout[*k].as_str().unwrap_or("").trim();
            (!v.is_empty()).then(|| if zh { format!("{z} {v}") } else { format!("{e}: {v}") })
        })
        .collect();
    match (parts.is_empty(), zh) {
        (true, _) => String::new(),
        (false, true) => format!("位置：{}", parts.join("；")),
        (false, false) => format!("Position: {}", parts.join("; ")),
    }
}

/// 台詞語言代碼 → H3 認的英文名；已經是英文名就照用，空的或認不得的當中文
pub fn lang_name(code: &str) -> &'static str {
    const NAMES: &[(&str, &[&str])] = &[
        ("Chinese", &["zh", "cn", "chinese", "中文", "國語", "普通話"]),
        ("Japanese", &["ja", "jp", "japanese", "日文", "日語"]),
        ("English", &["en", "english", "英文", "英語"]),
        ("Korean", &["ko", "kr", "korean", "韓文", "韓語"]),
        ("Cantonese", &["yue", "cantonese", "粵語", "廣東話"]),
    ];
    let c = code.trim().to_lowercase();
    NAMES.iter().find(|(_, alias)| alias.contains(&c.as_str())).map(|(n, _)| *n).unwrap_or("Chinese")
}

fn txt(v: &Value, max: usize) -> String {
    v.as_str().unwrap_or("").trim().chars().take(max).collect()
}

fn num(v: &Value) -> f64 {
    v.as_f64().filter(|x| x.is_finite()).unwrap_or(0.0)
}

/// 毫秒取整，避免 10.0-0.7 這種浮點尾巴
fn r3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn file_field(v: &Value, what: &str) -> Result<Value, String> {
    match v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(Value::Null),
        Some(n) if crate::h3chain::file_ok(n) => Ok(json!(n)),
        Some(n) => Err(format!("{what}的檔名不對：{n}（素材要從格子放，不能手填路徑）")),
    }
}

fn id_field(v: &Value) -> Value {
    v.as_i64().map(Value::from).unwrap_or(Value::Null)
}

/// 整理成固定形狀：補預設、秒數夾在 5～15、最多 30 段、角色依順序配 S1～S4、檔名只收 input 裡的檔名
pub fn normalize(v: &Value) -> Result<Value, String> {
    let cast: Vec<Value> = v["cast"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .take(MAX_CAST)
        .enumerate()
        .map(|(i, c)| {
            json!({"asset_id": id_field(&c["asset_id"]), "speaker": format!("S{}", i + 1), "name": txt(&c["name"], 40),
                   "look_zh": txt(&c["look_zh"], 2000), "look_en": txt(&c["look_en"], 2000),
                   "voice": txt(&c["voice"], 200), "lang": txt(&c["lang"], 30), "locked": c["locked"].as_bool().unwrap_or(false),
                   "refmod": Some(txt(&c["refmod"], 120)).filter(|n| crate::refmod::valid_name(n)).unwrap_or_default()})
        })
        .collect();
    let first = json!({"file": file_field(&v["first"]["file"], "首幀")?,
                       "asset_id": id_field(&v["first"]["asset_id"]), "gallery_id": id_field(&v["first"]["gallery_id"])});
    let mut items = Vec::new();
    for it in v["refs"]["items"].as_array().map(|a| a.as_slice()).unwrap_or_default().iter().take(MAX_REFS) {
        items.push(json!({"file": file_field(&it["file"], "參考圖")?, "asset_id": id_field(&it["asset_id"]), "who": txt(&it["who"], 4)}));
    }
    let mut layout = Map::new();
    for (k, _, _) in LAYOUT_LABELS {
        layout.insert(k.to_string(), json!(txt(&v["scene"]["layout"][*k], 300)));
    }
    let mut segments: Vec<Value> = v["segments"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .take(MAX_SEGS)
        .map(|s| {
            let secs = s["secs"].as_f64().filter(|x| x.is_finite()).unwrap_or(10.0).clamp(MIN_SECS, MAX_SECS);
            let beats: Vec<Value> = s["beats"]
                .as_array()
                .map(|a| a.as_slice())
                .unwrap_or_default()
                .iter()
                .take(MAX_BEATS)
                .map(|b| {
                    json!({"from": r3(num(&b["from"])), "to": r3(num(&b["to"])), "action": txt(&b["action"], 1000),
                           "who": txt(&b["who"], 4), "lang": txt(&b["lang"], 30), "line": txt(&b["line"], 500), "sfx": txt(&b["sfx"], 300)})
                })
                .collect();
            // 這段不送的參考圖（記檔名；換了圖就回到「要送」）
            let ref_off: Vec<Value> = s["ref_off"].as_array().map(|a| a.as_slice()).unwrap_or_default().iter()
                .filter_map(|f| file_field(f, "參考圖").ok().filter(Value::is_string)).take(MAX_REFS).collect();
            json!({"secs": secs, "beats": beats, "prompt": txt(&s["prompt"], 20000), "mirror": txt(&s["mirror"], 20000),
                   "manual": s["manual"].as_bool().unwrap_or(false), "ref_off": ref_off,
                   "entry": crate::shot_spec::normalize_state(&s["entry"]), "exit": crate::shot_spec::normalize_state(&s["exit"]),
                   "inherit_entry": s["inherit_entry"].as_bool().unwrap_or(true)})
        })
        .collect();
    if segments.is_empty() {
        segments.push(json!({"secs": 10.0, "beats": [], "prompt": "", "mirror": "", "manual": false,
            "entry":crate::shot_spec::normalize_state(&Value::Null),"exit":crate::shot_spec::normalize_state(&Value::Null),"inherit_entry":true}));
    }
    Ok(json!({
        "v": 1, "engine":crate::shot_spec::normalize_engine(&v["engine"]), "title": txt(&v["title"], 100), "lang": if v["lang"] == "en" { "en" } else { "zh" },
        "cast": cast, "first": first, "refs": {"on": v["refs"]["on"].as_bool().unwrap_or(false), "items": items},
        "scene": {"text": txt(&v["scene"]["text"], 2000), "layout": layout},
        "style": txt(&v["style"], 1000), "camera": txt(&v["camera"], 1000),
        "sound": {"ambient": txt(&v["sound"]["ambient"], 1000), "music": txt(&v["sound"]["music"], 1000)},
        "early": v["early"].as_bool().unwrap_or(true), "segments": segments, "refmod": v["refmod"].as_bool().unwrap_or(false),
    }))
}

/// 存成範本（資產庫 kind=shot）：拿掉這次才有的檔案和寫好的提示詞，只留資產連結和時間軸
pub fn template_of(sheet: &Value) -> Value {
    let mut t = sheet.clone();
    if let Some(f) = t.get_mut("first").and_then(Value::as_object_mut) {
        f.remove("file");
        f.remove("gallery_id");
    }
    if let Some(items) = t.pointer_mut("/refs/items").and_then(Value::as_array_mut) {
        for o in items.iter_mut().filter_map(Value::as_object_mut) {
            o.remove("file");
        }
    }
    if let Some(segs) = t.get_mut("segments").and_then(Value::as_array_mut) {
        for o in segs.iter_mut().filter_map(Value::as_object_mut) {
            for k in ["prompt", "mirror", "manual", "ref_off"] {
                o.remove(k);
            }
        }
    }
    t
}

fn seg_count(sheet: &Value) -> usize {
    sheet["segments"].as_array().map_or(0, |a| a.len())
}

/// 第 i 段能安排新動作的範圍：第 2 段起扣掉開頭的重疊，後面還有段就扣掉交棒區
pub fn region(sheet: &Value, i: usize) -> (f64, f64) {
    let secs = num(&sheet["segments"][i]["secs"]);
    if sheet["engine"].as_str().is_some_and(|e| !e.is_empty() && e != "h3") { return (0.0, secs); }
    let lo = if i > 0 { TAIL_SECS } else { 0.0 };
    let hi = if i + 1 < seg_count(sheet) { secs - HANDOVER_SECS } else { secs };
    (lo, r3(hi))
}

/// 秒數 → 00:01.600
pub fn ts(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as i64;
    format!("{:02}:{:02}.{:03}", ms / 60000, (ms / 1000) % 60, ms % 1000)
}

/// 這一拍實際寫進提示詞的時間：「提早」就往前挪，再夾在能安排的範圍內
pub fn beat_time(sheet: &Value, i: usize, beat: &Value) -> (f64, f64) {
    let (lo, hi) = region(sheet, i);
    let h3 = sheet["engine"].as_str().is_none_or(|e| e.is_empty() || e == "h3");
    let shift = if h3 && sheet["early"].as_bool().unwrap_or(true) { EARLY_SECS } else { 0.0 };
    let clamp = |t: f64| r3((t - shift).clamp(lo, hi));
    (clamp(num(&beat["from"])), clamp(num(&beat["to"])))
}

/// 真的有圖的參考圖（範本讀回來、還沒放圖的不算）
pub fn ref_items(sheet: &Value) -> Vec<&Value> {
    sheet["refs"]["items"].as_array().map(|a| a.iter().filter(|x| !s(&x["file"]).is_empty()).collect()).unwrap_or_default()
}

/// 第 i 段實際送的參考圖：拿掉這段關掉的（`ref_off`），圖號照剩下的順序排
pub fn seg_ref_items(sheet: &Value, i: usize) -> Vec<&Value> {
    let off = sheet["segments"][i]["ref_off"].as_array();
    ref_items(sheet).into_iter().filter(|x| !off.is_some_and(|a| a.contains(&x["file"]))).collect()
}

fn cast_of<'a>(sheet: &'a Value, speaker: &str) -> Option<&'a Value> {
    sheet["cast"].as_array()?.iter().find(|c| !speaker.is_empty() && c["speaker"] == speaker)
}

/// 「小克（S1）」／「小克 (S1)」；找不到人回「這個角色」
fn who_label(sheet: &Value, speaker: &str, zh: bool) -> String {
    match cast_of(sheet, speaker) {
        Some(c) => {
            let (name, sp) = (c["name"].as_str().unwrap_or(""), c["speaker"].as_str().unwrap_or(""));
            match (name.is_empty(), zh) {
                (true, _) => sp.to_string(),
                (false, true) => format!("{name}（{sp}）"),
                (false, false) => format!("{name} ({sp})"),
            }
        }
        None if zh => "這個角色".into(),
        None => "this character".into(),
    }
}

/// 句尾沒標點就補
fn end(s: &str, zh: bool) -> String {
    if s.ends_with(['。', '！', '？', '.', '!', '?', '…']) {
        s.to_string()
    } else {
        format!("{s}{}", if zh { "。" } else { "." })
    }
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

fn cast_of_kind(sheet: &Value, locked: bool) -> impl Iterator<Item = &Value> {
    sheet["cast"].as_array().map(|a| a.as_slice()).unwrap_or_default().iter().filter(move |c| c["locked"].as_bool().unwrap_or(false) == locked)
}

/// 角色外觀那一句：「S1（小克）：…。」／「小克 (S1): ….」；沒寫外觀回 None
fn cast_line(c: &Value, zh: bool) -> Option<String> {
    let (own, other) = if zh { ("look_zh", "look_en") } else { ("look_en", "look_zh") };
    let look = Some(s(&c[own])).filter(|x| !x.is_empty()).unwrap_or(s(&c[other]));
    if look.is_empty() {
        return None;
    }
    let (name, sp) = (s(&c["name"]), s(&c["speaker"]));
    Some(match (name.is_empty(), zh) {
        (true, true) => end(&format!("{sp}：{look}"), zh),
        (true, false) => end(&format!("{sp}: {look}"), zh),
        (false, true) => end(&format!("{sp}（{name}）：{look}"), zh),
        (false, false) => end(&format!("{name} ({sp}): {look}"), zh),
    })
}

/// 鎖定角色（資產庫鎖住的設定表）那幾句，照角色順序接起來。每段描述最前面原封不動放上；AI 潤飾前拿掉、潤完接回
pub fn locked_head(sheet: &Value, zh: bool) -> String {
    cast_of_kind(sheet, true).filter_map(|c| cast_line(c, zh)).collect::<Vec<_>>().join(if zh { "" } else { " " })
}

/// 第 i 段沒出場的角色（S1、S2…）：有綁參考圖、而且綁的圖這段全部關掉了。
/// 只關圖、外觀那句還在的話，H3 照樣把人畫出來（2026-10-27 雲端實測：第 1 段開頭冒出關掉的那個人），所以圖和外觀一起拿掉
pub fn absent(sheet: &Value, i: usize) -> Vec<String> {
    if !sheet["refs"]["on"].as_bool().unwrap_or(false) {
        return vec![];
    }
    let used = seg_ref_items(sheet, i);
    let all = ref_items(sheet);
    sheet["cast"].as_array().map(|a| a.as_slice()).unwrap_or_default().iter()
        .map(|c| s(&c["speaker"]))
        .filter(|sp| !sp.is_empty() && all.iter().any(|r| s(&r["who"]) == *sp) && !used.iter().any(|r| s(&r["who"]) == *sp))
        .map(str::to_string)
        .collect()
}

/// 第 i 段的鎖定段落：只放這段有出場的鎖定角色
pub fn locked_head_at(sheet: &Value, i: usize, zh: bool) -> String {
    let gone = absent(sheet, i);
    cast_of_kind(sheet, true).filter(|c| !gone.iter().any(|g| g == s(&c["speaker"]))).filter_map(|c| cast_line(c, zh)).collect::<Vec<_>>().join(if zh { "" } else { " " })
}

const DESC_KEY: &str = "integrated_multimodal_description:";

/// 拿掉提示詞裡的鎖定段落（第一次出現的那份）
pub fn strip_locked(prompt: &str, head: &str) -> String {
    if head.is_empty() {
        return prompt.to_string();
    }
    match prompt.find(head) {
        Some(p) => {
            let rest = &prompt[p + head.len()..];
            let rest = if prompt[..p].ends_with(' ') || prompt[..p].is_empty() { rest.trim_start_matches([' ', '\n']) } else { rest };
            format!("{}{rest}", &prompt[..p])
        }
        None => prompt.to_string(),
    }
}

/// 把鎖定段落放回去：H3 放在 integrated_multimodal_description 後面第一句，其他引擎放第一行；已經有就不重複
pub fn put_locked(prompt: &str, head: &str, zh: bool) -> String {
    if head.is_empty() {
        return prompt.to_string();
    }
    let body = strip_locked(prompt, head);
    if let Some(p) = body.find(DESC_KEY) {
        let after = p + DESC_KEY.len();
        let rest = body[after..].trim_start_matches(' ');
        let sep = if rest.is_empty() || zh { "" } else { " " };
        return format!("{}{DESC_KEY} {head}{sep}{rest}", &body[..p]);
    }
    let rest = body.trim_start();
    if rest.is_empty() { head.to_string() } else { format!("{head}\n{rest}") }
}

/// 第 i 段的 H3 提示詞（lang = "en" 寫英文，其他寫中文）
pub fn compose(sheet: &Value, i: usize, lang: &str) -> String {
    let zh = lang != "en";
    let n = seg_count(sheet);
    let seg = &sheet["segments"][i];
    let secs = num(&seg["secs"]);
    let has_first = i == 0 && !s(&sheet["first"]["file"]).is_empty();
    let mut out = String::new();
    if has_first {
        out.push_str(if zh {
            "目標影片的第 0.00 秒完全參考 <Picture 1>（[Shot 1]）。\n\n"
        } else {
            "For the target video, at 0.00 seconds into the target video, <Picture 1> (from [Shot 1]) is fully referenced.\n\n"
        });
    }
    let mut body: Vec<String> = Vec::new();
    let head = locked_head_at(sheet, i, zh);
    if !head.is_empty() {
        body.push(head);
    }
    let gone = absent(sheet, i);
    let entry = crate::shot_spec::state_lines(&crate::shot_spec::effective_entry(sheet, i), lang);
    if !entry.is_empty() { body.push(format!("{}: {entry}", if zh { "開始狀態" } else { "Entry state" })); }
    if i > 0 {
        body.push(if zh { CONT_ZH } else { CONT_EN }.into());
    }
    let style = s(&sheet["style"]);
    if !style.is_empty() {
        body.push(if zh { end(&format!("畫風：{style}"), zh) } else { end(&format!("Style: {style}"), zh) });
    }
    let camera = s(&sheet["camera"]);
    if !camera.is_empty() {
        body.push(if zh { end(&format!("鏡位與運鏡：{camera}"), zh) } else { end(&format!("Camera: {camera}"), zh) });
    }
    body.extend(cast_of_kind(sheet, false).filter(|c| !gone.iter().any(|g| g == s(&c["speaker"]))).filter_map(|c| cast_line(c, zh)));
    let scene = s(&sheet["scene"]["text"]);
    if !scene.is_empty() {
        body.push(if zh { end(&format!("場景：{scene}"), zh) } else { end(&format!("Scene: {scene}"), zh) });
    }
    let layout = layout_line(&sheet["scene"]["layout"], zh);
    if !layout.is_empty() {
        body.push(end(&layout, zh));
    }
    if sheet["refs"]["on"].as_bool().unwrap_or(false) {
        let base = if has_first { 2 } else { 1 };
        for (k, it) in seg_ref_items(sheet, i).into_iter().enumerate() {
            let who = who_label(sheet, s(&it["who"]), zh);
            let p = base + k;
            body.push(if zh {
                format!("<Picture {p}> 只用來參考 {who}的長相（臉、髮型、服裝、身材）{REF_TAIL_ZH}")
            } else {
                format!("<Picture {p}> is used only for the look of {who}{REF_TAIL_EN}")
            });
        }
    }
    if i > 0 {
        let (a, b) = (ts(0.0), ts(TAIL_SECS));
        body.push(if zh {
            format!("從 {a} 到 {b}，延續上一段的同一個動作。")
        } else {
            format!("From {a} to {b}, the same action from the previous clip continues.")
        });
    }
    let mut beats: Vec<&Value> = seg["beats"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    beats.sort_by(|a, b| num(&a["from"]).total_cmp(&num(&b["from"])));
    for beat in beats {
        let (a, b) = beat_time(sheet, i, beat);
        let (action, line, sfx) = (s(&beat["action"]), s(&beat["line"]), s(&beat["sfx"]));
        if b <= a || (action.is_empty() && line.is_empty() && sfx.is_empty()) {
            continue;
        }
        let mut parts: Vec<String> = Vec::new();
        if !action.is_empty() {
            parts.push(end(action, zh));
        }
        if !line.is_empty() {
            let who = s(&beat["who"]);
            let cast = cast_of(sheet, who);
            let code = Some(s(&beat["lang"])).filter(|x| !x.is_empty()).or_else(|| cast.map(|c| s(&c["lang"]))).unwrap_or("");
            let lname = lang_name(code);
            let voice = cast.map(|c| s(&c["voice"])).unwrap_or("");
            let label = who_label(sheet, who, zh);
            parts.push(match (voice.is_empty(), zh) {
                (true, true) => format!("{label}說：<d>[{lname}] {line}</d>"),
                (true, false) => format!("{} says: <d>[{lname}] {line}</d>", cap(&label)),
                (false, true) => format!("{label}用「{voice}」的聲音說：<d>[{lname}] {line}</d>"),
                (false, false) => format!("{} says in {voice}: <d>[{lname}] {line}</d>", cap(&label)),
            });
        }
        if !sfx.is_empty() {
            parts.push(if zh { end(&format!("音效：{sfx}"), zh) } else { end(&format!("Sound: {sfx}"), zh) });
        }
        let (a, b) = (ts(a), ts(b));
        body.push(if zh { format!("從 {a} 到 {b}，{}", parts.join("")) } else { format!("From {a} to {b}, {}", parts.join(" ")) });
    }
    if i + 1 < n {
        let h = ts(secs - HANDOVER_SECS);
        body.push(if zh { format!("從 {h} 到結尾，動作繼續。") } else { format!("From {h} to the end, the action continues.") });
    }
    let exit = crate::shot_spec::state_lines(&seg["exit"], lang);
    if !exit.is_empty() { body.push(format!("{}: {exit}", if zh { "結束狀態" } else { "Exit state" })); }
    out.push_str("integrated_multimodal_description: ");
    out.push_str(&body.join(if zh { "" } else { " " }));
    let ambient = s(&sheet["sound"]["ambient"]);
    let music = s(&sheet["sound"]["music"]);
    let ambient = if !ambient.is_empty() {
        ambient.to_string()
    } else if zh {
        "符合場景的自然環境音。".into()
    } else {
        "Natural ambient sound that matches the scene.".into()
    };
    out.push_str(&format!("\n\noverall_soundscape: {ambient}\n\nnon_diegetic_music: {}", if music.is_empty() { "N/A" } else { music }));
    out
}

// ---------------------------------------------------------------- 健檢與預估
/// 程式自己寫的固定句（裡面的 no／not 是官方寫法），健檢時先拿掉
const CONT_ZH: &str = "目標影片從上一段的最後一刻直接接著拍，不切鏡、不重來。";
const CONT_EN: &str = "The target video continues directly from the exact final moment of the previous clip with no cut or reset.";
const REF_TAIL_ZH: &str = "，不決定姿勢、鏡位和構圖。";
const REF_TAIL_EN: &str = " — face, hair, outfit and body — not for pose, camera or composition.";

static RULES_JSON: OnceLock<Value> = OnceLock::new();
static ESTIMATE_JSON: OnceLock<Value> = OnceLock::new();

/// resources\h3\sheet_rules.json（字表、鏡位按鈕）
pub fn rules_json() -> &'static Value {
    RULES_JSON.get_or_init(|| serde_json::from_str(include_str!("../../resources/h3/sheet_rules.json")).unwrap_or_default())
}

/// resources\h3\estimate.json（開機秒數、每張卡的速度和價錢）
pub fn estimate_json() -> &'static Value {
    ESTIMATE_JSON.get_or_init(|| serde_json::from_str(include_str!("../../resources/h3/estimate.json")).unwrap_or_default())
}

pub struct Rules {
    pub mood: Vec<String>,
    pub prevent: Vec<String>,
    pub prevent_re: Vec<Regex>,
    pub prevent_skip: Vec<String>,
    pub danger: Vec<String>,
    pub body: Vec<String>,
    pub position: Vec<String>,
    pub direction: Vec<String>,
}

fn words(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|x| x.as_str())
        .map(|x| x.trim().to_lowercase())
        .filter(|x| !x.is_empty())
        .collect()
}

/// 內建字表＋使用者自己加的危險字
pub fn rules(extra_danger: &[String]) -> Rules {
    let j = rules_json();
    let mut danger = words(&j["danger"]);
    for w in extra_danger.iter().map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty()) {
        if !danger.contains(&w) {
            danger.push(w);
        }
    }
    Rules {
        mood: words(&j["mood"]),
        prevent: words(&j["prevent"]),
        prevent_re: words(&j["prevent_re"]).iter().filter_map(|r| Regex::new(&format!("(?i){r}")).ok()).collect(),
        prevent_skip: words(&j["prevent_skip"]),
        danger,
        body: words(&j["body"]),
        position: words(&j["position"]),
        direction: words(&j["direction"]),
    }
}

/// 英文整字比對（前後不能接英數字）；中文直接找。hay 和 w 都要先轉小寫。回傳第一個符合的位置
fn find_word(hay: &str, w: &str) -> Option<usize> {
    if w.is_empty() {
        return None;
    }
    if !w.is_ascii() {
        return hay.find(w);
    }
    let edge = |c: Option<char>| c.is_none_or(|c| !c.is_ascii_alphanumeric());
    hay.match_indices(w).find(|(i, _)| edge(hay[..*i].chars().next_back()) && edge(hay[i + w.len()..].chars().next())).map(|(i, _)| i)
}

fn hits<'a>(hay: &str, list: &'a [String]) -> Vec<&'a str> {
    list.iter().filter(|w| find_word(hay, w).is_some()).map(|w| w.as_str()).collect()
}

fn dialogue_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)<d>.*?</d>").unwrap())
}

/// 送出前健檢。prompts[i] 是第 i 段實際要送的提示詞（可能手改過）。全部只是提醒，8 是 error 但一樣可以照送
pub fn check(sheet: &Value, prompts: &[String], r: &Rules) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let n = seg_count(sheet);
    let refs_on = sheet["refs"]["on"].as_bool().unwrap_or(false);
    let h3 = sheet["engine"].as_str().is_none_or(|e| e.is_empty() || e == "h3");
    for i in 0..n {
        let seg = &sheet["segments"][i];
        let secs = num(&seg["secs"]);
        let prompt = prompts.get(i).map(String::as_str).unwrap_or("");
        let low = prompt.to_lowercase();
        for w in if h3 { hits(&low, &r.mood) } else { vec![] } {
            out.push(json!({"seg": i, "rule": 1, "level": "warn", "word": w, "msg": "情緒形容詞 H3 演不出來，換成看得到的動作或表情"}));
        }
        let mut plain = dialogue_re().replace_all(&low, " ").into_owned();
        for f in [CONT_ZH, CONT_EN, REF_TAIL_ZH, REF_TAIL_EN].iter().map(|f| f.to_lowercase()).chain(r.prevent_skip.iter().cloned()) {
            plain = plain.replace(&f, " ");
        }
        for w in hits(&plain, &r.prevent) {
            out.push(json!({"seg": i, "rule": 2, "level": "warn", "word": w, "msg": "寫「不要什麼」常常反而被演出來，改寫成要它做的事"}));
        }
        if let Some(m) = r.prevent_re.iter().find_map(|re| re.find(&plain)) {
            out.push(json!({"seg": i, "rule": 2, "level": "warn", "word": m.as_str(), "msg": "寫「不要什麼」常常反而被演出來，改寫成要它做的事"}));
        }
        for w in hits(&low, &r.danger) {
            out.push(json!({"seg": i, "rule": 3, "level": "warn", "word": w, "msg": "這個字容易讓畫面多出不想要的東西（危險字清單）"}));
        }
        let ref_count = seg_ref_items(sheet, i).len();
        if h3 && refs_on && ref_count > 0 && !prompt.is_empty() {
            let base = if i == 0 && !s(&sheet["first"]["file"]).is_empty() { 2 } else { 1 };
            if let Some(p) = (base..base + ref_count).find(|p| !prompt.contains(&format!("<Picture {p}>"))) {
                out.push(json!({"seg": i, "rule": 7, "level": "warn", "word": format!("<Picture {p}>"), "msg": "有勾參考圖，但提示詞裡找不到對應的圖號（手改時刪掉了？）"}));
            }
        }
        let (lo, hi) = region(sheet, i);
        let mut beats: Vec<&Value> = seg["beats"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
        beats.sort_by(|a, b| num(&a["from"]).total_cmp(&num(&b["from"])));
        let mut prev_to: Option<f64> = None;
        for b in beats {
            let (from, to) = (num(&b["from"]), num(&b["to"]));
            let span = format!("{}～{}", ts(from), ts(to));
            if to > hi + 0.001 || from < lo - 0.001 {
                out.push(json!({"seg": i, "rule": 4, "level": "warn", "word": span, "msg": "這截跑進了接片用的尾巴區或交棒區，會被切掉或演不完"}));
            }
            let action = s(&b["action"]).to_lowercase();
            if !action.is_empty() {
                let anchored = |t: &str| !hits(t, &r.body).is_empty() || !hits(t, &r.position).is_empty();
                if h3 && !anchored(&action) {
                    out.push(json!({"seg": i, "rule": 5, "level": "warn", "word": s(&b["action"]), "msg": "動作沒寫是哪個部位、往哪裡，H3 容易亂演"}));
                }
                let dir = r.direction.iter().filter_map(|w| find_word(&action, w).map(|p| (w, p))).min_by_key(|(_, p)| *p);
                if let Some((w, p)) = dir {
                    if !anchored(&action[p + w.len()..]) {
                        out.push(json!({"seg": i, "rule": 6, "level": "warn", "word": w, "msg": "有方向的動作後面要寫清楚朝哪個部位或位置"}));
                    }
                }
            }
            // 訊息寫成 "msg": 才抽得到翻譯（tools\i18n_extract.py）
            let (effective_from, effective_to) = beat_time(sheet, i, b);
            let bad_time = if effective_to <= effective_from && to > from {
                Some(json!({"msg": "這截提早後沒有剩餘時間，請調整時間或關閉自動提早"}))
            } else if to <= from {
                Some(json!({"msg": "這截的結束時間沒有比開始晚"}))
            } else if to > secs + 0.001 {
                Some(json!({"msg": "這截超過這段的長度"}))
            } else {
                match prev_to {
                    Some(p) if from < p - 0.001 => Some(json!({"msg": "這截跟前一截的時間重疊"})),
                    Some(p) if h3 && from - p > 0.1 + 0.001 => Some(json!({"msg": "這截跟前一截中間空了一段，那段 H3 會自己亂演"})),
                    _ => None,
                }
            };
            if let Some(m) = bad_time {
                out.push(json!({"seg": i, "rule": 8, "level": "error", "word": span, "msg": m["msg"]}));
            }
            prev_to = Some(prev_to.map_or(to, |p| p.max(to)));
        }
    }
    out.sort_by_key(|x| (x["seg"].as_u64().unwrap_or(0), x["rule"].as_u64().unwrap_or(0)));
    out
}

/// 開拍預估：每張卡 {gpu, secs, usd}。secs = 開機 + Σ 每 10 秒要算的秒數 × 段長 / 10
pub fn estimate(sheet: &Value) -> Vec<Value> {
    let j = estimate_json();
    let total: f64 = sheet["segments"].as_array().map(|a| a.iter().map(|x| num(&x["secs"])).sum()).unwrap_or(0.0);
    j["gpus"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|g| {
            let secs = (num(&j["boot_secs"]) + num(&g["secs_per_10s"]) * total / 10.0).round();
            let usd = (secs / 3600.0 * num(&g["usd_per_hour"]) * 100.0).round() / 100.0;
            json!({"gpu": g["name"], "secs": secs, "usd": usd})
        })
        .collect()
}

/// 英文句首大寫（「this character says」放句首時）
fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn review_collapsed_beat_warns_and_compose_sorts() {
        let sh = normalize(&json!({"segments":[{"secs":10,"beats":[{"from":0,"to":1,"action":"右手放在桌上"}]}]})).unwrap();
        assert!(check(&sh, &[compose(&sh,0,"zh")], &rules(&[])).iter().any(|x| x["rule"] == 8));
        let sh = normalize(&json!({"early":false,"segments":[{"beats":[{"from":5,"to":8,"action":"later"},{"from":0,"to":5,"action":"earlier"}]}]})).unwrap();
        for lang in ["zh","en"] { let p=compose(&sh,0,lang); assert!(p.find("earlier") < p.find("later")); }
    }

    use super::*;
    use serde_json::json;

    fn cast3() -> Value {
        json!([{"name": "甲"}, {"name": "乙"}, {"name": "丙"}])
    }

    #[test]
    fn normalize_fills_defaults_and_clamps() {
        let s = normalize(&json!({})).unwrap();
        assert_eq!(s["segments"].as_array().unwrap().len(), 1);
        assert_eq!(s["segments"][0]["secs"], 10.0);
        assert_eq!(s["lang"], "zh");
        assert_eq!(s["early"], true);
        let s = normalize(&json!({"segments": [{"secs": 30}], "lang": "xx"})).unwrap();
        assert_eq!(s["segments"][0]["secs"], 15.0);
        assert_eq!(s["lang"], "zh");
        let s = normalize(&json!({"cast": cast3()})).unwrap();
        let sp: Vec<&str> = s["cast"].as_array().unwrap().iter().map(|c| c["speaker"].as_str().unwrap()).collect();
        assert_eq!(sp, ["S1", "S2", "S3"]);
        assert!(normalize(&json!({"first": {"file": "..\\a.png"}})).is_err());
        assert!(normalize(&json!({"refs": {"on": true, "items": [{"file": "a/b.png"}]}})).is_err());
        let many: Vec<Value> = (0..40).map(|_| json!({})).collect();
        assert_eq!(normalize(&json!({"segments": many})).unwrap()["segments"].as_array().unwrap().len(), MAX_SEGS);
    }

    #[test]
    fn lang_name_maps_codes() {
        assert_eq!(lang_name("ja"), "Japanese");
        assert_eq!(lang_name("yue"), "Cantonese");
        assert_eq!(lang_name("Japanese"), "Japanese");
        assert_eq!(lang_name(""), "Chinese");
        assert_eq!(lang_name("klingon"), "Chinese");
    }

    #[test]
    fn region_has_tail_and_handover() {
        let s = normalize(&json!({"segments": [{"secs": 10}, {"secs": 10}, {"secs": 10}]})).unwrap();
        assert_eq!(region(&s, 0), (0.0, 9.3));
        assert_eq!(region(&s, 1), (1.6, 9.3));
        assert_eq!(region(&s, 2), (1.6, 10.0));
        let one = normalize(&json!({})).unwrap();
        assert_eq!(region(&one, 0), (0.0, 10.0));
        assert_eq!(ts(1.6), "00:01.600");
        assert_eq!(ts(65.25), "01:05.250");
    }

    #[test]
    fn early_shift_clamps_to_region() {
        let mk = |early: bool| {
            normalize(&json!({"early": early, "segments": [
                {"secs": 10, "beats": [{"from": 0.5, "to": 3.0, "action": "她右手舉到臉旁"}]},
                {"secs": 10, "beats": [{"from": 2.0, "to": 4.0, "action": "她左手放到桌上"}]}]}))
            .unwrap()
        };
        let s = mk(true);
        assert!(compose(&s, 0, "zh").contains("從 00:00.000 到 00:02.000，她右手舉到臉旁。"), "{}", compose(&s, 0, "zh"));
        assert!(compose(&s, 1, "zh").contains("從 00:01.600 到 00:03.000，她左手放到桌上。"), "{}", compose(&s, 1, "zh"));
        let s = mk(false);
        assert!(compose(&s, 0, "zh").contains("從 00:00.500 到 00:03.000"));
        assert!(compose(&s, 1, "zh").contains("從 00:02.000 到 00:04.000"));
    }

    #[test]
    fn picture_numbers_per_segment() {
        let refs = json!({"on": true, "items": [{"file": "r1.png", "who": "S1"}, {"file": "r2.png", "who": "S2"}]});
        let s = normalize(&json!({"cast": cast3(), "first": {"file": "f.png"}, "refs": refs, "segments": [{}, {}]})).unwrap();
        let p0 = compose(&s, 0, "zh");
        let p1 = compose(&s, 1, "zh");
        assert!(p0.contains("<Picture 2> 只用來參考 甲（S1）") && p0.contains("<Picture 3> 只用來參考 乙（S2）"), "{p0}");
        assert!(p1.contains("<Picture 1> 只用來參考 甲（S1）") && p1.contains("<Picture 2>") && !p1.contains("<Picture 3>"), "{p1}");
        let s = normalize(&json!({"cast": cast3(), "refs": refs})).unwrap();
        assert!(compose(&s, 0, "en").contains("<Picture 1> is used only for the look of 甲 (S1)"));
        // 沒勾就不送、也不寫
        let s = normalize(&json!({"cast": cast3(), "refs": {"on": false, "items": [{"file": "r1.png"}]}})).unwrap();
        assert!(!compose(&s, 0, "zh").contains("<Picture"));
    }

    #[test]
    fn segment_can_turn_refs_off_and_numbers_close_up() {
        let refs = json!({"on": true, "items": [{"file": "r1.png", "who": "S1"}, {"file": "r2.png", "who": "S2"}]});
        let s = normalize(&json!({"cast": cast3(), "first": {"file": "f.png"}, "refs": refs,
            "segments": [{"ref_off": ["r1.png", "..\\x.png", 3]}, {"ref_off": ["r2.png"]}, {"ref_off": ["r1.png", "r2.png"]}]})).unwrap();
        assert_eq!(s["segments"][0]["ref_off"], json!(["r1.png"]), "壞檔名、不是字串的丟掉");
        let p0 = compose(&s, 0, "zh");
        assert!(p0.contains("<Picture 2> 只用來參考 乙（S2）") && !p0.contains("<Picture 3>") && !p0.contains("甲（S1）的長相"), "{p0}");
        let p1 = compose(&s, 1, "zh");
        assert!(p1.contains("<Picture 1> 只用來參考 甲（S1）") && !p1.contains("<Picture 2>"), "{p1}");
        assert!(!compose(&s, 2, "zh").contains("只用來參考"), "全關＝這段不送也不寫");
        assert_eq!(seg_ref_items(&s, 1).len(), 1);
        // 健檢照這段實際送的張數算，不會叫使用者補關掉的圖號
        let prompts: Vec<String> = (0..3).map(|i| compose(&s, i, "zh")).collect();
        let issues = check(&s, &prompts, &rules(&[]));
        assert!(!issues.iter().any(|x| x["rule"] == 7), "{issues:?}");
        // 存範本不帶這次才有的檔名
        assert!(template_of(&s)["segments"][0].get("ref_off").is_none());
    }

    #[test]
    fn turning_off_all_of_a_characters_refs_drops_them_from_the_segment() {
        let refs = json!({"on": true, "items": [{"file": "r1.png", "who": "S1"}, {"file": "r2.png", "who": "S2"}]});
        let cast = json!([{"name": "甲", "look_zh": "黑髮"}, {"name": "乙", "look_zh": "橘髮", "locked": true}, {"name": "丙", "look_zh": "金髮"}]);
        let s = normalize(&json!({"cast": cast, "refs": refs, "segments": [{"ref_off": ["r2.png"]}, {}]})).unwrap();
        assert_eq!(absent(&s, 0), ["S2"]);
        assert!(absent(&s, 1).is_empty());
        let p0 = compose(&s, 0, "zh");
        let p1 = compose(&s, 1, "zh");
        assert!(!p0.contains("乙") && p0.contains("甲"), "沒出場的鎖定角色、外觀、圖號都不寫：{p0}");
        assert!(p1.contains("S2（乙）") && p1.contains("<Picture 2>"), "{p1}");
        assert!(locked_head_at(&s, 0, true).is_empty() && !locked_head_at(&s, 1, true).is_empty());
        // 沒綁圖的角色（丙）不受影響；沒勾送參考圖時誰都不算缺席
        assert!(p0.contains("丙"), "{p0}");
        let off = normalize(&json!({"cast": cast, "refs": {"on": false, "items": [{"file": "r2.png", "who": "S2"}]}, "segments": [{"ref_off": ["r2.png"]}]})).unwrap();
        assert!(absent(&off, 0).is_empty());
    }

    #[test]
    fn locked_cast_goes_first_verbatim_and_keeps_order() {
        let sheet = json!({
            "cast": [
                {"name": "甲", "locked": true, "look_zh": "女性，二十出頭，橘色長髮；專屬標誌：右眼下方一顆淚痣。", "look_en": "female, early 20s, long orange hair; distinctive marks: a mole under the right eye."},
                {"name": "乙", "look_zh": "戴眼鏡的男性", "look_en": "a man with glasses"},
                {"name": "丙", "locked": true, "look_zh": "男性，黑短髮。", "look_en": "male, short black hair."}],
            "first": {"file": "f.png"}, "style": "日系動畫",
            "segments": [{"secs": 10, "beats": []}, {"secs": 10, "beats": []}]
        });
        let s = normalize(&sheet).unwrap();
        assert_eq!(s["cast"][0]["locked"], true);
        assert_eq!(s["cast"][1]["locked"], false);
        let r = normalize(&json!({"refmod": true, "cast": [{"name": "甲", "refmod": "paintdeck/pd_char_7"}, {"name": "乙", "refmod": "../x"}]})).unwrap();
        assert_eq!((r["refmod"].as_bool(), r["cast"][0]["refmod"].as_str(), r["cast"][1]["refmod"].as_str()), (Some(true), Some("paintdeck/pd_char_7"), Some("")));
        let head = "S1（甲）：女性，二十出頭，橘色長髮；專屬標誌：右眼下方一顆淚痣。S3（丙）：男性，黑短髮。";
        assert_eq!(locked_head(&s, true), head);
        let zh = compose(&s, 0, "zh");
        assert!(zh.starts_with(&format!("目標影片的第 0.00 秒完全參考 <Picture 1>（[Shot 1]）。\n\nintegrated_multimodal_description: {head}畫風：日系動畫。")), "{zh}");
        assert_eq!(zh.matches("S1（甲）").count(), 1, "鎖定的人不再出現第二次：{zh}");
        assert!(zh.contains("S2（乙）：戴眼鏡的男性。"), "{zh}");
        let second = compose(&s, 1, "zh");
        assert!(second.starts_with(&format!("integrated_multimodal_description: {head}目標影片從上一段")), "{second}");
        let en = compose(&s, 0, "en");
        let head_en = "甲 (S1): female, early 20s, long orange hair; distinctive marks: a mole under the right eye. 丙 (S3): male, short black hair.";
        assert_eq!(locked_head(&s, false), head_en);
        assert!(en.contains(&format!("integrated_multimodal_description: {head_en} Style: 日系動畫.")), "{en}");
        // 沒有鎖定的人：開頭照舊
        let plain = normalize(&json!({"cast": [{"name": "乙", "look_zh": "戴眼鏡的男性"}]})).unwrap();
        assert_eq!(locked_head(&plain, true), "");
    }

    #[test]
    fn put_locked_reinserts_once_after_ai_rewrite() {
        let head = "S1（甲）：女性，橘色長髮。";
        // AI 潤完的稿子：沒有鎖定段落
        let ai = "目標影片的第 0.00 秒完全參考 <Picture 1>（[Shot 1]）。\n\nintegrated_multimodal_description: 畫風：日系動畫。她轉身。\n\noverall_soundscape: 雨聲。\n\nnon_diegetic_music: N/A";
        let fixed = put_locked(ai, head, true);
        assert!(fixed.contains(&format!("integrated_multimodal_description: {head}畫風：日系動畫。她轉身。")), "{fixed}");
        assert_eq!(put_locked(&fixed, head, true), fixed, "接過一次不重複");
        assert_eq!(strip_locked(&fixed, head), ai);
        // 英文用空白接
        let en = put_locked("integrated_multimodal_description: Style: anime.", "Ko (S1): female.", false);
        assert_eq!(en, "integrated_multimodal_description: Ko (S1): female. Style: anime.");
        // 沒有 H3 欄位名（自訂工作流等引擎）：放在最前面一行
        assert_eq!(put_locked("Style: anime.\nShe turns.", "S1 Ko: female.", false), "S1 Ko: female.\nStyle: anime.\nShe turns.");
        assert_eq!(put_locked("", "S1 Ko: female.", false), "S1 Ko: female.");
        assert_eq!(put_locked("anything", "", true), "anything");
    }

    #[test]
    fn compose_structure_zh_en() {
        let sheet = json!({
            "cast": [{"name": "小克", "look_zh": "橘色長髮的成年女性", "look_en": "an adult woman with long orange hair", "voice": "soft", "lang": "Chinese"}],
            "first": {"file": "f.png"}, "style": "日系動畫", "camera": "正面中景",
            "scene": {"text": "咖啡廳", "layout": {"left": "窗戶"}},
            "segments": [
                {"secs": 10, "beats": [{"from": 1.0, "to": 4.0, "action": "她右手拿起杯子", "who": "S1", "lang": "ja", "line": "いただきます", "sfx": "杯子輕碰桌面"}]},
                {"secs": 10, "beats": []}, {"secs": 10, "beats": []}]
        });
        let s = normalize(&sheet).unwrap();
        let zh = compose(&s, 0, "zh");
        assert!(zh.starts_with("目標影片的第 0.00 秒完全參考 <Picture 1>（[Shot 1]）。\n\nintegrated_multimodal_description: "), "{zh}");
        for want in ["畫風：日系動畫。", "鏡位與運鏡：正面中景。", "S1（小克）：橘色長髮的成年女性。", "場景：咖啡廳。", "位置：左邊 窗戶。",
                     "從 00:00.000 到 00:03.000，她右手拿起杯子。", "小克（S1）用「soft」的聲音說：<d>[Japanese] いただきます</d>", "音效：杯子輕碰桌面。",
                     "從 00:09.300 到結尾，動作繼續。", "\n\noverall_soundscape: 符合場景的自然環境音。", "\n\nnon_diegetic_music: N/A"] {
            assert!(zh.contains(want), "缺「{want}」：{zh}");
        }
        let en = compose(&s, 0, "en");
        for want in ["For the target video, at 0.00 seconds into the target video, <Picture 1> (from [Shot 1]) is fully referenced.\n\n",
                     "Style: 日系動畫.", "小克 (S1): an adult woman with long orange hair.", "Position: left: 窗戶.",
                     "From 00:00.000 to 00:03.000, 她右手拿起杯子.", "小克 (S1) says in soft: <d>[Japanese] いただきます</d>",
                     "overall_soundscape: Natural ambient sound that matches the scene."] {
            assert!(en.contains(want), "缺「{want}」：{en}");
        }
        let last = compose(&s, 2, "zh");
        assert!(last.starts_with("integrated_multimodal_description: 目標影片從上一段的最後一刻直接接著拍，不切鏡、不重來。"), "{last}");
        assert!(last.contains("從 00:00.000 到 00:01.600，延續上一段的同一個動作。"));
        assert!(!last.contains("到結尾，動作繼續"), "最後一段不切尾");
        // 台詞沒寫語言、人也沒寫：用 Chinese；找不到人寫「這個角色」
        let s = normalize(&json!({"segments": [{"beats": [{"from": 0, "to": 2, "action": "x", "who": "S3", "line": "嗨"}]}]})).unwrap();
        assert!(compose(&s, 0, "zh").contains("這個角色說：<d>[Chinese] 嗨</d>"), "{}", compose(&s, 0, "zh"));
    }

    fn rules_of(issues: &[Value]) -> Vec<i64> {
        issues.iter().map(|x| x["rule"].as_i64().unwrap()).collect()
    }

    #[test]
    fn check_catches_each_rule() {
        let s = normalize(&json!({"refs": {"on": true, "items": [{"file": "r.png", "who": "S1"}]}, "segments": [
            {"secs": 10, "beats": [
                {"from": 0, "to": 3, "action": "她轉身"},
                {"from": 2.5, "to": 3, "action": "她右手放下"},
                {"from": 3, "to": 9.8, "action": "她右手舉起"}]},
            {"secs": 10, "beats": []}]}))
        .unwrap();
        let prompts = vec!["A sultry look, not tongue.".to_string(), "<Picture 1> ok".to_string()];
        let got = check(&s, &prompts, &rules(&[]));
        assert_eq!(rules_of(&got), [1, 2, 3, 4, 5, 6, 7, 8], "{got:#?}");
        assert!(got.iter().all(|x| x["seg"] == 0));
        assert_eq!(got[7]["level"], "error");
        assert_eq!(got[0]["level"], "warn");
        assert_eq!(got[0]["word"], "sultry");
    }

    #[test]
    fn check_ignores_dialogue_and_clean_sheet() {
        let r = rules(&[]);
        let s = normalize(&json!({"segments": [{"beats": [{"from": 1, "to": 3, "action": "她右手揮一揮", "line": "不要走"}]}]})).unwrap();
        let p = vec![compose(&s, 0, "zh")];
        assert!(p[0].contains("<d>[Chinese] 不要走</d>"));
        assert!(check(&s, &p, &r).is_empty(), "{:?}", check(&s, &p, &r));
        // 「特別」不算防止詞
        let s2 = normalize(&json!({"segments": [{"beats": [{"from": 1, "to": 3, "action": "她特別用右手揮一揮"}]}]})).unwrap();
        assert!(check(&s2, &[compose(&s2, 0, "zh")], &r).is_empty());
        let clean = normalize(&json!({
            "cast": [{"name": "小克", "look_zh": "橘髮女性", "look_en": "a woman with orange hair", "lang": "ja"}],
            "first": {"file": "f.png"}, "refs": {"on": true, "items": [{"file": "r.png", "who": "S1"}]},
            "scene": {"text": "教室", "layout": {"left": "窗戶"}}, "camera": "側面視角",
            "segments": [{"beats": [{"from": 1, "to": 5, "action": "她右手拿起杯子", "who": "S1", "line": "おはよう"}]},
                         {"beats": [{"from": 2, "to": 4, "action": "她左手把杯子放到桌上"}]}]}))
        .unwrap();
        for lang in ["zh", "en"] {
            let p: Vec<String> = (0..2).map(|i| compose(&clean, i, lang)).collect();
            assert!(check(&clean, &p, &r).is_empty(), "{lang}: {:?}\n{p:#?}", check(&clean, &p, &r));
        }
    }

    #[test]
    fn rules_merge_user_danger() {
        let r = rules(&["Blood".into(), " ".into()]);
        let s = normalize(&json!({})).unwrap();
        let got = check(&s, &["blood and a tongue".to_string()], &r);
        let words: Vec<&str> = got.iter().filter(|x| x["rule"] == 3).map(|x| x["word"].as_str().unwrap()).collect();
        assert_eq!(words, ["tongue", "blood"], "{got:?}");
        // 整字比對：bloody 裡的 blood 不算
        assert!(check(&s, &["bloodyx".to_string()], &r).is_empty());
    }

    #[test]
    fn estimate_from_table() {
        let e = estimate(&normalize(&json!({})).unwrap());
        assert_eq!(e[0]["gpu"], "L40S");
        assert_eq!(e[0]["secs"], 400.0);
        assert_eq!(e[0]["usd"], 0.12);
        assert_eq!(e[1]["gpu"], "A40");
        assert_eq!(e[1]["secs"], 650.0);
    }

    #[test]
    fn template_drops_files_and_prompts() {
        let s = normalize(&json!({
            "cast": [{"asset_id": 7, "name": "甲"}],
            "first": {"file": "f.png", "asset_id": 3, "gallery_id": 9},
            "refs": {"on": true, "items": [{"file": "r.png", "asset_id": 5, "who": "S1"}]},
            "segments": [{"beats": [{"from": 1, "to": 2, "action": "她右手揮一揮"}], "prompt": "p", "mirror": "m", "manual": true}]}))
        .unwrap();
        let t = template_of(&s);
        assert!(t["first"].get("file").is_none() && t["first"].get("gallery_id").is_none());
        assert_eq!(t["first"]["asset_id"], 3);
        assert!(t["refs"]["items"][0].get("file").is_none());
        assert_eq!(t["refs"]["items"][0]["asset_id"], 5);
        let seg = &t["segments"][0];
        assert!(seg.get("prompt").is_none() && seg.get("mirror").is_none() && seg.get("manual").is_none());
        assert_eq!(seg["beats"][0]["action"], "她右手揮一揮");
        assert_eq!(t["cast"][0]["asset_id"], 7);
        // 範本讀回來要能再正規化（檔名沒了就是空的）
        let back = normalize(&t).unwrap();
        assert!(back["first"]["file"].is_null());
        assert!(back["refs"]["items"][0]["file"].is_null() && back["refs"]["items"][0]["asset_id"] == 5, "{back}");
        // 沒檔名的參考圖不寫進提示詞
        assert!(!compose(&back, 0, "zh").contains("<Picture"));
    }

    #[test]
    fn rules_file_parses() {
        let j = rules_json();
        assert_eq!(j["cheat"]["view"].as_array().unwrap().len(), 5);
        assert_eq!(j["cheat"]["move"].as_array().unwrap().len(), 7);
        assert!(j["cheat"]["move"].as_array().unwrap().iter().all(|x| x.as_array().unwrap().len() == 3));
        assert!(estimate_json()["gpus"].as_array().unwrap().len() >= 2);
        let r = rules(&[]);
        assert!(!r.body.is_empty() && !r.position.is_empty() && !r.direction.is_empty() && r.prevent_re.len() == 1);
    }
}
