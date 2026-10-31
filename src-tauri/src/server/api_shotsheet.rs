//! 拍攝單 API（2026-10-24，子專案 2）：組提示詞、健檢、使用者的危險字、AI 潤飾、存成範本。
//! 邏輯在 crate::shotsheet；網頁在 ui\js\shotsheet.js。
use super::api_lib::item_from;
use super::{bad, blocking, ok, ApiResult, Body, Shared};
use crate::db::Thumb;
use crate::director::{tx, write_atomic};
use crate::shotsheet as sh;
use axum::extract::State;
use serde_json::{json, Value};
use std::path::PathBuf;

/// 使用者自己加的危險字，最多幾個
const MAX_DANGER: usize = 200;
// 同一份字表共用 write_atomic 的暫存檔，寫入必須排隊。
static DANGER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn danger_file(app: &Shared) -> PathBuf {
    app.paths.data.join("h3_sheet_danger.json")
}

fn danger_words(app: &Shared) -> Vec<String> {
    std::fs::read(danger_file(app))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect()
}

fn sheet_of(b: &Value) -> Result<Value, super::ApiError> {
    sh::normalize(&b["sheet"]).map_err(bad)
}

fn other(lang: &str) -> &'static str {
    if lang == "en" {
        "zh"
    } else {
        "en"
    }
}

/// 鏡位按鈕、危險字、時間常數
pub async fn meta(State(app): State<Shared>) -> ApiResult {
    let rules = sh::rules_json();
    ok(json!({
        "cheat": rules["cheat"],
        "rules": {"danger": rules["danger"], "danger_user": danger_words(&app)},
        "limits": {"tail": sh::TAIL_SECS, "handover": sh::HANDOVER_SECS, "early": sh::EARLY_SECS,
                   "min_secs": sh::MIN_SECS, "max_secs": sh::MAX_SECS, "max_segs": sh::MAX_SEGS,
                   "max_cast": sh::MAX_CAST, "max_refs": sh::MAX_REFS},
    }))
}

/// {sheet} → 正規化後的拍攝單、每段的提示詞（拍攝單的語言）和對照（另一種語言）、開拍預估。
/// 手改過的段另外回 kept／kept_mirror：使用者的稿子補回鎖定角色的設定表（拿掉了、改過了都照原樣接回）
pub async fn compose(Body(b): Body) -> ApiResult {
    let s = sheet_of(&b)?;
    let lang = s["lang"].as_str().unwrap_or("zh").to_string();
    let n = s["segments"].as_array().map_or(0, |a| a.len());
    let engine = s["engine"].as_str().unwrap_or("h3");
    let zh = lang != "en";
    let segments: Vec<Value> = (0..n)
        .map(|i| {
            let (head, head_mirror) = (sh::locked_head_at(&s, i, zh), sh::locked_head_at(&s, i, !zh));
            let mut v = json!({"prompt": crate::shot_spec::compose(&s, i, engine, &lang),
                "mirror": crate::shot_spec::compose(&s, i, engine, other(&lang))});
            let seg = &s["segments"][i];
            if seg["manual"] == true && !head.is_empty() {
                v["kept"] = json!(keep_locked(seg["prompt"].as_str().unwrap_or(""), &head, zh));
                v["kept_mirror"] = json!(keep_locked(seg["mirror"].as_str().unwrap_or(""), &head_mirror, !zh));
            }
            v
        })
        .collect();
    let estimate = if engine == "h3" {
        sh::estimate(&s)
    } else {
        vec![]
    };
    ok(json!({"estimate": estimate, "segments": segments, "sheet": s}))
}

/// 手改過的稿子補回鎖定段落（空的稿子不動）
fn keep_locked(prompt: &str, head: &str, zh: bool) -> String {
    if prompt.trim().is_empty() {
        prompt.to_string()
    } else {
        sh::put_locked(prompt, head, zh)
    }
}

/// {sheet, prompts} → {issues}
pub async fn check(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let s = sheet_of(&b)?;
    let prompts: Vec<String> = b["prompts"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| x.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    let rules = sh::rules(&danger_words(&app));
    ok(json!({"issues": sh::check(&s, &prompts, &rules)}))
}

/// {words} → 存使用者的危險字（去空白、去重、最多 200 個）
pub async fn danger(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let input = b["words"]
        .as_array()
        .filter(|a| a.iter().all(Value::is_string))
        .ok_or_else(|| bad("危險字必須是文字陣列"))?;
    let mut words: Vec<String> = Vec::new();
    for w in input {
        let w: String = w.as_str().unwrap_or("").trim().chars().take(60).collect();
        if !w.is_empty() && !words.contains(&w) && words.len() < MAX_DANGER {
            words.push(w);
        }
    }
    let path = danger_file(&app);
    let v = json!(words);
    let guard = DANGER_LOCK.lock().await;
    blocking(move || {
        let _guard = guard;
        write_atomic(&path, &v)
    })
    .await
    .map_err(|e| bad(format!("危險字沒存成：{e}")))?;
    ok(json!({"words": words}))
}

/// 從回覆裡拿出 ```text 區塊（沒寫 text 的 ``` 也收）
fn text_blocks(s: &str) -> Vec<String> {
    // agent_chat 回來前跑過 pe::clean_reply：整段開頭的 ``` 和結尾的 ``` 被剝掉了（```text 只剩 text）。
    // 補回去再拆；收尾只在前面已經有完整一塊時補，單獨一塊沒收尾照舊不收
    let mut t = s.trim().to_string();
    if t.starts_with("text\n") || t.starts_with("text\r\n") {
        t.insert_str(0, "```");
    }
    if t.matches("```").count() >= 3 && t.matches("```").count() % 2 == 1 {
        t.push_str("\n```");
    }
    let mut out = Vec::new();
    let mut rest = t.as_str();
    while let Some(a) = rest.find("```") {
        let after = &rest[a + 3..];
        let body_start = after.find('\n').map_or(after.len(), |i| i + 1);
        let body = &after[body_start..];
        match body.find("```") {
            Some(e) => {
                let t = body[..e].trim();
                if !t.is_empty() {
                    out.push(t.to_string());
                }
                rest = &body[e + 3..];
            }
            None => break,
        }
    }
    out
}

/// {sheet, seg, prompt, brain} → AI 潤飾這一段：{prompt, mirror}
pub async fn polish(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let s = sheet_of(&b)?;
    let n = s["segments"].as_array().map_or(0, |a| a.len());
    let i = b["seg"]
        .as_u64()
        .map(|x| x as usize)
        .filter(|x| *x < n)
        .ok_or_else(|| bad("沒有這一段"))?;
    let lang = s["lang"].as_str().unwrap_or("zh").to_string();
    let zh = lang != "en";
    // 鎖定角色的設定表不給 AI 動：稿子先拿掉、外觀欄清空，潤完原封不動接回
    let (head, head_mirror) = (sh::locked_head_at(&s, i, zh), sh::locked_head_at(&s, i, !zh));
    let draft = match tx(&b["prompt"]) {
        p if p.trim().is_empty() => {
            crate::shot_spec::compose(&s, i, s["engine"].as_str().unwrap_or("h3"), &lang)
        }
        p => p,
    };
    let draft = sh::strip_locked(&draft, &head);
    let (brain, remote) = super::api_h3::brain_of(&app, &b)?;
    let mut part = s.clone();
    let mut locked_names = Vec::new();
    let gone = sh::absent(&s, i);
    for c in part["cast"].as_array_mut().into_iter().flatten() {
        // 這段沒出場的：外觀清掉，AI 才不會把人寫進來
        if gone.iter().any(|g| Some(g.as_str()) == c["speaker"].as_str()) {
            c["look_zh"] = json!("");
            c["look_en"] = json!("");
            c["absent"] = json!(true);
        } else if c["locked"] == true {
            c["look_zh"] = json!("");
            c["look_en"] = json!("");
            locked_names.push(format!("{} {}", c["speaker"].as_str().unwrap_or(""), c["name"].as_str().unwrap_or("")).trim().to_string());
        }
    }
    part["segments"] = json!([s["segments"][i]]);
    for k in ["prompt", "mirror", "manual"] {
        if let Some(o) = part["segments"][0].as_object_mut() {
            o.remove(k);
        }
    }
    let locked_note = if locked_names.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nLocked characters: {}. The program adds their fixed appearance text back verbatim at the start; do not describe their appearance yourself.",
            locked_names.join(", ")
        )
    };
    let req = format!(
        "Output language: {}\nSegment {} of {}, {} seconds.\n\nShot sheet for this segment (JSON):\n{}{}\n\nDraft prompt:\n```text\n{}\n```",
        if lang == "en" { "English" } else { "Traditional Chinese" },
        i + 1,
        n,
        s["segments"][i]["secs"],
        part,
        locked_note,
        draft
    );
    let engine = s["engine"].as_str().unwrap_or("h3");
    let system = if engine == "h3" {
        app.pe.agent_text("agent_h3_sheet")
    } else {
        "Polish the user's single continuous video shot without inventing facts, changing dialogue, timing or continuity states. Preserve each time range and named speaker. Use plain natural-language video instructions; no H3 protocol field names, Picture tags or dialogue markup. Output exactly two fenced text blocks: first in the requested language, second its faithful translation into the other language (English or Traditional Chinese).".to_string()
    };
    let msgs = json!([{"role": "system", "content": system}, {"role": "user", "content": req}]);
    let rid = format!("h3s-{:x}", super::api_director::random_seed());
    let (content, _) = app
        .pe
        .agent_chat(&rid, &msgs, 6144, 0.6, None, &brain, remote.as_ref())
        .await
        .map_err(bad)?;
    let blocks = text_blocks(&content);
    if blocks.len() < 2 {
        return Err(bad("腦袋沒照格式寫，換一顆腦袋或再按一次"));
    }
    ok(json!({"prompt": sh::put_locked(&blocks[0], &head, zh), "mirror": sh::put_locked(&blocks[1], &head_mirror, !zh)}))
}

/// {name, sheet} → 存成資產庫的拍攝單範本（kind=shot），回存好的那筆
pub async fn template(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let s = sheet_of(&b)?;
    let it = item_from(
        &json!({"kind": "shot", "name": b["name"], "family": "h3", "data": sh::template_of(&s)}),
        None,
    )?;
    let _g = app.assets_lock.lock().await;
    let id = app
        .db
        .lib_save(None, &it, &Thumb::Keep)?
        .ok_or_else(|| bad("範本沒存成"))?;
    ok(json!(app.db.lib_item(id)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_blocks_reads_two_fences() {
        let r = "好的：\n```text\nA\nB\n```\n說明\n```\nC\n```";
        assert_eq!(text_blocks(r), ["A\nB", "C"]);
        assert!(text_blocks("```text\n沒收尾").is_empty());
        // agent_chat 的 clean_reply 會剝掉整段開頭的 ``` 和結尾的 ```：模型乖乖只回兩塊時長這樣
        let raw = "```text\nintegrated_multimodal_description: 她轉身。\n```\n```text\nintegrated_multimodal_description: She turns.\n```";
        let cleaned = crate::pe::clean_reply(raw);
        assert_eq!(text_blocks(&cleaned), ["integrated_multimodal_description: 她轉身。", "integrated_multimodal_description: She turns."]);
        assert_eq!(text_blocks(&crate::pe::clean_reply("好的：\n```text\nA\n```\n```text\nB\n```")), ["A", "B"]);
        assert_eq!(other("zh"), "en");
        assert_eq!(other("en"), "zh");
    }

    #[tokio::test]
    async fn compose_returns_manual_prompts_with_locked_head_back() {
        let sheet = json!({"cast": [{"name": "甲", "locked": true, "look_zh": "女性，橘髮。", "look_en": "female, orange hair."}],
            "segments": [
                {"secs": 10, "manual": true, "prompt": "integrated_multimodal_description: 她轉身。", "mirror": "integrated_multimodal_description: She turns."},
                {"secs": 10, "manual": false, "prompt": "舊的"}]});
        let r = compose(Body(json!({"sheet": sheet}))).await.unwrap();
        let v: Value = serde_json::from_slice(&axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap()).unwrap();
        assert_eq!(v["segments"][0]["kept"], "integrated_multimodal_description: S1（甲）：女性，橘髮。她轉身。");
        assert_eq!(v["segments"][0]["kept_mirror"], "integrated_multimodal_description: 甲 (S1): female, orange hair. She turns.");
        assert!(v["segments"][1].get("kept").is_none(), "沒手改的段直接用重組的");
        assert!(v["segments"][1]["prompt"].as_str().unwrap().contains("S1（甲）：女性，橘髮。"));
    }
}
