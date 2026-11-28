//! H3 長片接續（2026-10-23）：一條長片＝好幾段，第 2 段起直接吃上一段的 AV latent 接著拍（社群 Herrgotts H3 Infinite 節點）。
//! 這裡只有純邏輯：存檔格式、組提示詞、組節點圖。API 和背景工作在 server\api_h3chain.rs。
//! 設計在 design\H3長片接續_設計.md，計畫在 design\H3長片接續_實作計畫.md。
//!
//! 雲端上的檔（ComfyUI output 底下）：h3chain/<id>/lat_<段號 5 位>.safetensors（接續用的 AV latent）、
//! h3chain/<id>/seg<段號 2 位>_cut_00001_.mp4（切掉重疊的頭和定住的尾，接片用）、…_end_00001_.mp4（只切頭，最後一段和預覽用）。
//! 本機：<資料>\data\h3_chains\<id>\chain.json＋同名的檔
use crate::director::{new_id, tx, valid_id, write_atomic};
use crate::h3;
use crate::py::now;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// 一條最多幾段
pub const MAX_SEGS: usize = 30;
pub const MIN_SECS: f64 = 5.0;
pub const MAX_SECS: f64 = 15.0;
pub const DEFAULT_SECS: f64 = 10.0;
/// 每 1 秒影片在雲端大約要算幾秒（2026-10-23 L40S、640×960、8 步實測 23～25 秒）
pub const SECS_PER_SEC: f64 = 25.0;
const STEPS: i64 = 8;
const ASPECTS: &[&str] = &["16:9", "9:16", "1:1", "4:3", "3:4", "21:9"];
const QUALITIES: &[&str] = &["fast", "std", "high"];
const STATUSES: &[&str] = &["todo", "running", "done", "error", "stopped", "review"];
/// 在哪拍：runpod＝雲端顯卡（接上一段的 latent，最順）；local＝這台電腦的 ComfyUI（同一套 latent 接續，慢但不花錢）；
/// runninghub＝一段一個任務，用 H3「接著上一支拍」（上一段最後約 0.9 秒畫面＋聲音當開頭引導）
const WHERES: &[&str] = &["runpod", "local", "runninghub"];
/// 本機每 1 秒影片大約要算幾秒（2026-11-13 RTX 3070 8GB、10Eros w4a8、864×480、8 步實測 60～74 秒）
pub const LOCAL_SECS_PER_SEC: f64 = 70.0;
/// 繪匣附帶的小節點（resources\extras\anima_h3_nodes）：把參考圖編碼後放進 minimax_refs
pub const REF_NODE: &str = "AnimaH3AddRefLatents";
/// 每段拍完才有的欄位（網頁存檔不能改，重拍時清掉）
/// 拍攝單的多張參考圖：最多 8 張（第 1 段首幀佔 <Picture 1>，合起來不超過 qwen_reference 的 9 張）
pub const MAX_REFS: usize = 8;
/// 長片裡存的拍攝單，序列化後的上限
const MAX_SHEET_BYTES: usize = 200 * 1024;

pub const RESULT_KEYS: &[&str] = &["status", "error", "seed", "took", "head", "frames", "handover", "files", "check"];

/// 雲端存 latent 的檔名（i＝段號，從 1 算起，跟 SaveLatent 的 clip_index 一樣）
pub fn latent_name(i: usize) -> String {
    format!("lat_{i:05}.safetensors")
}

/// 雲端 output 底下這條長片的子資料夾
pub fn remote_dir(id: &str) -> String {
    format!("h3chain/{id}")
}

/// input 裡的素材檔名：只收英數、底線、減號、點（不能有路徑）
pub(crate) fn file_ok(n: &str) -> bool {
    !n.is_empty() && n.len() <= 128 && !n.starts_with('.') && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn opt_file(v: &Value, what: &str) -> Result<Value, String> {
    match v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(Value::Null),
        Some(n) if file_ok(n) => Ok(json!(n)),
        Some(n) => Err(format!("{what}的檔名不對：{n}（素材要從格子放，不能手填路徑）")),
    }
}

fn pick<'a>(v: &Value, list: &[&'a str], default: &'a str) -> &'a str {
    list.iter().copied().find(|x| v.as_str() == Some(*x)).unwrap_or(default)
}

/// 網頁送來的長片整理成固定格式：補預設值、秒數夾到 5～15、段數 1～30、檔名只能是 input 裡的素材
pub fn normalize(v: &Value) -> Result<Value, String> {
    let s = |k: &str| v[k].as_str().unwrap_or("").trim().to_string();
    let id = s("id");
    if !id.is_empty() && !valid_id(&id) {
        return Err("長片編號不對".into());
    }
    let mut segs = vec![];
    for (i, g) in v["segments"].as_array().into_iter().flatten().take(MAX_SEGS).enumerate() {
        let secs = g["secs"].as_f64().unwrap_or(DEFAULT_SECS).clamp(MIN_SECS, MAX_SECS);
        let lines: Vec<Value> = g["lines"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|l| {
                json!({"at": l["at"].as_f64().unwrap_or(0.0).clamp(0.0, secs), "lang": if tx(&l["lang"]).trim().is_empty() { "Chinese".to_string() } else { tx(&l["lang"]).trim().to_string() },
                       "who": tx(&l["who"]).trim(), "text": tx(&l["text"]).trim()})
            })
            .collect();
        let mut seg = json!({"secs": secs, "text": tx(&g["text"]).trim(), "lines": lines, "last": opt_file(&g["last"], &format!("第 {} 段尾幀", i + 1))?});
        for k in RESULT_KEYS {
            seg[*k] = g.get(*k).cloned().unwrap_or(Value::Null);
        }
        seg["status"] = json!(pick(&g["status"], STATUSES, "todo"));
        // 這段自己挑的參考圖（拍攝單關掉幾張時才有）；沒有＝用整條的
        if let Some(a) = g["refs"].as_array() {
            let mut list = vec![];
            for (k, r) in a.iter().take(MAX_REFS).enumerate() {
                let f = opt_file(r, &format!("第 {} 段第 {} 張參考圖", i + 1, k + 1))?;
                if !f.is_null() {
                    list.push(f);
                }
            }
            seg["refs"] = json!(list);
        }
        for k in ["takes", "selected_take", "pending_take", "draft", "entry", "exit", "inherit_entry"] {
            if let Some(value) = g.get(k) { seg[k] = value.clone(); }
        }
        segs.push(seg);
    }
    if segs.is_empty() {
        segs.push(json!({"secs": DEFAULT_SECS, "text": "", "lines": [], "last": null, "status": "todo"}));
    }
    let mut refs = vec![];
    for (k, r) in v["refs"].as_array().into_iter().flatten().take(MAX_REFS).enumerate() {
        let f = opt_file(r, &format!("第 {} 張參考圖", k + 1))?;
        if !f.is_null() {
            refs.push(f);
        }
    }
    let sheet = match &v["sheet"] {
        x @ Value::Object(_) if serde_json::to_vec(x).map_or(usize::MAX, |b| b.len()) > MAX_SHEET_BYTES => return Err("拍攝單太大（超過 200 KB）".into()),
        x @ Value::Object(_) => x.clone(),
        _ => Value::Null,
    };
    let size = v["size"].as_array().filter(|a| a.len() == 2 && a.iter().all(|x| x.as_i64().is_some_and(|n| n > 0))).map(|a| json!(a)).unwrap_or(Value::Null);
    Ok(json!({
        "id": id, "title": if s("title").is_empty() { "未命名長片".to_string() } else { s("title") },
        "created": v["created"].as_f64().unwrap_or_else(now), "updated": v["updated"].as_f64().unwrap_or_else(now),
        "aspect": pick(&v["aspect"], ASPECTS, "9:16"), "quality": pick(&v["quality"], QUALITIES, "std"), "size": size,
        "look": s("look"), "scene": s("scene"), "style": s("style"), "voice": s("voice"), "sound": s("sound"),
        "ref": opt_file(&v["ref"], "角色參考圖")?, "first": opt_file(&v["first"], "第 1 段首幀")?,
        "te": h3::te_name(&v["te"])?, "refs": refs, "sheet": sheet, "review": v["review"].as_bool().unwrap_or(false), "where": pick(&v["where"], WHERES, "runpod"),
        // 接續段把參考圖（沒有就用第 1 段首幀）也送進 DiT：預設開，舊存檔沒有這欄也算開
        "ref_dit": v["ref_dit"].as_bool().unwrap_or(true),
        "segments": segs, "final": v.get("final").cloned().unwrap_or(Value::Null),
    }))
}

pub fn size_locked(c: &Value) -> bool {
    c["size"].is_array() || c["segments"].as_array().into_iter().flatten().any(|s| {
        s["status"] == "done" || s["selected_take"].is_string() || s["pending_take"].is_string()
            || s["takes"].as_array().is_some_and(|takes| !takes.is_empty())
    })
}

/// 已經有拍好的段時不能改比例、畫質（latent 的尺寸會對不上）
pub fn check_edit(old: &Value, new: &Value) -> Result<(), String> {
    let old_segs = old["segments"].as_array().cloned().unwrap_or_default();
    let new_segs = new["segments"].as_array().cloned().unwrap_or_default();
    if old_segs.len() != new_segs.len() {
        for (i, seg) in old_segs.iter().enumerate() {
            let protected = seg["takes"].as_array().is_some_and(|a| !a.is_empty()) || seg["status"] == "done" || seg["pending_take"].is_string();
            if protected && (new_segs.get(i).is_none() || ["secs", "text", "lines", "last", "refs"].iter().any(|k| seg[*k] != new_segs[i][*k])) {
                return Err("已有拍攝版本的段落不能移位或刪除，請另存新的長片".into());
            }
        }
    }
    if size_locked(old) && (old["aspect"] != new["aspect"] || old["quality"] != new["quality"]) {
        return Err("長片已有畫面尺寸或拍攝版本；要改比例或畫質，請另存新的長片".into());
    }
    if size_locked(old) && old["where"].as_str().unwrap_or("runpod") != new["where"].as_str().unwrap_or("runpod") {
        return Err("長片已經有拍好的段：兩種拍法接不起來，要換地方拍請另存新的長片".into());
    }
    Ok(())
}

/// 第 n 段（0 起算）以後全部作廢：後面的段是接前面的 latent 拍的，前面重拍了就對不上
pub fn reset_from(c: &mut Value, n: usize) {
    if let Some(segs) = c["segments"].as_array_mut() {
        for s in segs.iter_mut().skip(n) {
            crate::film_takes::ensure_legacy(s, RESULT_KEYS);
            crate::film_takes::clear_current(s, RESULT_KEYS);
        }
    }
    c["final"] = Value::Null;
    if n == 0 {
        c["size"] = Value::Null;
    }
}

/// 還沒拍的段大約要算幾秒
pub fn est_secs(c: &Value) -> f64 {
    let rate = if on_local(c) { LOCAL_SECS_PER_SEC } else { SECS_PER_SEC };
    c["segments"].as_array().into_iter().flatten().filter(|s| s["status"] != "done").map(|s| s["secs"].as_f64().unwrap_or(DEFAULT_SECS) * rate).sum()
}

/// 一句話：去頭尾空白，沒有句號就補一個
fn sentence(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    Some(if t.ends_with(['.', '!', '?', '。', '！', '？']) { t.to_string() } else { format!("{t}.") })
}

/// 第 i 段（0 起算）的提示詞：外觀、場景、畫風、這段的畫面，每句台詞「At 2.0 seconds she says to the viewer in <聲音>: <d>[語言] 台詞</d>」
/// （聲音描述寫在 <d> 前面，第七十五筆的規則），最後是環境音
pub fn seg_prompt(c: &Value, i: usize) -> String {
    seg_prompt_with(c, i, 0)
}

/// dit_refs＝這段有幾張參考圖同時送進 DiT（<Picture 1>～<Picture N>）：開頭先交代它們是參考圖、不是影片的畫格
pub fn seg_prompt_with(c: &Value, i: usize, dit_refs: usize) -> String {
    let seg = &c["segments"][i];
    let note = match dit_refs {
        0 => None,
        1 => Some("<Picture 1> is a reference image, not a frame of the video: whatever it shows (characters, outfits, setting) looks the same in the video, and the whole video keeps the line art, coloring and color palette of <Picture 1>.".to_string()),
        n => Some(format!("<Picture 1> to <Picture {n}> are reference images, not frames of the video: whatever they show (characters, outfits, setting) looks the same in the video, and the whole video keeps their line art, coloring and color palette.")),
    };
    let mut parts: Vec<String> = note.into_iter().chain([&c["look"], &c["scene"], &c["style"], &seg["text"]].iter().filter_map(|v| sentence(&tx(v)))).collect();
    let voice = tx(&c["voice"]);
    for l in seg["lines"].as_array().into_iter().flatten() {
        let text = tx(&l["text"]);
        if text.trim().is_empty() {
            continue;
        }
        let who = match tx(&l["who"]).trim() {
            "" => "she".to_string(),
            w => w.to_string(),
        };
        let how = if voice.trim().is_empty() { String::new() } else { format!(" in {}", voice.trim()) };
        parts.push(format!("At {:.1} seconds {who} says to the viewer{how}: <d>[{}] {}</d>", l["at"].as_f64().unwrap_or(0.0), tx(&l["lang"]), text.trim()));
    }
    parts.extend(sentence(&tx(&c["sound"])));
    parts.join(" ")
}

pub fn on_runninghub(c: &Value) -> bool {
    c["where"] == "runninghub"
}

pub fn on_local(c: &Value) -> bool {
    c["where"] == "local"
}

/// 節點圖跟著機器變的部分：用哪顆主模型、要不要低顯存切塊、那台 ComfyUI 有沒有 REF_NODE
#[derive(Clone, Debug)]
pub struct GraphOpts {
    pub model: String,
    pub te: String,
    pub turbo_lora: bool,
    /// MiniMaxChunkFeedForward 切幾塊（小顯存卡用，結果一樣）；None＝不切
    pub chunks: Option<i64>,
    pub ref_node: bool,
    /// 上一段 latent 的路徑（相對 ComfyUI output）；None＝固定的 h3chain/<id>/lat_0000N.safetensors。
    /// 本機一定要給不重複的檔名：Windows 上 ComfyUI 讀過的 safetensors 會一直被對應在記憶體裡（節點輸出快取著），
    /// 同名檔覆寫不了也刪不掉（2026-11-13 實測「user-mapped section open」），重拍同一段會卡在放檔那一步
    pub latent_in: Option<String>,
}

impl Default for GraphOpts {
    fn default() -> Self {
        GraphOpts { model: h3::MODEL.to_string(), te: h3::TE.into(), turbo_lora: false, chunks: None, ref_node: false, latent_in: None }
    }
}

/// 第 i 段要同時送進 DiT 的參考圖（input 檔名，照 <Picture 1>… 的順序）。
/// 只有這段沒有首幀／尾幀佔掉 <Picture> 編號時才送（那種編號對不上的接法沒測過）；
/// 這段沒有參考圖時，接續段用第 1 段的首幀當參考（2026-11-13 對照：洋裝、髮色守得住）。長片關掉 ref_dit＝空的
pub fn dit_refs(c: &Value, i: usize) -> Vec<String> {
    if c["ref_dit"] == false || c["segments"][i]["last"].is_string() || (i == 0 && c["first"].is_string()) {
        return vec![];
    }
    let own: Vec<String> = seg_refs(c, i).into_iter().filter_map(|r| r.as_str().map(str::to_string)).collect();
    if !own.is_empty() || i == 0 || c["segments"][i]["refs"].is_array() {
        return own;
    }
    c["first"].as_str().map(|f| vec![f.to_string()]).unwrap_or_default()
}

/// RunningHub 版第 i 段送給 h3::build 的參數（素材板模式）：
/// 第 1 段＝參考圖（＋首幀釘在第 0 秒）；第 2 段起＝上一段的影片當「接著拍」（帶聲音）＋參考圖。
/// 開頭那 CONTINUE_FRAMES 格是上一段尾巴的重疊，先加回去，拍完再切掉。
/// H3 的總格數只能是 17k＋5：照「這段自己的格數＋重疊」找最近的一格（最多 15 秒的 362 格）。
/// 2026-11-13 以前是秒數直接加 22/24 再往上對齊，10 秒一段切掉重疊後變 255 格（10.6 秒），每段都多半秒；
/// 現在 238 格（9.9 秒）。15 秒的段被 362 格的上限卡住，切掉重疊後只有 340 格（14.2 秒）
pub fn rh_params(c: &Value, i: usize, prev: Option<&str>, seed: i64) -> Value {
    let seg = &c["segments"][i];
    let mut items: Vec<Value> = seg_refs(c, i).into_iter().filter_map(|r| r.as_str()).map(|n| json!({"kind": "image", "name": n, "use": "ref"})).collect();
    let mut secs = seg["secs"].as_f64().unwrap_or(DEFAULT_SECS);
    match prev {
        Some(v) if i > 0 => {
            items.insert(0, json!({"kind": "video", "name": v, "use": "continue", "audio": true}));
            let want = h3::frames(secs) + h3::CONTINUE_FRAMES;
            let total = (17 * (((want - 5) as f64 / 17.0).round() as i64) + 5).min(h3::frames(15.0));
            secs = total as f64 / h3::FPS as f64;
        }
        _ => {
            if let Some(f) = c["first"].as_str() {
                items.push(json!({"kind": "image", "name": f, "use": "anchor", "secs": 0.0}));
            }
        }
    }
    // 這段釘了尾幀：釘在最後一格（-1）
    if let Some(l) = seg["last"].as_str() {
        items.push(json!({"kind": "image", "name": l, "use": "anchor", "secs": -1.0}));
    }
    let mut p = json!({"h3_mode": "board", "items": items, "prompt": seg_prompt(c, i), "seconds": secs,
        "aspect": c["aspect"], "quality": c["quality"], "steps": STEPS, "sampler": "res_multistep", "scheduler": "simple", "seed": seed});
    if let Some(sz) = c["size"].as_array() {
        p["src_wh"] = json!(sz);
    }
    p
}

/// 畫面尺寸：第一段拍了就固定（存在 size），還沒拍照比例、畫質算
pub fn size_of(c: &Value) -> (i64, i64) {
    match c["size"].as_array().map(|a| (a[0].as_i64(), a[1].as_i64())) {
        Some((Some(w), Some(h))) => (w, h),
        _ => h3::canvas(&tx(&c["aspect"]), &tx(&c["quality"]), None),
    }
}

/// 第 i 段（0 起算）的節點圖。照 2026-10-23 實驗（D:\AI\小克工作區\研究\h3_chain_src\spike_chain.py）：
/// 第 1 段 Start，第 2 段起 LoadLatent（上一段的 latent）→ Continue；存 latent、cut／end 兩支影片、交棒資訊（PreviewAny）
/// 第 i 段送的參考圖：這段自己挑過就用這段的（空陣列＝這段一張都不送），不然用整條的；都沒有照舊用一張 ref
pub fn seg_refs(c: &Value, i: usize) -> Vec<&Value> {
    if let Some(a) = c["segments"][i]["refs"].as_array() {
        return a.iter().collect();
    }
    match c["refs"].as_array().filter(|a| !a.is_empty()) {
        Some(a) => a.iter().collect(),
        None => vec![&c["ref"]],
    }
}

/// 第 i 段節點圖要載的素材檔（input 裡的檔名）：參考圖（多張或舊的一張）、第 1 段的首幀、這段的尾幀
pub fn inputs_of(c: &Value, i: usize) -> Vec<String> {
    let first = if i == 0 { &c["first"] } else { &Value::Null };
    seg_refs(c, i).into_iter().chain([first, &c["segments"][i]["last"]]).filter_map(|v| v.as_str().map(str::to_string)).collect()
}

pub fn seg_graph(c: &Value, i: usize, seed: i64) -> Result<Map<String, Value>, String> {
    seg_graph_with(c, i, seed, &GraphOpts::default())
}

pub fn seg_graph_with(c: &Value, i: usize, seed: i64, o: &GraphOpts) -> Result<Map<String, Value>, String> {
    let id = tx(&c["id"]);
    if !valid_id(&id) {
        return Err("長片編號不對".into());
    }
    let seg = c["segments"].get(i).ok_or("沒有這一段")?;
    let (w, h) = size_of(c);
    let secs = seg["secs"].as_f64().unwrap_or(DEFAULT_SECS);
    let sub = remote_dir(&id);
    let mut g = Map::new();
    let mut add = |cls: &str, inputs: Value| -> String {
        let k = (g.len() + 1).to_string();
        g.insert(k.clone(), json!({"class_type": cls, "inputs": inputs}));
        k
    };
    let mut unet = if o.model.to_lowercase().ends_with(".gguf") {
        add("UnetLoaderGGUF", json!({"unet_name": o.model}))
    } else {
        add("UNETLoader", json!({"unet_name": o.model, "weight_dtype": "default"}))
    };
    if o.turbo_lora {
        unet = add("LoraLoaderModelOnly", json!({"model": [unet, 0], "lora_name": h3::TURBO_REF2VA, "strength_model": 1.0}));
    }
    if let Some(n) = o.chunks.filter(|n| (2..=64).contains(n)) {
        unet = add("MiniMaxChunkFeedForward", json!({"model": [unet, 0], "chunks": n, "seq_threshold": 4096}));
    }
    let dit: Vec<String> = if o.ref_node { dit_refs(c, i) } else { vec![] };
    let clip = add("CLIPLoader", json!({"clip_name": o.te, "type": "minimax", "device": "default"}));
    let vae = add("VAELoader", json!({"vae_name": h3::VAE}));
    let avae = add("VAELoader", json!({"vae_name": h3::AVAE}));
    let mut cond_in = json!({"clip": [clip, 0], "vae": [vae, 0], "prompt": seg_prompt_with(c, i, dit.len()), "width": w, "height": h, "duration": secs, "ref_image_size": "match"});
    // 同時送進 DiT 的參考圖：LoadImage 的節點編號留著，後面接 REF_NODE
    let mut dit_imgs: Vec<String> = vec![];
    let mut img = |k: &str, v: &Value| {
        if let Some(n) = v.as_str() {
            let li = add("LoadImage", json!({"image": n}));
            cond_in[k] = json!([li.clone(), 0]);
            if k.starts_with("qwen_reference_") {
                dit_imgs.push(li);
            }
        }
    };
    if dit.is_empty() {
        // 拍攝單的多張參考圖每段都接（這段挑過就用這段的）；沒有就照舊用一張 ref
        for (k, r) in seg_refs(c, i).into_iter().enumerate() {
            img(&format!("qwen_reference_{}", k + 1), r);
        }
    } else {
        for (k, r) in dit.iter().enumerate() {
            img(&format!("qwen_reference_{}", k + 1), &json!(r));
        }
    }
    img("last_frame", &seg["last"]);
    let (cond, head) = if i == 0 {
        img("first_frame", &c["first"]);
        (add("H3ContinuousStartV14", cond_in), None)
    } else {
        let load = add("H3ContinuousLoadLatent", json!({"latent_path": o.latent_in.clone().unwrap_or_else(|| format!("{sub}/{}", latent_name(i))), "clip_index": 0}));
        for (k, v) in [
            ("previous_latent", json!([load, 0])),
            ("handover", json!([load, 3])),
            ("masked_context_frames", json!("39")),
            ("audio_feather_ticks", json!(0)),
            ("duration_mode", json!("Net New Content")),
            ("audio_tail_carryover", json!("Full Previous Tail")),
        ] {
            cond_in[k] = v;
        }
        let cont = add("H3ContinuousContinueV14", cond_in);
        add("PreviewAny", json!({"source": [cont, 4]}));
        (cont.clone(), Some(json!([cont, 2])))
    };
    // 參考圖也送進 DiT（minimax_refs）：跟 qwen_reference 同一批圖、同一個順序
    let cond_out = if dit.is_empty() {
        json!([cond, 0])
    } else {
        let mut inputs = json!({"positive": [cond, 0], "vae": [vae, 0], "latent": [cond, 1], "ref_image_size": "match"});
        for (k, li) in dit_imgs.iter().enumerate() {
            inputs[format!("image_{}", k + 1)] = json!([li, 0]);
        }
        json!([add(REF_NODE, inputs), 0])
    };
    // 拍攝單勾了「用 RefMod 鎖臉」：條件先接 RefMod（refmod.rs）再進 guider
    let refmods = crate::refmod::names_of(c)?;
    let positive = if refmods.is_empty() {
        cond_out
    } else {
        let mods = add("MiniMaxH3RefModsLoader", crate::refmod::loader_inputs(&refmods));
        json!([add("MiniMaxH3RefModApply", crate::refmod::apply_inputs(cond_out, json!([mods, 0]))), 0])
    };
    let guider = add("BasicGuider", json!({"model": [unet, 0], "conditioning": positive}));
    let sampler = add("KSamplerSelect", json!({"sampler_name": "res_multistep"}));
    let sig = add("BasicScheduler", json!({"model": [unet, 0], "scheduler": "simple", "steps": STEPS, "denoise": 1.0}));
    let noise = add("RandomNoise", json!({"noise_seed": seed}));
    let s = add("SamplerCustomAdvanced", json!({"noise": [noise, 0], "guider": [guider, 0], "sampler": [sampler, 0], "sigmas": [sig, 0], "latent_image": [cond, 1]}));
    let img = add("VAEDecode", json!({"samples": [s, 0], "vae": [vae, 0]}));
    let aud = add("VAEDecodeAudio", json!({"samples": [s, 0], "vae": [avae, 0]}));
    // 數字照節點的 Balanced 預設（2026-10-23 從雲端 object_info 讀的）
    let ho = add(
        "H3ContinuousAnalyzeHandoverV14",
        json!({"images": [img, 0], "preset": "Balanced", "analysis_window": 72, "freeze_hold": 8, "safety_margin": 3, "context_frames": "39", "analysis_size": 192,
               "final_mean_diff_threshold": 0.012, "final_active_pixel_threshold": 0.025, "max_final_active_area_percent": 3.0,
               "transition_mean_diff_threshold": 0.002, "transition_active_pixel_threshold": 0.01, "max_transition_active_area_percent": 1.0,
               "min_static_transition_percent": 70.0, "max_consecutive_motion_outliers": 2, "final_reference_frames": 15, "min_final_match_percent": 75.0,
               "max_consecutive_final_outliers": 3, "safety_mode": "fixed"}),
    );
    add("PreviewAny", json!({"source": [ho, 1]}));
    let mut save = json!({"latent": [s, 0], "filename_prefix": format!("{sub}/lat"), "clip_index": i + 1, "handover": [ho, 0]});
    if let Some(hd) = &head {
        save["head_context_frames"] = hd.clone();
    }
    let sv = add("H3ContinuousSaveLatent", save);
    add("PreviewAny", json!({"source": [sv, 1]}));
    for (mode, tag) in [("Stitch Ready", "cut"), ("Final Clip", "end")] {
        let mut so_in = json!({"images": [img, 0], "output_mode": mode, "handover": [ho, 0], "audio": [aud, 0]});
        if let Some(hd) = &head {
            so_in["head_context_frames"] = hd.clone();
        }
        let so = add("H3ContinuousStitchOutputV14", so_in);
        let cv = add("CreateVideo", json!({"images": [so, 0], "fps": h3::FPS as f64, "audio": [so, 1]}));
        add("SaveVideo", json!({"video": [cv, 0], "filename_prefix": format!("{sub}/seg{:02}_{tag}", i + 1), "format": "auto", "format.codec": "auto"}));
    }
    Ok(g)
}

/// 長片存檔：<資料>\data\h3_chains\<id>\chain.json，拍好的影片、latent 也放同一個資料夾
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn new(data: &Path) -> Store {
        Store { dir: data.join("h3_chains") }
    }
    pub fn seg_dir(&self, id: &str) -> PathBuf {
        self.dir.join(id)
    }
    fn file(&self, id: &str) -> PathBuf {
        self.dir.join(id).join("chain.json")
    }
    pub fn get(&self, id: &str) -> Option<Value> {
        if !valid_id(id) {
            return None;
        }
        std::fs::read(self.file(id)).ok().and_then(|b| serde_json::from_slice(&b).ok())
    }
    /// 存檔（沒有編號就給一個新的），回傳存進去的樣子
    pub fn save(&self, c: &Value) -> Result<Value, String> {
        let mut c = c.clone();
        if !valid_id(&tx(&c["id"])) {
            c["id"] = json!(new_id());
        }
        c["updated"] = json!(now());
        write_atomic(&self.file(&tx(&c["id"])), &c).map_err(|e| format!("存長片失敗：{e}"))?;
        Ok(c)
    }
    /// 清單（新的在前）：只回摘要
    pub fn list(&self) -> Vec<Value> {
        let mut v: Vec<Value> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| self.get(&e.file_name().to_string_lossy()))
            .map(|c| {
                let segs = c["segments"].as_array().cloned().unwrap_or_default();
                json!({"id": c["id"], "title": c["title"], "updated": c["updated"], "segments": segs.len(),
                       "done": segs.iter().filter(|s| s["status"] == "done").count(), "final": c["final"]})
            })
            .collect();
        v.sort_by(|a, b| b["updated"].as_f64().unwrap_or(0.0).total_cmp(&a["updated"].as_f64().unwrap_or(0.0)));
        v
    }
    /// 不真的刪：整個資料夾搬到 <trash>\H3長片\
    pub fn trash(&self, id: &str, trash: &Path) -> Result<(), String> {
        if !valid_id(id) || !self.seg_dir(id).is_dir() {
            return Err("找不到這條長片".into());
        }
        let to = trash.join("H3長片");
        std::fs::create_dir_all(&to).map_err(|e| e.to_string())?;
        std::fs::rename(self.seg_dir(id), to.join(format!("{id}_{}", chrono::Local::now().format("%Y%m%d_%H%M%S")))).map_err(|e| format!("搬到待刪除失敗：{e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chain(segs: Value) -> Value {
        normalize(&json!({"id": "abc", "title": "t", "aspect": "9:16", "quality": "std",
                          "look": "Xiaoke has long orange hair", "scene": "A small bedroom at night.", "style": "2.5D anime",
                          "voice": "a soft voice", "sound": "", "ref": "ref_1.png", "first": "first_1.png", "segments": segs}))
        .unwrap()
    }
    fn classes(g: &Map<String, Value>, c: &str) -> Vec<Value> {
        g.values().filter(|n| n["class_type"] == c).cloned().collect()
    }

    #[test]
    fn sheet_refmods_feed_every_segment_guider() {
        let sheet = json!({"refmod": true, "cast": [{"name": "甲", "locked": true, "refmod": "paintdeck/pd_char_7"}]});
        let c = normalize(&json!({"id": "abc", "sheet": sheet, "segments": [{"text": "a"}, {"text": "b"}]})).unwrap();
        for i in 0..2 {
            let g = seg_graph(&c, i, 1).unwrap();
            let guider = &classes(&g, "BasicGuider")[0];
            let apply = &g[guider["inputs"]["conditioning"][0].as_str().unwrap()];
            assert_eq!(apply["class_type"], "MiniMaxH3RefModApply", "第 {i} 段");
            let src = &g[apply["inputs"]["conditioning"][0].as_str().unwrap()]["class_type"];
            assert_eq!(src, if i == 0 { "H3ContinuousStartV14" } else { "H3ContinuousContinueV14" });
            assert_eq!(classes(&g, "MiniMaxH3RefModsLoader")[0]["inputs"]["mod_1"], "paintdeck/pd_char_7");
        }
    }

    /// 舊長片（沒有 refs、sheet）的節點圖，存在 tests\golden\chain_graph_v1.json；ANIMA_UPDATE_GOLDEN=1 時重寫
    #[test]
    fn old_chain_graph_unchanged() {
        let mut v = json!({"id": "abc", "title": "t", "aspect": "9:16", "quality": "std", "created": 1.0, "updated": 1.0,
                           "look": "Xiaoke has long orange hair", "scene": "A small bedroom at night.", "style": "2.5D anime",
                           "voice": "a soft voice", "sound": "rain", "ref": "ref_1.png", "first": "first_1.png",
                           "segments": [{"text": "she smiles", "lines": [{"at": 2, "who": "Xiaoke", "text": "hi"}]}, {"text": "she waves", "last": "last_2.png"}]});
        let c = normalize(&v).unwrap();
        let got = json!([seg_graph(&c, 0, 1).unwrap(), seg_graph(&c, 1, 1).unwrap()]);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden").join("chain_graph_v1.json");
        if std::env::var("ANIMA_UPDATE_GOLDEN").is_ok() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, serde_json::to_string_pretty(&got).unwrap()).unwrap();
        }
        let want: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(got, want);
        // 空的 refs 等於沒有
        v["refs"] = json!([]);
        assert_eq!(json!(seg_graph(&normalize(&v).unwrap(), 0, 1).unwrap()), want[0]);
    }

    #[test]
    fn refs_become_qwen_references() {
        let mut v = json!({"id": "abc", "ref": "old.png", "first": "first_1.png", "refs": ["a.png", "b.png"], "segments": [{"text": "x"}, {"text": "y"}]});
        let c = normalize(&v).unwrap();
        for i in 0..2 {
            let g = seg_graph(&c, i, 1).unwrap();
            let node = classes(&g, if i == 0 { "H3ContinuousStartV14" } else { "H3ContinuousContinueV14" }).pop().unwrap_or_default();
            let inp = &node["inputs"];
            assert!(inp["qwen_reference_1"].is_array() && inp["qwen_reference_2"].is_array() && inp.get("qwen_reference_3").is_none(), "{i}: {inp}");
            let loads: Vec<String> = classes(&g, "LoadImage").iter().map(|n| n["inputs"]["image"].as_str().unwrap().to_string()).collect();
            assert!(loads.contains(&"a.png".into()) && loads.contains(&"b.png".into()) && !loads.contains(&"old.png".into()), "{loads:?}");
        }
        v["refs"] = json!([]);
        let g = seg_graph(&normalize(&v).unwrap(), 1, 1).unwrap();
        let loads: Vec<Value> = classes(&g, "LoadImage").iter().map(|n| n["inputs"]["image"].clone()).collect();
        assert!(loads.contains(&json!("old.png")), "沒有 refs 就照舊用 ref");
    }

    #[test]
    fn runninghub_params_continue_from_previous_video() {
        let c = normalize(&json!({"id": "abc", "where": "runninghub", "first": "f.png", "refs": ["a.png", "b.png"], "size": [608, 1056],
            "segments": [{"secs": 8, "text": "她轉身"}, {"secs": 8, "text": "她揮手", "refs": ["b.png"]}]})).unwrap();
        assert!(on_runninghub(&c));
        let p0 = rh_params(&c, 0, None, 7);
        assert_eq!(p0["h3_mode"], "board");
        assert_eq!(p0["items"], json!([{"kind": "image", "name": "a.png", "use": "ref"}, {"kind": "image", "name": "b.png", "use": "ref"},
            {"kind": "image", "name": "f.png", "use": "anchor", "secs": 0.0}]));
        assert_eq!(p0["seconds"], 8.0);
        assert_eq!(p0["src_wh"], json!([608, 1056]));
        let p1 = rh_params(&c, 1, Some("h3_prev.mp4"), 7);
        assert_eq!(p1["items"][0], json!({"kind": "video", "name": "h3_prev.mp4", "use": "continue", "audio": true}));
        assert_eq!(p1["items"].as_array().unwrap().len(), 2, "這段只送自己挑的參考圖、不再釘首幀");
        // 8 秒＝192 格，加重疊 22 格＝214，最近的 17k＋5 是 209（切掉重疊剩 187 格、7.8 秒；以前往上對齊成 226、剩 204 格、8.5 秒）
        assert!((p1["seconds"].as_f64().unwrap() - 209.0 / 24.0).abs() < 1e-9, "重疊的格數先加回去，總格數對齊最近的 17k＋5");
        for secs in [5.0, 8.0, 10.0, 12.0, 15.0] {
            let mut cs = c.clone();
            cs["segments"][1]["secs"] = json!(secs);
            let total = crate::h3::frames(rh_params(&cs, 1, Some("h3_prev.mp4"), 7)["seconds"].as_f64().unwrap());
            assert_eq!(total % 17, 5, "{secs} 秒");
            assert!(total <= 362, "{secs} 秒");
            let net = total - crate::h3::CONTINUE_FRAMES;
            if secs < 15.0 {
                assert!((net - crate::h3::frames(secs)).abs() <= 8, "{secs} 秒：切掉重疊後 {net} 格，跟第 1 段同秒數差不到半個 17 格");
            }
        }
        assert!(p1["prompt"].as_str().unwrap().contains("她揮手"));
        // 真的組得出節點圖（接續的影片走 AddGuide）
        let input = std::env::temp_dir();
        let (g, _) = crate::h3::build(&p1, &input, 0).unwrap();
        assert!(g.values().any(|n| n["class_type"] == "LoadVideo" && n["inputs"]["file"] == "h3_prev.mp4"));
        assert!(g.values().any(|n| n["class_type"] == "MiniMaxH3AddGuide"));
        assert_eq!(normalize(&json!({"where": "nope"})).unwrap()["where"], "runpod");
        let mut c2 = c.clone();
        c2["segments"][1]["last"] = json!("l.png");
        let p2 = rh_params(&c2, 1, Some("h3_prev.mp4"), 7);
        assert_eq!(p2["items"].as_array().unwrap().last().unwrap(), &json!({"kind": "image", "name": "l.png", "use": "anchor", "secs": -1.0}));
    }

    #[test]
    fn cannot_switch_where_after_a_segment_is_shot() {
        let old = normalize(&json!({"where": "runpod", "segments": [{"status": "done"}]})).unwrap();
        let new = normalize(&json!({"where": "runninghub", "segments": [{"status": "done"}]})).unwrap();
        assert!(check_edit(&old, &new).unwrap_err().contains("另存"));
        let fresh = normalize(&json!({"where": "runpod", "segments": [{}]})).unwrap();
        assert!(check_edit(&fresh, &normalize(&json!({"where": "runninghub", "segments": [{}]})).unwrap()).is_ok());
    }

    #[test]
    fn segment_refs_override_the_chain_refs() {
        let c = normalize(&json!({"id": "abc", "first": "f.png", "refs": ["a.png", "b.png"], "segments": [{}, {"refs": ["b.png"]}, {"refs": []}]})).unwrap();
        let loads = |i: usize| -> Vec<String> {
            classes(&seg_graph(&c, i, 1).unwrap(), "LoadImage").iter().map(|n| n["inputs"]["image"].as_str().unwrap().to_string()).collect()
        };
        assert!(loads(0).contains(&"a.png".into()) && loads(0).contains(&"b.png".into()));
        let g1 = seg_graph(&c, 1, 1).unwrap();
        let cont = &classes(&g1, "H3ContinuousContinueV14")[0]["inputs"];
        assert!(cont["qwen_reference_1"].is_array() && cont.get("qwen_reference_2").is_none(), "{cont}");
        assert_eq!(loads(1), ["b.png"]);
        assert!(loads(2).is_empty(), "空陣列＝這段一張都不送");
        assert_eq!(inputs_of(&c, 1), ["b.png"]);
        assert!(c["segments"][0].get("refs").is_none(), "沒挑過的段不存 refs，照用整條的");
        assert!(normalize(&json!({"segments": [{"refs": ["..\\a.png"]}]})).is_err());
    }

    #[test]
    fn inputs_of_lists_what_the_graph_loads() {
        let c = normalize(&json!({"ref": "old.png", "first": "f.png", "refs": ["a.png", "b.png"], "segments": [{}, {"last": "l.png"}]})).unwrap();
        assert_eq!(inputs_of(&c, 0), ["a.png", "b.png", "f.png"]);
        assert_eq!(inputs_of(&c, 1), ["a.png", "b.png", "l.png"]);
        let old = normalize(&json!({"ref": "old.png", "first": "f.png", "segments": [{}]})).unwrap();
        assert_eq!(inputs_of(&old, 0), ["old.png", "f.png"]);
        // 每一個都是節點圖真的會載的檔
        for i in 0..2 {
            let g = seg_graph(&normalize(&json!({"id": "abc", "ref": "old.png", "first": "f.png", "refs": ["a.png"], "segments": [{}, {"last": "l.png"}]})).unwrap(), i, 1).unwrap();
            let mut loads: Vec<String> = classes(&g, "LoadImage").iter().map(|n| n["inputs"]["image"].as_str().unwrap().to_string()).collect();
            loads.sort();
            let mut want = inputs_of(&normalize(&json!({"ref": "old.png", "first": "f.png", "refs": ["a.png"], "segments": [{}, {"last": "l.png"}]})).unwrap(), i);
            want.sort();
            assert_eq!(loads, want, "{i}");
        }
    }

    #[test]
    fn refs_capped_and_checked() {
        let nine: Vec<String> = (1..=9).map(|n| format!("r{n}.png")).collect();
        let c = normalize(&json!({"refs": nine, "segments": [{}]})).unwrap();
        assert_eq!(c["refs"].as_array().unwrap().len(), MAX_REFS);
        assert!(normalize(&json!({"refs": ["..\\a.png"], "segments": [{}]})).is_err());
        let sheet = json!({"v": 1, "cast": [{"name": "甲"}], "anything": {"kept": true}});
        let c = normalize(&json!({"sheet": sheet, "segments": [{}]})).unwrap();
        assert_eq!(c["sheet"], sheet);
        let big = json!({"pad": "x".repeat(210 * 1024)});
        assert!(normalize(&json!({"sheet": big, "segments": [{}]})).is_err());
        assert!(normalize(&json!({"sheet": "不是物件", "segments": [{}]})).unwrap()["sheet"].is_null());
    }

    #[test]
    fn graph_first_segment() {
        let c = chain(json!([{"text": "she smiles"}, {"text": "she waves"}]));
        let g = seg_graph(&c, 0, 7).unwrap();
        assert_eq!(classes(&g, "H3ContinuousStartV14").len(), 1);
        assert!(classes(&g, "H3ContinuousLoadLatent").is_empty());
        let start = &classes(&g, "H3ContinuousStartV14")[0]["inputs"];
        assert!(start["first_frame"].is_array() && start["qwen_reference_1"].is_array() && start.get("last_frame").is_none());
        assert_eq!(start["width"], h3::canvas("9:16", "std", None).0);
        let saves: Vec<String> = classes(&g, "SaveVideo").iter().map(|n| n["inputs"]["filename_prefix"].as_str().unwrap().to_string()).collect();
        assert!(saves.contains(&"h3chain/abc/seg01_cut".to_string()) && saves.contains(&"h3chain/abc/seg01_end".to_string()), "{saves:?}");
        let lat = &classes(&g, "H3ContinuousSaveLatent")[0]["inputs"];
        assert_eq!(lat["clip_index"], 1);
        assert_eq!(lat["filename_prefix"], "h3chain/abc/lat");
        assert!(lat.get("head_context_frames").is_none());
        assert_eq!(classes(&g, "RandomNoise")[0]["inputs"]["noise_seed"], 7);
        // 沒有首幀、參考圖：就不接
        let mut bare = c.clone();
        bare["first"] = Value::Null;
        bare["ref"] = Value::Null;
        let s2 = seg_graph(&bare, 0, 7).unwrap();
        let st = &classes(&s2, "H3ContinuousStartV14")[0]["inputs"];
        assert!(st.get("first_frame").is_none() && st.get("qwen_reference_1").is_none());
    }

    #[test]
    fn cloud_chain_uses_selected_encoder_and_turbo() {
        let c = normalize(&json!({"id":"abc", "where":"runpod", "te":"custom.safetensors", "segments":[{}]})).unwrap();
        let opts = GraphOpts { model: h3::OFFICIAL_REF2VA.into(), te: tx(&c["te"]), turbo_lora: true, ..Default::default() };
        let graph = seg_graph_with(&c, 0, 1, &opts).unwrap();
        assert_eq!(classes(&graph, "CLIPLoader")[0]["inputs"]["clip_name"], "custom.safetensors");
        let guider = &classes(&graph, "BasicGuider")[0]["inputs"];
        let lora = &graph[guider["model"][0].as_str().unwrap()]["inputs"];
        assert_eq!(lora["lora_name"], h3::TURBO_REF2VA);
        assert_eq!(graph[lora["model"][0].as_str().unwrap()]["inputs"]["unet_name"], h3::OFFICIAL_REF2VA);
    }

    /// 本機拍：where=local 留得住、估時用本機的速度；節點圖換成本機的模型（GGUF 用 GGUF 載入器）、小顯存切塊接在取樣前
    #[test]
    fn local_chain_uses_local_model_and_chunks() {
        let mut v = json!({"id": "abc", "where": "local", "segments": [{"text": "a", "secs": 10}]});
        let c = normalize(&v).unwrap();
        assert!(on_local(&c) && !on_runninghub(&c));
        assert_eq!(est_secs(&c), 10.0 * LOCAL_SECS_PER_SEC);
        v["where"] = json!("somewhere");
        assert_eq!(normalize(&v).unwrap()["where"], "runpod");
        let o = GraphOpts { model: "10Eros_w4a8.safetensors".into(), chunks: Some(8), ..Default::default() };
        let g = seg_graph_with(&c, 0, 1, &o).unwrap();
        assert_eq!(classes(&g, "UNETLoader")[0]["inputs"]["unet_name"], "10Eros_w4a8.safetensors");
        let chunk = g.iter().find(|(_, n)| n["class_type"] == "MiniMaxChunkFeedForward").map(|(k, _)| k.clone()).unwrap();
        assert_eq!(classes(&g, "BasicGuider")[0]["inputs"]["model"][0], chunk);
        assert_eq!(classes(&g, "BasicScheduler")[0]["inputs"]["model"][0], chunk);
        let g = seg_graph_with(&c, 0, 1, &GraphOpts { model: "h3-Q3.gguf".into(), ..Default::default() }).unwrap();
        assert_eq!(classes(&g, "UnetLoaderGGUF")[0]["inputs"]["unet_name"], "h3-Q3.gguf");
        assert!(classes(&g, "UNETLoader").is_empty() && classes(&g, "MiniMaxChunkFeedForward").is_empty());
    }

    /// 參考圖也送進 DiT：只有這段沒有首幀／尾幀佔掉 <Picture> 編號、那台 ComfyUI 有 REF_NODE 時才接；
    /// 同一批圖同時接 qwen_reference_N 和 image_N，提示詞開頭交代它們是參考圖
    #[test]
    fn dit_refs_only_where_picture_numbers_line_up() {
        let on = GraphOpts { ref_node: true, ..Default::default() };
        let c = chain(json!([{"text": "a"}, {"text": "b"}, {"text": "c", "last": "end_3.png"}]));
        assert!(c["ref_dit"] == true);
        // 第 1 段有首幀：不送；第 3 段有尾幀：不送
        assert!(dit_refs(&c, 0).is_empty() && dit_refs(&c, 2).is_empty());
        for i in [0, 2] {
            assert!(classes(&seg_graph_with(&c, i, 1, &on).unwrap(), REF_NODE).is_empty(), "第 {i} 段");
        }
        // 第 2 段：角色參考圖進 DiT
        assert_eq!(dit_refs(&c, 1), ["ref_1.png"]);
        let g = seg_graph_with(&c, 1, 1, &on).unwrap();
        let node = &classes(&g, REF_NODE)[0]["inputs"];
        let cont = &classes(&g, "H3ContinuousContinueV14")[0]["inputs"];
        assert_eq!(node["image_1"], cont["qwen_reference_1"]);
        assert_eq!(g[node["image_1"][0].as_str().unwrap()]["inputs"]["image"], "ref_1.png");
        assert_eq!(g[node["positive"][0].as_str().unwrap()]["class_type"], "H3ContinuousContinueV14");
        assert!(cont["prompt"].as_str().unwrap().starts_with("<Picture 1> is a reference image, not a frame of the video"));
        assert_eq!(g[classes(&g, "BasicGuider")[0]["inputs"]["conditioning"][0].as_str().unwrap()]["class_type"], REF_NODE);
        // 那台沒有節點＝跟舊的一模一樣；長片關掉 ref_dit 也是
        assert_eq!(seg_graph_with(&c, 1, 1, &GraphOpts::default()).unwrap(), seg_graph(&c, 1, 1).unwrap());
        let mut off = c.clone();
        off["ref_dit"] = json!(false);
        assert_eq!(seg_graph_with(&off, 1, 1, &on).unwrap(), seg_graph(&c, 1, 1).unwrap());
        assert!(!seg_prompt(&c, 1).contains("<Picture"));
    }

    /// 沒放參考圖的長片：接續段拿第 1 段首幀當參考；這段自己關掉全部參考圖（refs: []）就不送；多張時說明寫範圍
    #[test]
    fn dit_refs_fall_back_to_first_frame() {
        let c = normalize(&json!({"id": "abc", "first": "first_1.png", "segments": [{"text": "a"}, {"text": "b"}, {"text": "c", "refs": []}]})).unwrap();
        assert!(dit_refs(&c, 0).is_empty());
        assert_eq!(dit_refs(&c, 1), ["first_1.png"]);
        assert!(dit_refs(&c, 2).is_empty());
        // 首幀只是順便當參考：不算這段「一定要有」的素材（檔不見了不擋開拍）
        assert!(inputs_of(&c, 1).is_empty());
        let g = seg_graph_with(&c, 1, 1, &GraphOpts { ref_node: true, ..Default::default() }).unwrap();
        assert_eq!(g[classes(&g, REF_NODE)[0]["inputs"]["image_1"][0].as_str().unwrap()]["inputs"]["image"], "first_1.png");
        let many = normalize(&json!({"id": "abc", "refs": ["a.png", "b.png"], "segments": [{"text": "a"}]})).unwrap();
        assert_eq!(dit_refs(&many, 0), ["a.png", "b.png"]);
        let g = seg_graph_with(&many, 0, 1, &GraphOpts { ref_node: true, ..Default::default() }).unwrap();
        let node = &classes(&g, REF_NODE)[0]["inputs"];
        assert!(node["image_2"].is_array() && node.get("image_3").is_none());
        assert!(classes(&g, "H3ContinuousStartV14")[0]["inputs"]["prompt"].as_str().unwrap().starts_with("<Picture 1> to <Picture 2> are reference images"));
    }

    /// 本機拍：上一段的 latent 換成呼叫端給的（不重複的）檔名；沒給就是固定檔名
    #[test]
    fn local_chain_loads_latent_from_given_path() {
        let c = chain(json!([{"text": "a"}, {"text": "b"}]));
        let o = GraphOpts { latent_in: Some("h3chain/abc/prev_00001_17_99.safetensors".into()), ..Default::default() };
        let g = seg_graph_with(&c, 1, 1, &o).unwrap();
        assert_eq!(classes(&g, "H3ContinuousLoadLatent")[0]["inputs"]["latent_path"], "h3chain/abc/prev_00001_17_99.safetensors");
        // 存檔的那顆不變：這段自己的 latent 照舊存固定檔名
        assert_eq!(classes(&g, "H3ContinuousSaveLatent"), classes(&seg_graph(&c, 1, 1).unwrap(), "H3ContinuousSaveLatent"));
        assert!(classes(&seg_graph(&c, 1, 1).unwrap(), "H3ContinuousLoadLatent")[0]["inputs"]["latent_path"].as_str().unwrap().ends_with("/lat_00001.safetensors"));
    }

    /// RefMod 鎖臉和參考圖一起開：RefMod 吃的是加過參考圖的條件
    #[test]
    fn refmod_applies_after_dit_refs() {
        let sheet = json!({"refmod": true, "cast": [{"name": "甲", "locked": true, "refmod": "paintdeck/pd_char_7"}]});
        let c = normalize(&json!({"id": "abc", "sheet": sheet, "refs": ["a.png"], "segments": [{"text": "a"}]})).unwrap();
        let g = seg_graph_with(&c, 0, 1, &GraphOpts { ref_node: true, ..Default::default() }).unwrap();
        let apply = &classes(&g, "MiniMaxH3RefModApply")[0];
        assert_eq!(g[apply["inputs"]["conditioning"][0].as_str().unwrap()]["class_type"], REF_NODE);
    }

    #[test]
    fn graph_continue_segment() {
        let c = chain(json!([{"text": "a"}, {"text": "b"}, {"text": "c", "last": "end_3.png"}]));
        let g = seg_graph(&c, 2, 1).unwrap();
        let load = &classes(&g, "H3ContinuousLoadLatent")[0]["inputs"];
        assert_eq!(load["latent_path"], "h3chain/abc/lat_00002.safetensors");
        assert_eq!(load["clip_index"], 0);
        let cont = &classes(&g, "H3ContinuousContinueV14")[0]["inputs"];
        assert_eq!(cont["masked_context_frames"], "39");
        assert_eq!(cont["duration_mode"], "Net New Content");
        assert_eq!(cont["audio_tail_carryover"], "Full Previous Tail");
        assert!(cont["last_frame"].is_array());
        assert!(classes(&g, "H3ContinuousStartV14").is_empty());
        let lat = &classes(&g, "H3ContinuousSaveLatent")[0]["inputs"];
        assert_eq!(lat["clip_index"], 3);
        assert!(lat["head_context_frames"].is_array());
        let modes: Vec<Value> = classes(&g, "H3ContinuousStitchOutputV14").iter().map(|n| n["inputs"]["output_mode"].clone()).collect();
        assert!(modes.contains(&json!("Stitch Ready")) && modes.contains(&json!("Final Clip")));
        // 第 2 段沒釘尾幀
        let g1 = seg_graph(&c, 1, 1).unwrap();
        assert!(classes(&g1, "H3ContinuousContinueV14")[0]["inputs"].get("last_frame").is_none());
        assert_eq!(latent_name(2), "lat_00002.safetensors");
    }

    #[test]
    fn prompt_voice_before_dialogue() {
        let c = chain(json!([{"text": "She waves.", "lines": [{"at": 2, "lang": "Chinese", "text": "你好"}]}]));
        let p = seg_prompt(&c, 0);
        assert!(p.contains("At 2.0 seconds she says to the viewer in a soft voice: <d>[Chinese] 你好</d>"), "{p}");
        assert!(p.starts_with("Xiaoke has long orange hair. A small bedroom at night. 2.5D anime. She waves."), "{p}");
        assert!(!p.contains(". .") && !p.contains("..") && !p.ends_with(' '), "{p}");
    }

    #[test]
    fn normalize_clamps() {
        let c = chain(json!([{"secs": 30, "text": "a", "lines": [{"at": 99, "text": "x"}]}, {"secs": 3, "text": "b"}]));
        assert_eq!(c["segments"][0]["secs"], 15.0);
        assert_eq!(c["segments"][0]["lines"][0]["at"], 15.0);
        assert_eq!(c["segments"][1]["secs"], 5.0);
        assert_eq!(c["segments"][1]["status"], "todo");
        let many: Vec<Value> = (0..40).map(|_| json!({"text": "x"})).collect();
        assert_eq!(chain(json!(many))["segments"].as_array().unwrap().len(), MAX_SEGS);
        assert!(normalize(&json!({"ref": "../x.png", "segments": [{}]})).is_err());
        assert!(normalize(&json!({"segments": [{"last": "a/b.png"}]})).is_err());
        assert_eq!(normalize(&json!({"segments": []})).unwrap()["segments"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn edit_blocked_after_done() {
        let mut old = chain(json!([{"text": "a"}, {"text": "b"}]));
        let mut new = old.clone();
        new["aspect"] = json!("16:9");
        assert!(check_edit(&old, &new).is_ok(), "還沒拍：可以改");
        old["segments"][0]["status"] = json!("done");
        assert!(check_edit(&old, &new).unwrap_err().contains("另存新的長片"));
        let mut same = old.clone();
        same["segments"][1]["text"] = json!("changed");
        assert!(check_edit(&old, &same).is_ok(), "只改文字：可以");
    }

    #[test]
    fn pending_review_and_recorded_dimensions_lock_shape_without_completed_shots() {
        for state in [json!({"status":"review", "pending_take":"candidate"}),
            json!({"status":"todo", "takes":[{"id":"old"}]}), json!({"status":"todo", "selected_take":"old"})] {
            let mut old = chain(json!([{"text":"a"}]));
            old["segments"][0] = state;
            let mut changed = old.clone(); changed["aspect"] = json!("1:1");
            assert!(size_locked(&old));
            assert!(check_edit(&old, &changed).unwrap_err().contains("另存新的長片"));
            assert!(check_edit(&old, &old).is_ok());
        }
        let mut old = chain(json!([{"text":"a", "status":"error"}]));
        old["size"] = json!([640, 960]);
        let mut changed = old.clone(); changed["quality"] = json!("high");
        assert!(check_edit(&old, &changed).is_err(), "failed generation still owns its recorded canvas");
        assert_eq!(old["size"], json!([640, 960]));
    }

    #[test]
    fn reset_from_clears_later() {
        let mut c = chain(json!([{"text": "a"}, {"text": "b"}, {"text": "c"}]));
        for s in c["segments"].as_array_mut().unwrap() {
            s["status"] = json!("done");
            s["files"] = json!({"cut": "x"});
            s["took"] = json!(200);
        }
        c["final"] = json!({"id": 5});
        reset_from(&mut c, 1);
        assert_eq!(c["segments"][0]["status"], "done");
        assert_eq!(c["segments"][1]["status"], "todo");
        assert_eq!(c["segments"][2]["status"], "todo");
        assert!(c["segments"][2]["files"].is_null() && c["segments"][2]["took"].is_null());
        assert!(c["final"].is_null());
        assert_eq!(est_secs(&c), 2.0 * 10.0 * SECS_PER_SEC);
    }
}
