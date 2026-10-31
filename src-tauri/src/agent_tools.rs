//! 生圖助手的工具（2026-10-27「助手全功能」）：助手除了畫圖，還能用 {"action":"call","tool":…,"args":…} 操作全站。
//! 使用者原話：「真的能讓助手使用我們 PAINTDECK 所有功能」「先把你剛剛列出的 做不到 想辦法搞成 都能做到」。
//!
//! 這裡放不碰網路的部分：工具清單（給模型看的說明）、分級（直接做／先問使用者／禁止）、參數檢查與正規化、
//! 「直接打 API」的路徑分級（anima-tools.js 的 DANGER 的 Rust 版，再加上禁止的）。執行在 server\api_agent_tools.rs。
use crate::director::tx;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::LazyLock;

/// 一個請求最多叫幾次工具（9B 會繞圈：查圖庫 → 看圖 → 再查…）
pub const MAX_CALLS: i64 = 12;
/// 工具結果給模型看的上限（字）
pub const RESULT_CHARS: usize = 3000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// 直接做
    Normal,
    /// 花錢、刪東西、改設定：助手視窗跳確認卡，使用者按「准」才做
    Confirm,
    /// 助手做不到（永久刪除、恢復原廠、叫助手自己）
    Forbidden,
}

pub struct Tool {
    pub name: &'static str,
    /// 參數範例（給模型看，JSON）
    pub args: &'static str,
    /// 做什麼（英文一行，給模型看）
    pub desc: &'static str,
    /// 中文名（畫面上的工具標籤）
    pub zh: &'static str,
}

/// 主視窗的頁面（open_page、「讓我看」切頁用；跟 ui/js/main.js 認得的一樣）
pub const PAGES: &[&str] = &["gen", "gallery", "assets", "loras", "civitai", "director", "comic", "settings"];

pub const TOOLS: &[Tool] = &[
    Tool { name: "models", args: "{}", desc: "List the image models on this computer by family, and which one you use now.", zh: "看模型清單" },
    Tool { name: "set_model", args: r#"{"model":"exact file name from models"}"#, desc: "Switch the image model your generate action uses (its family's default steps and CFG come with it). Use it when the user asks for another model, or when a job needs another family (Qwen 2.1 for edits and text, MOSS for layouts and lettering, Anima for anime tags).", zh: "換生圖模型" },
    Tool { name: "gallery", args: r#"{"q":"words in the prompt","mode":"txt2img","fav":false,"limit":10}"#, desc: "Search the gallery, newest first: ids, mode, size and the start of each prompt. With no q you get the latest images.", zh: "查圖庫" },
    Tool { name: "image_info", args: r#"{"id":123}"#, desc: "Full details of one gallery image: prompt, negative, model, LoRAs, seed, size.", zh: "看圖的參數" },
    Tool { name: "look", args: r#"{"ids":[123,124]}"#, desc: "See up to 4 gallery images yourself in the next turn (to judge them, compare them, or pick the best).", zh: "看圖" },
    Tool { name: "edit", args: r#"{"id":123,"instruction":"把背景換成夜晚的海邊，其他保持不變"}"#, desc: "Edit one picture by instruction with Qwen 2.1 (change clothes, background, expression, the pose of the arms or hands, add or remove things; keeps the person and the style). Use \"ref\":N instead of id for user image N: to change the user's own picture, always edit it with ref, not a picture you drew. The instruction may be Chinese.", zh: "改圖" },
    Tool { name: "upscale", args: r#"{"id":123,"scale":2}"#, desc: "Upscale an image (scale 1.5 to 4).", zh: "放大" },
    Tool { name: "remove_bg", args: r#"{"id":123}"#, desc: "Cut out the subject onto a transparent background.", zh: "去背" },
    Tool { name: "fix", args: r#"{"id":123,"what":"face"}"#, desc: "Repaint the faces (\"face\"), hands (\"hand\") or both (\"both\") of an image to fix them.", zh: "修臉修手" },
    Tool { name: "outpaint", args: r#"{"id":123,"left":256,"right":256,"top":0,"bottom":0}"#, desc: "Extend the canvas (pixels per side, up to 1024) and paint the new area.", zh: "擴圖" },
    Tool { name: "video", args: r#"{"id":123,"prompt":"English description of the motion and sound","seconds":5}"#, desc: "Make a video of 1 to 10 seconds with the video model chosen in settings. With id (or ref) it animates that image; without, it is text to video.", zh: "做影片" },
    Tool { name: "character_sheet", args: r#"{"id":123,"kind":"turnaround"}"#, desc: "Draw the same character from new angles with Qwen 2.1: \"turnaround\" (front, side, back), \"face\" (face close-ups) or \"portrait\" (front close-up). Use \"ref\":N for user image N. Always use this, not refs with denoise, for new angles or a turnaround of an existing character.", zh: "角色多視圖" },
    Tool { name: "comic", args: r#"{"story":"這一頁的劇情（中文）","title":"標題"}"#, desc: "Make a whole comic page: creates a comic project, splits the story into panels with dialogue, and draws the page with MOSS.", zh: "畫漫畫" },
    Tool { name: "assets", args: r#"{"kind":"char"}"#, desc: "List saved assets (kind: char, scene, outfit, prop, frame, fx, sound) with ids, names and looks.", zh: "看資產庫" },
    Tool { name: "trash", args: r#"{"ids":[123]}"#, desc: "Move gallery images to the recycle bin. The user has to approve first.", zh: "移到回收區" },
    Tool { name: "guide", args: r#"{"name":"qwen_edit"}"#, desc: "Read the full prompt-writing guide for a model and task: anima, qwen, qwen_edit, qwen_multiref, moss, other, h3_video. Read the matching guide before you write an edit instruction, a video prompt, a prompt with refs, or a prompt for a model you just switched to. When a guide lists the parts of an extra skill, read one part with \"part\":N.", zh: "讀寫法" },
    Tool { name: "docs", args: r#"{"section":"漫畫"}"#, desc: "Read the app manual (Chinese). Without section you get the table of contents. Read the matching section before you use api.", zh: "讀說明書" },
    Tool { name: "api", args: r#"{"method":"GET","path":"/api/comic/projects","query":{},"body":{}}"#, desc: "Call any HTTP API of the app (director, H3 video, long films, prompt library, LoRAs, Civitai, tagger, settings…); paths and fields are in docs. Spending money, deleting or changing settings waits for the user's approval; permanent deletion and factory reset are not possible.", zh: "直接呼叫 API" },
    Tool { name: "open_page", args: r#"{"page":"gallery"}"#, desc: "Show a page in the main window: gen, gallery, assets, loras, civitai, director, comic, settings.", zh: "切換頁面" },
];

pub fn find(name: &str) -> Option<&'static Tool> {
    let n = name.trim().to_lowercase().replace(['-', ' '], "_");
    TOOLS.iter().find(|t| t.name == n)
}

/// 工具說明（放進系統提示詞的 {tools}）
pub fn tools_text() -> String {
    let mut out = vec![
        "Besides drawing with generate, you can operate the whole app with:".to_string(),
        r#"{"action":"call","tool":"name","args":{...},"say":"one short work note in Traditional Chinese"}"#.to_string(),
        "The result comes back in the next turn (pictures it makes are shown to you like your own rounds, and you may pick them in finish). One tool per turn. Tools:".to_string(),
    ];
    for t in TOOLS {
        out.push(format!("- {} {}: {}", t.name, t.args, t.desc));
    }
    out.push(format!(
        "\"id\" is a gallery image id (from your rounds, gallery or look); \"ref\":N is user image N. Use the tool that fits the request instead of redrawing from scratch (\"放大那張\" → upscale, \"把那張刪掉\" → trash). \
If a tool fails, read why: fix the arguments and try once more only when that can help; never swap in an unrelated tool or a new drawing to fake the result. Otherwise reply and tell the user what went wrong. When you finish after tools, \"message\" says what you actually did for THIS request (for example that the picture went to the recycle bin), and \"best\" lists only images made for this request, or [] when none. At most {MAX_CALLS} tool calls per request."
    ));
    out.join("\n")
}

/// 工作說明加上工具段落：有 {tools} 就填在那裡，沒有（使用者改過的舊說明）就接在最後
pub fn with_tools(system: &str) -> String {
    let t = tools_text();
    if system.contains("{tools}") {
        system.replace("{tools}", &t)
    } else {
        format!("{system}\n\nTOOLS\n{t}")
    }
}

// ================================================================ 直接打 API 的分級
// anima-tools.js 的 DANGER（會花錢、刪資料、改設定的要確認）；改那邊時這裡一起改
static DANGER: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"^/api/diagnostics/(clear|preferences)$", "清除診斷紀錄或改保留設定"),
        (r"^/api/studio/frames/clear$", "清除拆幀圖片、聲音和修幀紀錄"),
        (r"^/api/cache/clear$", "清除快取與安裝暫存"),
        (r"^/api/app/update/(install|preferences)$", "安裝新版或改自動更新設定"),
        (r"^/api/agent/cloud/start$", "雲端開機（開始計費）"),
        (r"^/api/agent/cloud/stop$", "雲端關機（＝刪機，下次要重下模型）"),
        (r"^/api/h3/chain/run$", "H3 長片開拍（雲端計費）"),
        (r"^/api/h3/generate$", "H3 影片（選雲端或 RunningHub 會花錢）"),
        (r"^/api/agent/cloud/(config|idle)$", "改雲端設定"),
        (r"^/api/runninghub/config$", "改 RunningHub 設定"),
        (r"^/api/gallery/prune_missing$", "把檔案不見的紀錄從圖庫拿掉"),
        (r"/delete$", "刪除"),
        (r"^/api/jobs/clear$", "清除任務紀錄"),
        (r"^/api/skills/(install|bind|remove)$", "安裝、綁定或移除外掛寫法"),
        (r"^/api/clear_queue$", "清空等待中的任務"),
        (r"^/api/settings(/import)?$", "改生圖站設定"),
        (r"^/api/feature_flows$", "改內建功能要用的流程"),
        (r"^/api/pe/prompts$", "改 AI 改寫的系統提示詞"),
        (r"^/api/models/(dirs/(add|remove)|scan/apply|download|fetch_listed|import|place|classify|restart_comfy)$", "改模型資料夾、下載或搬模型檔、重開 ComfyUI"),
        (r"^/api/civitai/download$", "從 Civitai 下載檔案"),
        // 2026-10-29 下載中心：MCP 那邊（anima-tools.js 的 DANGER）同一天一起加
        (r"^/api/hub/download$", "從 C 站或魔塔下載模型檔"),
        (r"^/api/hub/modelscope$", "改魔塔金鑰"),
        (r"^/api/hub/rh_run$", "在 RunningHub 跑工作流（照秒扣 RH 幣）"),
        (r"^/api/(image|video)/workflows","新增或改動匯入的工作流"),
        (r"^/api/voices/tts_workflow$", "換掉或拿掉聲音庫的 TTS 工作流"),
    ]
    .into_iter()
    .map(|(r, why)| (Regex::new(r).expect("DANGER"), why))
    .collect()
});

/// 直接打 API 的分級：(等級, 中文原因)。path 已經去掉 query
pub fn api_level(method: &str, path: &str, body: &Value) -> (Level, &'static str) {
    let m = method.to_ascii_uppercase();
    // 永久刪除、恢復原廠：使用者自己按（anima-guide 規則「永久刪除留給使用者自己按」）
    if path.starts_with("/api/reset") {
        return (Level::Forbidden, "恢復原廠只能使用者自己在設定頁按");
    }
    if path == "/api/trash/empty" || (path == "/api/images/batch" && tx(&body["action"]) == "purge") {
        return (Level::Forbidden, "永久刪除只能使用者自己按");
    }
    if m == "GET" {
        return (Level::Normal, "");
    }
    // 叫助手自己（開新對話、改自己的設定）會打結
    if path.starts_with("/api/agent/") && !path.starts_with("/api/agent/cloud") {
        return (Level::Forbidden, "助手不能操作助手自己");
    }
    if path == "/api/images/batch" && tx(&body["action"]) == "trash" {
        return (Level::Confirm, "把圖移到回收區");
    }
    if m == "DELETE" {
        return (Level::Confirm, "刪除");
    }
    if let Some((_, why)) = DANGER.iter().find(|(re, _)| re.is_match(path)) {
        return (Level::Confirm, why);
    }
    (Level::Normal, "")
}

// ================================================================ 參數檢查
fn int_of(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().trim_start_matches('#').parse().ok(),
        _ => None,
    }
}

fn ids_of(v: Option<&Value>) -> Vec<i64> {
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(|x| int_of(Some(x))).collect(),
        Some(x) => int_of(Some(x)).into_iter().collect(),
        None => vec![],
    }
}

/// id（圖庫）或 ref（使用者的圖）：兩個都沒有就退回
fn image_of(a: &Map<String, Value>, out: &mut Map<String, Value>, tool: &str, need: bool) -> Result<(), String> {
    if let Some(r) = int_of(a.get("ref")) {
        out.insert("ref".into(), json!(r));
    } else if let Some(i) = int_of(a.get("id").or(a.get("image")).or(a.get("image_id"))) {
        out.insert("id".into(), json!(i));
    } else if need {
        return Err(format!("{tool} needs \"id\" (a gallery image id) or \"ref\" (a user image number)."));
    }
    Ok(())
}

fn text_arg(a: &Map<String, Value>, keys: &[&str]) -> String {
    keys.iter().find_map(|k| a.get(*k)).map(tx).unwrap_or_default().trim().to_string()
}

/// 檢查並正規化一次工具呼叫 → (args, 等級, 中文原因)；不合格回傳要退回給模型的英文說明
pub fn check_call(tool: &str, args: &Value) -> Result<(Value, Level, String), String> {
    let Some(t) = find(tool) else {
        return Err(format!(
            "Unknown tool \"{tool}\". Tools: {}.",
            TOOLS.iter().map(|t| t.name).collect::<Vec<_>>().join(", ")
        ));
    };
    let empty = Map::new();
    let a = args.as_object().unwrap_or(&empty);
    let mut out = Map::new();
    let mut level = (Level::Normal, String::new());
    match t.name {
        "guide" => {
            let n = text_arg(a, &["name", "guide", "model"]).to_lowercase().replace(['-', ' '], "_");
            let Some(g) = crate::agent::GUIDES.iter().find(|g| g.0 == n) else {
                return Err(format!("guide needs \"name\": one of {}.", crate::agent::GUIDES.iter().map(|g| g.0).collect::<Vec<_>>().join(", ")));
            };
            out.insert("name".into(), json!(g.0));
            // 外掛寫法的第幾段（從 1 起）
            if let Some(p) = int_of(a.get("part").or(a.get("section"))) {
                if p < 1 {
                    return Err("guide \"part\" starts at 1.".into());
                }
                out.insert("part".into(), json!(p));
            }
        }
        "models" | "assets" => {
            if t.name == "assets" {
                let k = text_arg(a, &["kind"]);
                if !k.is_empty() {
                    out.insert("kind".into(), json!(k));
                }
            }
        }
        "set_model" => {
            let m = text_arg(a, &["model", "name"]);
            if m.is_empty() {
                return Err("set_model needs \"model\": an exact file name from the models tool.".into());
            }
            out.insert("model".into(), json!(m));
        }
        "gallery" => {
            for k in ["q", "mode", "model"] {
                let v = text_arg(a, &[k]);
                if !v.is_empty() {
                    out.insert(k.into(), json!(v));
                }
            }
            if a.get("fav").is_some_and(|v| crate::py::truthy(Some(v))) {
                out.insert("fav".into(), json!(true));
            }
            out.insert("limit".into(), json!(int_of(a.get("limit")).unwrap_or(10).clamp(1, 20)));
        }
        "image_info" => {
            let id = int_of(a.get("id")).ok_or("image_info needs \"id\".")?;
            out.insert("id".into(), json!(id));
        }
        "look" => {
            let mut ids = ids_of(a.get("ids").or(a.get("id")));
            ids.dedup();
            ids.truncate(4);
            if ids.is_empty() {
                return Err("look needs \"ids\": 1 to 4 gallery image ids.".into());
            }
            out.insert("ids".into(), json!(ids));
        }
        "edit" => {
            image_of(a, &mut out, "edit", true)?;
            let ins = text_arg(a, &["instruction", "prompt", "text"]);
            if ins.chars().count() < 2 {
                return Err("edit needs \"instruction\": what to change in the picture.".into());
            }
            out.insert("instruction".into(), json!(ins));
        }
        "upscale" => {
            image_of(a, &mut out, "upscale", true)?;
            let s = a.get("scale").and_then(crate::py::float).unwrap_or(2.0).clamp(1.5, 4.0);
            out.insert("scale".into(), json!(s));
        }
        "remove_bg" => image_of(a, &mut out, "remove_bg", true)?,
        "fix" => {
            image_of(a, &mut out, "fix", true)?;
            let w = text_arg(a, &["what", "part"]).to_lowercase();
            let w = match w.as_str() {
                "" | "face" | "faces" => "face",
                "hand" | "hands" => "hand",
                "both" | "face and hand" | "face,hand" => "both",
                _ => return Err("fix needs \"what\": \"face\", \"hand\" or \"both\".".into()),
            };
            out.insert("what".into(), json!(w));
        }
        "outpaint" => {
            image_of(a, &mut out, "outpaint", true)?;
            let mut any = false;
            for k in ["top", "bottom", "left", "right"] {
                let v = int_of(a.get(k)).unwrap_or(0).clamp(0, 1024);
                any |= v > 0;
                out.insert(k.into(), json!(v));
            }
            if !any {
                return Err("outpaint needs at least one of top, bottom, left, right in pixels (for example \"left\":256).".into());
            }
        }
        "video" => {
            image_of(a, &mut out, "video", false)?;
            let p = text_arg(a, &["prompt", "text"]);
            if p.chars().count() < 10 {
                return Err("video needs \"prompt\": an English description of what moves and what is heard.".into());
            }
            out.insert("prompt".into(), json!(p));
            out.insert("seconds".into(), json!(int_of(a.get("seconds")).unwrap_or(5).clamp(1, 10)));
        }
        "character_sheet" => {
            image_of(a, &mut out, "character_sheet", true)?;
            let k = text_arg(a, &["kind", "type"]).to_lowercase();
            let k = match k.as_str() {
                "" | "turnaround" | "views" | "sheet" | "三視圖" => "turnaround",
                "face" | "faces" => "face",
                "portrait" | "front" => "portrait",
                _ => return Err("character_sheet needs \"kind\": \"turnaround\", \"face\" or \"portrait\".".into()),
            };
            out.insert("kind".into(), json!(k));
        }
        "comic" => {
            let s = text_arg(a, &["story", "script", "text"]);
            if s.chars().count() < 4 {
                return Err("comic needs \"story\": what happens on this page.".into());
            }
            out.insert("story".into(), json!(s));
            let title = text_arg(a, &["title"]);
            out.insert("title".into(), json!(if title.is_empty() { s.chars().take(20).collect::<String>() } else { title }));
        }
        "trash" => {
            let ids = ids_of(a.get("ids").or(a.get("id")));
            if ids.is_empty() {
                return Err("trash needs \"ids\": gallery image ids.".into());
            }
            out.insert("ids".into(), json!(ids));
            level = (Level::Confirm, format!("把 {} 張圖移到回收區", ids.len()));
        }
        "docs" => {
            out.insert("section".into(), json!(text_arg(a, &["section", "q"])));
        }
        "api" => {
            let method = text_arg(a, &["method"]).to_ascii_uppercase();
            let method = if method.is_empty() { "GET".to_string() } else { method };
            if !["GET", "POST", "PUT", "DELETE"].contains(&method.as_str()) {
                return Err("api \"method\" must be GET, POST, PUT or DELETE.".into());
            }
            let raw = text_arg(a, &["path", "url"]);
            let path = raw.split('?').next().unwrap_or("").to_string();
            if !path.starts_with("/api/") || path.contains("..") {
                return Err("api \"path\" must start with /api/ (see docs).".into());
            }
            let body = a.get("body").cloned().unwrap_or(json!({}));
            let mut query = a.get("query").cloned().filter(Value::is_object).unwrap_or(json!({}));
            // 9B 會把 query 寫在 path 後面
            if let Some((_, qs)) = raw.split_once('?') {
                for kv in qs.split('&').filter(|s| !s.is_empty()) {
                    let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                    query[k] = json!(v);
                }
            }
            let (lv, why) = api_level(&method, &path, &body);
            if lv == Level::Forbidden {
                return Err(format!("Not allowed: {why}. Tell the user they can do it themselves."));
            }
            level = (lv, format!("{why}（{method} {path}）"));
            out.insert("method".into(), json!(method));
            out.insert("path".into(), json!(path));
            out.insert("query".into(), query);
            out.insert("body".into(), body);
        }
        "open_page" => {
            let p = text_arg(a, &["page"]).to_lowercase();
            if !PAGES.contains(&p.as_str()) {
                return Err(format!("open_page \"page\" must be one of {}.", PAGES.join(", ")));
            }
            out.insert("page".into(), json!(p));
        }
        _ => unreachable!("工具清單和這裡要對上"),
    }
    Ok((Value::Object(out), level.0, level.1))
}

/// 截斷給模型看的文字
pub fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let t: String = s.chars().take(n).collect();
    format!("{t}…（後面省略）")
}

/// 說明書（anima-guide.md）的一段：section 空白＝目錄（## 標題）；有就回標題含那幾個字的段落
pub fn docs_section(guide: &str, section: &str) -> String {
    let heads: Vec<(usize, &str)> = guide.match_indices("\n## ").map(|(i, _)| (i + 1, &guide[i + 4..])).collect();
    let title = |s: &str| s.lines().next().unwrap_or("").trim().to_string();
    if section.trim().is_empty() {
        return format!("Sections: {}", heads.iter().map(|(_, s)| title(s)).collect::<Vec<_>>().join(" / "));
    }
    let want = section.trim().to_lowercase();
    let mut out = vec![];
    for (k, (start, s)) in heads.iter().enumerate() {
        if title(s).to_lowercase().contains(&want) {
            let end = heads.get(k + 1).map(|h| h.0).unwrap_or(guide.len());
            out.push(guide[*start..end].trim());
        }
    }
    if out.is_empty() {
        return format!("No section matches \"{section}\". Sections: {}", heads.iter().map(|(_, s)| title(s)).collect::<Vec<_>>().join(" / "));
    }
    out.join("\n\n")
}

/// 「讓我看」模式：這個工具在主視窗哪一頁做（api 依路徑猜）
pub fn page_of(tool: &str, args: &Value) -> Option<&'static str> {
    Some(match tool {
        "edit" | "upscale" | "remove_bg" | "fix" | "outpaint" | "video" | "character_sheet" | "set_model" | "models" => "gen",
        "gallery" | "image_info" | "look" | "trash" => "gallery",
        "comic" => "comic",
        "assets" => "assets",
        "open_page" => return PAGES.iter().copied().find(|p| *p == tx(&args["page"])),
        "api" => {
            let p = tx(&args["path"]);
            let seg = p.trim_start_matches("/api/").split('/').next().unwrap_or("");
            match seg {
                "comic" | "colorbook" => "comic",
                "director" | "h3" | "studio" | "shotsheet" => "director",
                "lib" | "assets" | "charsheet" => "assets",
                "gallery" | "image" | "images" => "gallery",
                "loras" => "loras",
                "civitai" | "downloads" | "hub" => "civitai",
                "settings" | "models" => "settings",
                "generate" | "detail" => "gen",
                _ => return None,
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_paths_are_graded() {
        let lv = |m: &str, p: &str, b: Value| api_level(m, p, &b).0;
        assert_eq!(lv("GET", "/api/agent/cloud", json!({})), Level::Normal);
        assert_eq!(lv("POST", "/api/agent/cloud/start", json!({})), Level::Confirm);
        assert_eq!(lv("POST", "/api/reset", json!({})), Level::Forbidden);
        assert_eq!(lv("GET", "/api/reset/undo", json!({})), Level::Forbidden);
        assert_eq!(lv("POST", "/api/images/batch", json!({"action":"purge"})), Level::Forbidden);
        assert_eq!(lv("POST", "/api/images/batch", json!({"action":"trash"})), Level::Confirm);
        assert_eq!(lv("POST", "/api/images/batch", json!({"action":"fav"})), Level::Normal);
        assert_eq!(lv("POST", "/api/trash/empty", json!({})), Level::Forbidden);
        assert_eq!(lv("POST", "/api/settings", json!({})), Level::Confirm);
        assert_eq!(lv("POST", "/api/agent/say", json!({})), Level::Forbidden);
        assert_eq!(lv("POST", "/api/comic/project/delete", json!({})), Level::Confirm);
        assert_eq!(lv("DELETE", "/api/lib/3", json!({})), Level::Confirm);
        assert_eq!(lv("POST", "/api/comic/run", json!({})), Level::Normal);
        // 整本上色：本機顯卡上色不花錢（跟漫畫台一樣）；整本搬到待刪除要確認
        assert_eq!(lv("POST", "/api/colorbook/cb_1/run", json!({})), Level::Normal);
        assert_eq!(lv("POST", "/api/colorbook/cb_1/delete", json!({})), Level::Confirm);
        assert_eq!(lv("POST", "/api/skills/install", json!({})), Level::Confirm);
        assert_eq!(lv("GET", "/api/skills", json!({})), Level::Normal);
        // 下載中心：下載、改金鑰要確認；搜尋、看金鑰有沒有填不用
        assert_eq!(lv("POST", "/api/hub/download", json!({})), Level::Confirm);
        assert_eq!(lv("POST", "/api/hub/modelscope", json!({})), Level::Confirm);
        assert_eq!(lv("GET", "/api/hub/modelscope", json!({})), Level::Normal);
        assert_eq!(lv("GET", "/api/hub/search", json!({})), Level::Normal);
        assert_eq!(lv("POST", "/api/hub/rh_run", json!({"dry": true})), Level::Confirm);
    }

    #[test]
    fn check_call_rejects_and_normalizes() {
        let e = check_call("paint_house", &json!({})).unwrap_err();
        assert!(e.contains("Unknown tool") && e.contains("upscale"), "{e}");
        let (a, lv, _) = check_call("upscale", &json!({"id": "12"})).unwrap();
        assert_eq!((a["id"].clone(), a["scale"].clone(), lv), (json!(12), json!(2.0), Level::Normal));
        let (a, _, _) = check_call("look", &json!({"ids": [1, 2, 3, 4, 5, 6]})).unwrap();
        assert_eq!(a["ids"], json!([1, 2, 3, 4]));
        assert!(check_call("fix", &json!({"id": 3, "what": "臉"})).is_err());
        let (_, lv, why) = check_call("trash", &json!({"ids": [5, 6]})).unwrap();
        assert_eq!(lv, Level::Confirm);
        assert!(why.contains("2 張"));
        assert!(check_call("api", &json!({"method": "POST", "path": "/api/reset"})).unwrap_err().starts_with("Not allowed"));
        let (a, lv, _) = check_call("api", &json!({"path": "/api/gallery?q=cat&limit=3"})).unwrap();
        assert_eq!((a["method"].clone(), a["path"].clone(), a["query"]["q"].clone(), lv), (json!("GET"), json!("/api/gallery"), json!("cat"), Level::Normal));
        let (a, _, _) = check_call("character_sheet", &json!({"ref": 1})).unwrap();
        assert_eq!((a["ref"].clone(), a["kind"].clone()), (json!(1), json!("turnaround")));
        assert!(check_call("edit", &json!({"instruction": "換背景"})).unwrap_err().contains("\"id\""));
        assert!(check_call("open_page", &json!({"page": "../etc"})).is_err());
        // 9B 會寫成 Upscale、remove-bg
        assert!(check_call("Upscale", &json!({"id": 1})).is_ok() && check_call("remove-bg", &json!({"id": 1})).is_ok());
        // 外掛寫法按段讀
        let (a, _, _) = check_call("guide", &json!({"name": "MOSS", "part": "7"})).unwrap();
        assert_eq!((a["name"].clone(), a["part"].clone()), (json!("moss"), json!(7)));
        assert!(check_call("guide", &json!({"name": "moss", "part": 0})).is_err());
        assert!(check_call("guide", &json!({"name": "moss"})).unwrap().0.get("part").is_none());
    }

    #[test]
    fn docs_sections() {
        let g = "# 標題\n\n## 規則\n不要刪。\n\n## 漫畫台\n一整頁。\n";
        assert!(docs_section(g, "").contains("規則 / 漫畫台"));
        assert_eq!(docs_section(g, "漫畫"), "## 漫畫台\n一整頁。");
        assert!(docs_section(g, "影片").starts_with("No section"));
    }

    #[test]
    fn tools_text_lists_every_tool() {
        let t = with_tools("SYSTEM {tools} END");
        assert!(t.starts_with("SYSTEM Besides drawing") && t.ends_with("END"));
        assert!(TOOLS.iter().all(|x| t.contains(&format!("- {} ", x.name))));
        assert!(with_tools("OLD").contains("\n\nTOOLS\n"));
        assert_eq!(page_of("api", &json!({"path": "/api/comic/run"})), Some("comic"));
        assert_eq!(page_of("api", &json!({"path": "/api/colorbook/cb_1/run"})), Some("comic"));
        assert_eq!(page_of("api", &json!({"path": "/api/hub/search"})), Some("civitai"));
        assert_eq!(page_of("open_page", &json!({"page": "loras"})), Some("loras"));
        assert_eq!(clip("一二三四五", 3), "一二三…（後面省略）");
    }
}
