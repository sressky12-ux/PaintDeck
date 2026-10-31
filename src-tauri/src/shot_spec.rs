//! 共用鏡頭規格：只整理使用者填寫的連戲狀態，並轉成各影片引擎的提示詞。
use serde_json::{json, Value};

fn text(v: &Value, max: usize) -> String {
    v.as_str().unwrap_or("").trim().chars().take(max).collect()
}

pub fn normalize_engine(v: &Value) -> String {
    let e = text(v, 100);
    // 2026-10-30 LTX 退場：舊拍攝單記的 ltx／ltx23 一律當 H3
    if e.is_empty() || e == "h3" || e == "ltx" || e == crate::video_wf::LEGACY_LTX {
        "h3".into()
    } else {
        e
    }
}

/// 空欄位保持空白；不推測人物姿態、持物或光向。
pub fn normalize_state(v: &Value) -> Value {
    let actors: Vec<Value> = v["actors"]
        .as_array()
        .into_iter()
        .flatten()
        .take(8)
        .filter_map(|a| {
            let actor = json!({"who": text(&a["who"], 80), "position": text(&a["position"], 500),
            "pose": text(&a["pose"], 500), "holding": text(&a["holding"], 500),
            "gaze": text(&a["gaze"], 500), "wardrobe": text(&a["wardrobe"], 500)});
            actor
                .as_object()
                .unwrap()
                .values()
                .any(|x| !x.as_str().unwrap_or("").is_empty())
                .then_some(actor)
        })
        .collect();
    json!({"actors": actors, "light": text(&v["light"], 1000), "camera": text(&v["camera"], 1000), "notes": text(&v["notes"], 2000)})
}

pub fn state_empty(v: &Value) -> bool {
    let v = normalize_state(v);
    v["actors"].as_array().is_none_or(Vec::is_empty)
        && ["light", "camera", "notes"]
            .iter()
            .all(|k| v[*k].as_str().unwrap_or("").is_empty())
}

/// 手填開始狀態優先；只有整份開始狀態空白時才沿用前鏡結束。
pub fn effective_entry(sheet: &Value, i: usize) -> Value {
    let seg = &sheet["segments"][i];
    let entry = normalize_state(&seg["entry"]);
    if i > 0 && seg["inherit_entry"].as_bool().unwrap_or(true) && state_empty(&entry) {
        normalize_state(&sheet["segments"][i - 1]["exit"])
    } else {
        entry
    }
}

pub fn state_lines(v: &Value, lang: &str) -> String {
    let v = normalize_state(v);
    let zh = lang != "en";
    let labels = if zh {
        ["位置", "姿態", "持物", "視線", "服裝"]
    } else {
        ["position", "pose", "holding", "gaze", "wardrobe"]
    };
    let mut lines = Vec::new();
    for a in v["actors"].as_array().into_iter().flatten() {
        let mut parts = Vec::new();
        for (key, label) in ["position", "pose", "holding", "gaze", "wardrobe"]
            .iter()
            .zip(labels)
        {
            let t = a[*key].as_str().unwrap_or("");
            if !t.is_empty() {
                parts.push(format!("{label}: {t}"));
            }
        }
        let who = a["who"].as_str().unwrap_or("");
        if !who.is_empty() || !parts.is_empty() {
            lines.push(format!(
                "{}{}{}",
                who,
                if who.is_empty() || parts.is_empty() {
                    ""
                } else {
                    ": "
                },
                parts.join("; ")
            ));
        }
    }
    for (key, label) in if zh {
        [("light", "光向"), ("camera", "鏡頭軸線"), ("notes", "備註")]
    } else {
        [
            ("light", "Lighting"),
            ("camera", "Camera axis"),
            ("notes", "Notes"),
        ]
    } {
        let t = v[key].as_str().unwrap_or("");
        if !t.is_empty() {
            lines.push(format!("{label}: {t}"));
        }
    }
    lines.join("\n")
}

/// H3 保留既有協議；其他引擎只收到通用敘述與原始鏡頭時間，不混入 H3 圖號及重疊區。
pub fn compose(sheet: &Value, i: usize, engine: &str, lang: &str) -> String {
    if engine == "h3" || engine.is_empty() {
        return crate::shotsheet::compose(sheet, i, lang);
    }
    let zh = lang != "en";
    let seg = &sheet["segments"][i];
    let mut lines = Vec::new();
    // 鎖定角色的設定表原封不動放第一行（AI 潤飾前拿掉、潤完接回，見 shotsheet::put_locked）
    let head = crate::shotsheet::locked_head_at(sheet, i, zh);
    if !head.is_empty() {
        lines.push(head);
    }
    for (key, label) in if zh {
        [("style", "畫風"), ("camera", "鏡位與運鏡")]
    } else {
        [("style", "Style"), ("camera", "Camera")]
    } {
        let t = text(&sheet[key], 2000);
        if !t.is_empty() {
            lines.push(format!("{label}: {t}"));
        }
    }
    for c in sheet["cast"].as_array().into_iter().flatten().filter(|c| c["locked"] != true) {
        let own = if zh { "look_zh" } else { "look_en" };
        let other = if zh { "look_en" } else { "look_zh" };
        let look = text(&c[own], 2000);
        let look = if look.is_empty() {
            text(&c[other], 2000)
        } else {
            look
        };
        let name = text(&c["name"], 80);
        let who = text(&c["speaker"], 10);
        if !look.is_empty() {
            lines.push(format!("{who} {name}: {look}"));
        }
    }
    let scene = text(&sheet["scene"]["text"], 2000);
    if !scene.is_empty() {
        lines.push(format!("{}: {scene}", if zh { "場景" } else { "Scene" }));
    }
    let layout = crate::shotsheet::layout_line(&sheet["scene"]["layout"], zh);
    if !layout.is_empty() {
        lines.push(layout);
    }
    let entry = state_lines(&effective_entry(sheet, i), lang);
    if !entry.is_empty() {
        lines.push(format!(
            "{}:\n{entry}",
            if zh { "開始狀態" } else { "Entry state" }
        ));
    }
    let mut beats: Vec<&Value> = seg["beats"].as_array().into_iter().flatten().collect();
    beats.sort_by(|a, b| {
        a["from"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&b["from"].as_f64().unwrap_or(0.0))
    });
    for beat in beats {
        let mut parts = Vec::new();
        let action = text(&beat["action"], 1000);
        if !action.is_empty() {
            parts.push(action);
        }
        let line = text(&beat["line"], 500);
        if !line.is_empty() {
            let who = text(&beat["who"], 80);
            let cast = sheet["cast"]
                .as_array()
                .and_then(|a| a.iter().find(|c| c["speaker"] == beat["who"]));
            let code = text(&beat["lang"], 30);
            let code = if code.is_empty() {
                cast.map(|c| text(&c["lang"], 30)).unwrap_or_default()
            } else {
                code
            };
            let language = crate::shotsheet::lang_name(&code);
            let voice = cast.map(|c| text(&c["voice"], 200)).unwrap_or_default();
            parts.push(format!(
                "{}: {who} [{language}] \"{line}\"{}",
                if zh { "台詞" } else { "Dialogue" },
                if voice.is_empty() {
                    String::new()
                } else {
                    format!(" ({voice})")
                }
            ));
        }
        let sfx = text(&beat["sfx"], 300);
        if !sfx.is_empty() {
            parts.push(format!("{}: {sfx}", if zh { "音效" } else { "Sound" }));
        }
        if !parts.is_empty() {
            let from = beat["from"].as_f64().unwrap_or(0.0);
            let to = beat["to"].as_f64().unwrap_or(0.0);
            lines.push(format!("[{from:.3}-{to:.3}] {}", parts.join(". ")));
        }
    }
    let exit = state_lines(&seg["exit"], lang);
    if !exit.is_empty() {
        lines.push(format!(
            "{}:\n{exit}",
            if zh { "結束狀態" } else { "Exit state" }
        ));
    }
    for (key, label) in if zh {
        [("ambient", "環境音"), ("music", "配樂")]
    } else {
        [("ambient", "Ambient sound"), ("music", "Music")]
    } {
        let t = text(&sheet["sound"][key], 1000);
        if !t.is_empty() {
            lines.push(format!("{label}: {t}"));
        }
    }
    lines.join("\n")
}

/// 拍攝單轉為既有導演專案；image 設定由呼叫端補上，避免改動全域模型偏好。
pub fn director_project(sheet: &Value, prompts: &[String], engine: &str) -> Result<Value, String> {
    let normalized = crate::shotsheet::normalize(sheet)?;
    let sheet = &normalized;
    let count = sheet["segments"].as_array().map_or(0, Vec::len);
    if prompts.len() != count || prompts.iter().any(|p| p.trim().is_empty()) {
        return Err("每個鏡頭都要有提示詞，請先重新組提示詞".into());
    }
    let mut p = crate::director::blank_project(&text(&sheet["title"], 100));
    p["settings"]["video_engine"] = json!(engine);
    p["settings"]["seconds"] = sheet["segments"][0]["secs"].clone();
    p["settings"]["visual"] = sheet["style"].clone();
    p["sheet"] = sheet.clone();
    p["story"] = sheet["scene"]["text"].clone();
    let segments: Vec<Value> = sheet["segments"].as_array().into_iter().flatten().enumerate().map(|(i,s)| {
        let entry = effective_entry(sheet, i);
        let mut frame = Vec::new();
        for k in ["style", "camera"] { let t = text(&sheet[k],2000); if !t.is_empty() { frame.push(t); } }
        let scene = text(&sheet["scene"]["text"], 2000);
        if !scene.is_empty() { frame.push(scene.clone()); }
        for c in sheet["cast"].as_array().into_iter().flatten() {
            let t = text(&c["look_en"],2000);
            let t = if t.is_empty() { text(&c["look_zh"],2000) } else { t };
            if !t.is_empty() { frame.push(t); }
        }
        let state = state_lines(&entry,"en");
        if !state.is_empty() { frame.push(state); }
        let mut v = json!({"title": format!("鏡頭 {}",i+1), "summary": s["beats"].as_array().into_iter().flatten().filter_map(|b| b["action"].as_str()).filter(|x| !x.is_empty()).collect::<Vec<_>>().join("；"),
            "characters": sheet["cast"].as_array().into_iter().flatten().map(|c|c["name"].clone()).collect::<Vec<_>>(),
            "continue": i>0, "scene":scene, "frame":frame.join(". "), "frame_zh":"", "video":prompts.get(i).cloned().unwrap_or_default(), "video_zh":"",
            "seconds":s["secs"], "engine":engine, "entry":entry, "exit":normalize_state(&s["exit"]),
            "inherit_entry":s["inherit_entry"].as_bool().unwrap_or(true), "shot_spec":s});
        if i==0 { if let Some(f)=sheet["first"]["file"].as_str().filter(|f|!f.is_empty()) { v["frame_lock"]=json!(f); } }
        v
    }).collect();
    p["segments"] = json!(segments);
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn entry_inherits_only_when_blank_and_enabled() {
        let mut s = json!({"segments":[{"exit":{"actors":[{"who":"S1","holding":"cup"}],"light":"left"}},{}]});
        assert_eq!(effective_entry(&s, 1)["actors"][0]["holding"], "cup");
        s["segments"][1]["entry"] = json!({"notes":"user choice"});
        assert_eq!(effective_entry(&s, 1)["notes"], "user choice");
        assert!(effective_entry(&s, 1)["actors"]
            .as_array()
            .unwrap()
            .is_empty());
        s["segments"][1]["entry"] = Value::Null;
        s["segments"][1]["inherit_entry"] = json!(false);
        assert!(state_empty(&effective_entry(&s, 1)));
    }
    #[test]
    fn generic_engine_puts_locked_cast_on_first_line() {
        let s = crate::shotsheet::normalize(&json!({"engine": "wan-demo", "style": "anime",
            "cast": [{"name": "Bo", "look_en": "a tall man"}, {"name": "Ada", "locked": true, "look_en": "female, early 20s, silver hair."}],
            "segments": [{"secs": 8}]})).unwrap();
        let p = compose(&s, 0, "wan-demo", "en");
        assert!(p.starts_with("Ada (S2): female, early 20s, silver hair.\n"), "{p}");
        assert_eq!(p.matches("silver hair").count(), 1, "{p}");
        assert!(p.contains("S1 Bo: a tall man"), "{p}");
    }
    #[test]
    fn generic_engine_preserves_time_dialogue_and_user_facts_without_h3_protocol() {
        let s=crate::shotsheet::normalize(&json!({"engine":"wan-demo","cast":[{"name":"Ada","lang":"Japanese"}],"segments":[{"secs":8,"beats":[{"from":1,"to":3,"action":"right hand raises cup","who":"S1","line":"hello"}],"exit":{"actors":[{"who":"S1","holding":"cup"}]}}]})).unwrap();
        let p = compose(&s, 0, "wan-demo", "en");
        assert!(
            p.contains("[1.000-3.000]")
                && p.contains("[Japanese] \"hello\"")
                && p.contains("holding: cup")
        );
        for x in [
            "integrated_multimodal_description",
            "<Picture",
            "<d>",
            "Net New Content",
        ] {
            assert!(!p.contains(x));
        }
        let d = director_project(&s, std::slice::from_ref(&p), "custom-flow").unwrap();
        assert_eq!(d["segments"][0]["video"], p);
        assert_eq!(d["segments"][0]["seconds"], 8.0);
        assert_eq!(d["settings"]["video_engine"], "custom-flow");
    }
    #[test]
    fn blank_state_does_not_invent_facts() {
        assert_eq!(state_lines(&Value::Null, "en"), "");
        let s = crate::shotsheet::normalize(&json!({"segments":[{}]})).unwrap();
        assert_eq!(s["engine"], "h3");
        assert_eq!(s["segments"][0]["inherit_entry"], true);
    }
}
