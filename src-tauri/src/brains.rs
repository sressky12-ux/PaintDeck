//! API 總機（2026-11-02，設計：design\API總機與指令預設_設計.md）
//!
//! - 大模型連線（多組，OpenAI 相容）：`<資料>\llm_profiles.json`，金鑰用 DPAPI 加密成 key_dpapi。
//!   第一次開站會把第二百零六筆存在 cloud.json 的單組 api_*（助手的「API」腦袋）搬成 id "default" 那組。

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 服務商預設：(id, 顯示名, 網址)。模型名稱不寫死，用「抓模型清單」或使用者自己填。
/// id 沿用第二百零六筆網頁的 deepseek／glm／zai／custom
pub const PRESETS: &[(&str, &str, &str)] = &[
    ("deepseek", "DeepSeek", "https://api.deepseek.com/v1"),
    ("glm", "智譜 GLM", "https://open.bigmodel.cn/api/paas/v4"),
    ("zai", "Z.ai", "https://api.z.ai/api/paas/v4"),
    ("openai", "OpenAI", "https://api.openai.com/v1"),
    ("anthropic", "Anthropic（Claude）", "https://api.anthropic.com/v1"),
    ("gemini", "Google Gemini", "https://generativelanguage.googleapis.com/v1beta/openai"),
    ("kimi", "Kimi（月之暗面）", "https://api.moonshot.cn/v1"),
    ("qwen", "通義千問（百煉）", "https://dashscope.aliyuncs.com/compatible-mode/v1"),
    ("openrouter", "OpenRouter", "https://openrouter.ai/api/v1"),
    ("custom", "自訂", ""),
];

pub const DEFAULT_ID: &str = "default";
const PROFILES_FILE: &str = "llm_profiles.json";

#[derive(Clone, Debug, PartialEq)]
pub struct VisionSeen {
    pub r#for: String,
    pub ok: bool,
}

#[derive(Clone, Debug)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub url: String,
    pub model: String,
    pub key: Option<String>,
    pub vision: Option<VisionSeen>,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

#[cfg(windows)]
fn seal(s: &str) -> Result<String, String> {
    crate::settings::protect(s).map_err(|e| format!("加密金鑰失敗：{e}"))
}
#[cfg(not(windows))]
fn seal(s: &str) -> Result<String, String> {
    Ok(s.to_string())
}
#[cfg(windows)]
fn unseal(s: &str) -> Option<String> {
    crate::settings::unprotect(s).ok()
}
#[cfg(not(windows))]
fn unseal(s: &str) -> Option<String> {
    Some(s.to_string())
}

impl Profile {
    /// 看圖結果只對「網址｜模型」算數
    pub fn vision_for(&self) -> String {
        format!("{}|{}", self.url, self.model)
    }
    pub fn vision_known(&self) -> Option<bool> {
        self.vision.as_ref().filter(|v| v.r#for == self.vision_for()).map(|v| v.ok)
    }
    /// 網址和模型都填了才算能用（金鑰可以空著：自己架的 LM Studio 不用）
    pub fn ready(&self) -> bool {
        !self.url.is_empty() && !self.model.is_empty()
    }
    pub fn remote(&self) -> crate::pe::Remote {
        crate::pe::Remote { base: self.url.clone(), key: self.key.clone(), model: self.model.clone(), api: true }
    }
    /// 給網頁看：金鑰只說有沒有填
    pub fn view(&self) -> Value {
        json!({"id": self.id, "name": self.name, "provider": self.provider, "url": self.url, "model": self.model,
               "has_key": self.key.is_some(), "vision": self.vision_known(), "ready": self.ready()})
    }
    fn to_disk(&self) -> Result<Value, String> {
        let mut m = Map::new();
        for (k, v) in [("id", &self.id), ("name", &self.name), ("provider", &self.provider), ("url", &self.url), ("model", &self.model)] {
            m.insert(k.into(), json!(v));
        }
        if let Some(k) = &self.key {
            m.insert("key_dpapi".into(), json!(seal(k)?));
        }
        if let Some(v) = &self.vision {
            m.insert("vision".into(), json!({"for": v.r#for, "ok": v.ok}));
        }
        Ok(Value::Object(m))
    }
    fn from_disk(v: &Value) -> Option<Profile> {
        let id = s(v, "id");
        if id.is_empty() {
            return None;
        }
        let key = v.get("key_dpapi").and_then(Value::as_str).and_then(unseal).filter(|k| !k.is_empty());
        Some(Profile {
            name: Some(s(v, "name")).filter(|n| !n.is_empty()).unwrap_or_else(|| id.clone()),
            id,
            provider: s(v, "provider"),
            url: s(v, "url"),
            model: s(v, "model"),
            key,
            vision: vision_of(v.get("vision")),
        })
    }
}

fn vision_of(v: Option<&Value>) -> Option<VisionSeen> {
    let v = v?;
    Some(VisionSeen { r#for: s(v, "for"), ok: v.get("ok").and_then(Value::as_bool)? })
}

/// 第二百零六筆存在 cloud.json 的單組設定 → 「預設」連線（沒填網址就不搬）
fn from_cloud(c: &Map<String, Value>) -> Option<Profile> {
    let g = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let url = g("api_url");
    if url.is_empty() {
        return None;
    }
    let provider = g("api_provider");
    let name = PRESETS.iter().find(|p| p.0 == provider).map(|p| p.1.to_string()).unwrap_or_else(|| "預設".into());
    Some(Profile {
        id: DEFAULT_ID.into(),
        name,
        provider,
        url,
        model: g("api_model"),
        key: Some(g("api_key")).filter(|k| !k.is_empty()),
        vision: vision_of(c.get("api_vision")),
    })
}

pub struct Profiles {
    dir: PathBuf,
    list: Mutex<Vec<Profile>>,
}

impl Profiles {
    /// 讀 llm_profiles.json；沒有這個檔就從 cloud.json 的舊設定搬（搬完立刻存檔，之後不再搬）
    pub fn load(dir: &Path, cloud_cfg: &Map<String, Value>) -> Profiles {
        let file = dir.join(PROFILES_FILE);
        let me = Profiles { dir: dir.to_path_buf(), list: Mutex::new(vec![]) };
        match std::fs::read(&file).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            Some(v) => {
                let list = v["profiles"].as_array().into_iter().flatten().filter_map(Profile::from_disk).collect();
                *me.lock() = list;
            }
            None => {
                if let Some(p) = from_cloud(cloud_cfg) {
                    *me.lock() = vec![p];
                    if let Err(e) = me.save() {
                        eprintln!("搬外接 API 設定失敗：{e}");
                    }
                }
            }
        }
        me
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Profile>> {
        self.list.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self) -> Result<(), String> {
        let list = self.lock().iter().map(Profile::to_disk).collect::<Result<Vec<_>, _>>()?;
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("建立 {} 失敗：{e}", self.dir.display()))?;
        crate::director::write_atomic(&self.dir.join(PROFILES_FILE), &json!({"profiles": list})).map_err(|e| format!("存連線設定失敗：{e}"))
    }

    pub fn list_view(&self) -> Value {
        Value::Array(self.lock().iter().map(Profile::view).collect())
    }

    pub fn get(&self, id: &str) -> Option<Profile> {
        self.lock().iter().find(|p| p.id == id).cloned()
    }

    /// 「預設」那組（助手舊的「API」腦袋、/api/agent/llm_api 用）；沒有就第一組
    pub fn default_id(&self) -> Option<String> {
        let l = self.lock();
        l.iter().find(|p| p.id == DEFAULT_ID).or_else(|| l.first()).map(|p| p.id.clone())
    }

    /// 新增或修改：{id?, name, provider, url, model, key?}。key 沒帶＝不動，空字串＝清掉。沒給 id＝新增
    pub fn upsert(&self, v: &Value) -> Result<Profile, String> {
        let url = s(v, "url").trim_end_matches('/').to_string();
        if !url.is_empty() && !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("網址要用 http:// 或 https:// 開頭".into());
        }
        let key = match v.get("key") {
            None => None,
            Some(k) => {
                let k = k.as_str().unwrap_or("").trim().to_string();
                if k.chars().any(char::is_whitespace) {
                    return Err("金鑰裡不能有空白：重新複製一次".into());
                }
                Some(k)
            }
        };
        let mut id = s(v, "id");
        let mut l = self.lock();
        let p = match l.iter_mut().find(|p| p.id == id && !id.is_empty()) {
            Some(p) => p,
            None => {
                if id.is_empty() {
                    id = crate::director::new_id();
                }
                l.push(Profile { id: id.clone(), name: String::new(), provider: String::new(), url: String::new(), model: String::new(), key: None, vision: None });
                l.last_mut().expect("剛加的")
            }
        };
        let name = s(v, "name").chars().take(40).collect::<String>();
        p.name = if name.is_empty() { if p.name.is_empty() { id.clone() } else { p.name.clone() } } else { name };
        p.provider = s(v, "provider").chars().take(20).collect();
        p.url = url;
        p.model = s(v, "model").chars().take(200).collect();
        if let Some(k) = key {
            p.key = Some(k).filter(|k| !k.is_empty());
        }
        let out = p.clone();
        drop(l);
        self.save()?;
        Ok(out)
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let n = {
            let mut l = self.lock();
            let n = l.len();
            l.retain(|p| p.id != id);
            n - l.len()
        };
        if n == 0 {
            return Err("找不到這組連線".into());
        }
        self.save()
    }

    pub fn set_vision(&self, id: &str, ok: bool) -> Result<(), String> {
        {
            let mut l = self.lock();
            let p = l.iter_mut().find(|p| p.id == id).ok_or("找不到這組連線")?;
            p.vision = Some(VisionSeen { r#for: p.vision_for(), ok });
        }
        self.save()
    }
}

// ---------------------------------------------------------------- 指令預設（學 SillyTavern 的對話預設）

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Block {
    pub id: String,
    pub name: String,
    /// system／user／assistant
    pub role: String,
    pub content: String,
    pub enabled: bool,
    /// text＝自己的段落；builtin＝功能的內建指令；request＝功能本來要送的對話（兩個佔位段不能刪、不能關）
    pub kind: String,
}

impl Default for Block {
    fn default() -> Self {
        Block { id: String::new(), name: String::new(), role: "system".into(), content: String::new(), enabled: true, kind: "text".into() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Params {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<i64>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub max_tokens: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub blocks: Vec<Block>,
    pub params: Params,
    /// 預填：接在最後當 assistant 訊息，讓模型接著寫
    pub prefill: String,
    /// 相鄰的 system 段併成一則（有的服務只收一則 system）
    pub squash_system: bool,
    /// {{user}}、{{char}} 換成什麼
    pub user_name: String,
    pub char_name: String,
}

impl Default for Preset {
    fn default() -> Self {
        Preset::new_default("")
    }
}

pub fn marker(kind: &str) -> Block {
    let name = if kind == "builtin" { "功能內建指令" } else { "功能的請求內容" };
    Block { id: kind.into(), name: name.into(), role: if kind == "builtin" { "system" } else { "user" }.into(), kind: kind.into(), ..Block::default() }
}

impl Preset {
    pub fn new_default(name: &str) -> Preset {
        Preset {
            id: String::new(),
            name: name.into(),
            blocks: vec![marker("builtin"), marker("request")],
            params: Params::default(),
            prefill: String::new(),
            squash_system: false,
            user_name: "User".into(),
            char_name: "Assistant".into(),
        }
    }
}

/// 組訊息。沒套預設＝現在的行為：[system 內建指令（空的就不放）] + 原本的請求
pub fn assemble(p: Option<&Preset>, builtin_system: &str, request: &[Value]) -> Vec<Value> {
    let builtin = || (!builtin_system.trim().is_empty()).then(|| json!({"role": "system", "content": builtin_system}));
    let Some(p) = p else {
        return builtin().into_iter().chain(request.iter().cloned()).collect();
    };
    let mac = |t: &str| t.replace("{{user}}", &p.user_name).replace("{{char}}", &p.char_name);
    let has = |k: &str| p.blocks.iter().any(|b| b.kind == k);
    let mut out: Vec<Value> = vec![];
    // 佔位段被刪掉了也要送：內建放最前、請求放最後
    if !has("builtin") {
        out.extend(builtin());
    }
    for b in &p.blocks {
        match b.kind.as_str() {
            "builtin" => out.extend(builtin()),
            "request" => out.extend(request.iter().cloned()),
            _ if !b.enabled || b.content.trim().is_empty() => {}
            _ => {
                let role = if matches!(b.role.as_str(), "user" | "assistant") { b.role.as_str() } else { "system" };
                out.push(json!({"role": role, "content": mac(&b.content)}));
            }
        }
    }
    if !has("request") {
        out.extend(request.iter().cloned());
    }
    if p.squash_system {
        let mut merged: Vec<Value> = vec![];
        for m in out {
            let prev_sys = merged.last().is_some_and(|l: &Value| l["role"] == "system" && l["content"].is_string());
            if prev_sys && m["role"] == "system" && m["content"].is_string() {
                let l = merged.last_mut().expect("上面確認過");
                l["content"] = json!(format!("{}\n\n{}", l["content"].as_str().unwrap_or(""), m["content"].as_str().unwrap_or("")));
            } else {
                merged.push(m);
            }
        }
        out = merged;
    }
    if !p.prefill.trim().is_empty() {
        out.push(json!({"role": "assistant", "content": mac(&p.prefill)}));
    }
    out
}

/// 已經組好的訊息（第一則 system＝功能的內建指令，其餘＝請求）套上預設；沒預設原樣回傳
pub fn apply(p: Option<&Preset>, messages: &Value) -> Value {
    let Some(p) = p else { return messages.clone() };
    let all = messages.as_array().cloned().unwrap_or_default();
    let (sys, rest) = match all.first() {
        Some(m) if m["role"] == "system" && m["content"].is_string() => (m["content"].as_str().unwrap_or("").to_string(), &all[1..]),
        _ => (String::new(), &all[..]),
    };
    Value::Array(assemble(Some(p), &sys, rest))
}

/// 預設有填的取樣參數蓋過功能原本的值。api＝外接 API：只放各家都認得的欄位（top_k 是 llama.cpp 的）
pub fn apply_params(body: &mut Value, p: &Params, api: bool) {
    let Some(m) = body.as_object_mut() else { return };
    let mut put = |k: &str, v: Value| {
        m.insert(k.into(), v);
    };
    if let Some(t) = p.temperature {
        put("temperature", json!(if api { t.clamp(0.0, 1.0) } else { t }));
    }
    if let Some(x) = p.top_p {
        put("top_p", json!(x));
    }
    if let Some(x) = p.frequency_penalty {
        put("frequency_penalty", json!(x));
    }
    if let Some(x) = p.presence_penalty {
        put("presence_penalty", json!(x));
    }
    if let Some(x) = p.max_tokens {
        put("max_tokens", json!(x));
    }
    if let (Some(k), false) = (p.top_k, api) {
        put("top_k", json!(k));
    }
}

/// SillyTavern 的佔位段：在繪匣沒有對應的內容（角色卡、世界書、範例對話），匯入時略過
const ST_SKIP: [&str; 7] = ["dialogueExamples", "worldInfoBefore", "worldInfoAfter", "charDescription", "charPersonality", "scenario", "personaDescription"];
/// 有對應、會匯入的 SillyTavern 欄位（其他欄位列進報告）
const ST_PARAMS: [&str; 6] = ["temperature", "top_p", "top_k", "frequency_penalty", "presence_penalty", "openai_max_tokens"];
const ST_KEEP: [&str; 4] = ["prompts", "prompt_order", "assistant_prefill", "squash_system_messages"];

/// SillyTavern Chat Completion 預設 JSON → 指令預設＋匯入報告（一行一件略過或改動的事）
pub fn import_st(v: &Value, file_name: &str) -> Result<(Preset, Vec<String>), String> {
    let prompts = v.get("prompts").and_then(Value::as_array).ok_or("這不是 SillyTavern 的對話預設：找不到 prompts")?;
    let mut report = vec![];
    let by_id = |id: &str| prompts.iter().find(|p| s(p, "identifier") == id);
    // 順序：character_id 100001（SillyTavern 全域那組）→ 100000 → 第一組；都沒有就照 prompts 原順序
    let orders = v.get("prompt_order").and_then(Value::as_array).cloned().unwrap_or_default();
    let pick = orders
        .iter()
        .find(|o| o["character_id"] == 100001)
        .or_else(|| orders.iter().find(|o| o["character_id"] == 100000))
        .or_else(|| orders.first())
        .and_then(|o| o["order"].as_array().cloned());
    let order: Vec<(String, bool)> = match pick {
        Some(o) => o.iter().map(|e| (s(e, "identifier"), e.get("enabled").and_then(Value::as_bool).unwrap_or(true))).collect(),
        None => {
            report.push("沒有 prompt_order：照 prompts 原本的順序匯入、全部開著".into());
            prompts.iter().map(|p| (s(p, "identifier"), true)).collect()
        }
    };
    let mut blocks: Vec<Block> = vec![];
    let mut tail: Vec<Block> = vec![]; // 照深度插入的段：放到對話後面
    let mut skipped = vec![];
    for (id, enabled) in &order {
        if id == "chatHistory" {
            blocks.push(marker("request"));
            continue;
        }
        if ST_SKIP.contains(&id.as_str()) {
            skipped.push(id.clone());
            continue;
        }
        let Some(p) = by_id(id) else {
            report.push(format!("順序裡的 {id} 在 prompts 找不到，略過"));
            continue;
        };
        if p.get("marker").and_then(Value::as_bool) == Some(true) {
            skipped.push(id.clone());
            continue;
        }
        let role = match s(p, "role").as_str() {
            r @ ("user" | "assistant") => r.to_string(),
            _ => "system".into(),
        };
        let name = Some(s(p, "name")).filter(|n| !n.is_empty()).unwrap_or_else(|| id.clone());
        let b = Block { id: id.clone(), name: name.clone(), role, content: p.get("content").and_then(Value::as_str).unwrap_or("").to_string(), enabled: *enabled, kind: "text".into() };
        if p.get("injection_position").and_then(Value::as_i64) == Some(1) {
            let depth = p.get("injection_depth").and_then(Value::as_i64).unwrap_or(0);
            report.push(format!("「{name}」原本是插在對話第 {depth} 層，這裡改放在對話後面"));
            tail.push(b);
        } else {
            blocks.push(b);
        }
        if id == "main" {
            blocks.push(marker("builtin"));
        }
    }
    if !blocks.iter().any(|b| b.kind == "builtin") {
        blocks.insert(0, marker("builtin"));
        report.push("沒有 main（主要指令）：功能內建指令放在最前面".into());
    }
    match blocks.iter().position(|b| b.kind == "request") {
        Some(i) => {
            let rest = blocks.split_off(i + 1);
            blocks.extend(tail);
            blocks.extend(rest);
        }
        None => {
            report.push("沒有 chatHistory：功能的請求內容放在最後".into());
            blocks.extend(tail);
            blocks.push(marker("request"));
        }
    }
    if !skipped.is_empty() {
        report.push(format!("略過（繪匣沒有角色卡、世界書、範例對話）：{}", skipped.join("、")));
    }
    let f = |k: &str| v.get(k).and_then(Value::as_f64);
    let params = Params {
        temperature: f("temperature"),
        top_p: f("top_p"),
        top_k: v.get("top_k").and_then(Value::as_i64).filter(|k| *k > 0),
        frequency_penalty: f("frequency_penalty"),
        presence_penalty: f("presence_penalty"),
        max_tokens: v.get("openai_max_tokens").and_then(Value::as_i64).filter(|n| *n > 0),
    };
    let ignored: Vec<&str> = v
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, _)| k.as_str())
        .filter(|k| !ST_PARAMS.contains(k) && !ST_KEEP.contains(k))
        .collect();
    if !ignored.is_empty() {
        report.push(format!("沒匯入的設定（模型、網址、代理這些在 API 頁的連線裡設）：{}", ignored.join("、")));
    }
    let name = Path::new(file_name).file_stem().and_then(|n| n.to_str()).unwrap_or("匯入的預設").to_string();
    let mut p = Preset::new_default(&name);
    p.blocks = blocks;
    p.params = params;
    p.prefill = v.get("assistant_prefill").and_then(Value::as_str).unwrap_or("").to_string();
    p.squash_system = v.get("squash_system_messages").and_then(Value::as_bool).unwrap_or(false);
    Ok((p, report))
}

// ---------------------------------------------------------------- 對照表：哪個功能用哪顆腦袋、套哪組預設

/// (功能 id, 中文名)
pub const FEATURES: [(&str, &str); 6] = [
    ("agent", "生圖助手"),
    ("rewrite", "AI 改寫"),
    ("translate", "中英對照"),
    ("shotsheet", "拍攝單"),
    ("h3writer", "H3 寫手"),
    ("director", "導演台"),
];

const PRESETS_FILE: &str = "prompt_presets.json";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Route {
    /// page＝照頁面自己選（舊行為）／9b／35b／cloud／api:<連線 id>
    pub brain: String,
    pub preset: Option<String>,
}

impl Default for Route {
    fn default() -> Self {
        Route { brain: "page".into(), preset: None }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct PresetFile {
    presets: Vec<Preset>,
    routes: std::collections::BTreeMap<String, Route>,
}

/// 這次要用的腦袋（不給 Debug：裡面有金鑰）
pub enum Brain {
    /// 本機 llama-server：9b／35b／auto
    Local(String),
    /// 雲端（RunPod／SSH／網址）：呼叫端照舊用 app.agent.cloud 解
    Cloud,
    Api { remote: crate::pe::Remote, profile_id: String, vision: Option<bool> },
}

impl Brain {
    pub fn is_api(&self) -> bool {
        matches!(self, Brain::Api { .. })
    }
}

pub struct Brains {
    pub profiles: Profiles,
    dir: PathBuf,
    file: Mutex<PresetFile>,
}

pub const NO_API: &str = "外接 API 還沒設定：到左邊「API」頁填網址和模型";

impl Brains {
    /// dir＝資料夾（data）；cloud_cfg＝cloud.json 的設定（搬舊的單組外接 API 用）
    pub fn new(dir: &Path, cloud_cfg: &Map<String, Value>) -> Brains {
        let file = std::fs::read(dir.join(PRESETS_FILE)).ok().and_then(|b| serde_json::from_slice::<PresetFile>(&b).ok()).unwrap_or_default();
        Brains { profiles: Profiles::load(dir, cloud_cfg), dir: dir.to_path_buf(), file: Mutex::new(file) }
    }

    fn f(&self) -> std::sync::MutexGuard<'_, PresetFile> {
        self.file.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, f: &PresetFile) -> Result<(), String> {
        let v = serde_json::to_value(f).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("建立 {} 失敗：{e}", self.dir.display()))?;
        crate::director::write_atomic(&self.dir.join(PRESETS_FILE), &v).map_err(|e| format!("存指令預設失敗：{e}"))
    }

    pub fn route(&self, feature: &str) -> Route {
        self.f().routes.get(feature).cloned().unwrap_or_default()
    }

    /// 全部功能的對照（沒設定的給預設值）
    pub fn routes_view(&self) -> Value {
        let f = self.f();
        let m: Map<String, Value> = FEATURES
            .iter()
            .map(|(id, _)| (id.to_string(), serde_json::to_value(f.routes.get(*id).cloned().unwrap_or_default()).unwrap_or(Value::Null)))
            .collect();
        Value::Object(m)
    }

    /// {功能: {brain, preset}}，只改有帶的功能
    pub fn routes_save(&self, v: &Value) -> Result<Value, String> {
        let o = v.as_object().ok_or("要給 {功能: {brain, preset}}")?;
        let mut f = self.f().clone();
        for (k, r) in o {
            if !FEATURES.iter().any(|(id, _)| id == k) {
                return Err(format!("沒有這個功能：{k}"));
            }
            let brain = s(r, "brain");
            let ok = matches!(brain.as_str(), "page" | "9b" | "35b" | "cloud") || brain.strip_prefix("api:").is_some_and(|i| !i.is_empty());
            if !ok {
                return Err(format!("腦袋只能選 page／9b／35b／cloud／api:<連線>：{brain}"));
            }
            let preset = Some(s(r, "preset")).filter(|p| !p.is_empty());
            f.routes.insert(k.clone(), Route { brain, preset });
        }
        self.save(&f)?;
        *self.f() = f;
        Ok(self.routes_view())
    }

    pub fn presets_view(&self) -> Value {
        Value::Array(self.f().presets.iter().map(|p| json!({"id": p.id, "name": p.name})).collect())
    }

    pub fn preset(&self, id: &str) -> Option<Preset> {
        self.f().presets.iter().find(|p| p.id == id).cloned()
    }

    /// 存整組（沒 id＝新增）。佔位段少了就補回去（內建放最前、請求放最後）
    pub fn preset_save(&self, v: &Value) -> Result<Preset, String> {
        let mut p: Preset = serde_json::from_value(v.clone()).map_err(|e| format!("預設格式不對：{e}"))?;
        p.name = p.name.trim().chars().take(60).collect();
        if p.name.is_empty() {
            p.name = "未命名預設".into();
        }
        if p.id.is_empty() {
            p.id = crate::director::new_id();
        }
        if !p.blocks.iter().any(|b| b.kind == "builtin") {
            p.blocks.insert(0, marker("builtin"));
        }
        if !p.blocks.iter().any(|b| b.kind == "request") {
            p.blocks.push(marker("request"));
        }
        for b in &mut p.blocks {
            if b.kind != "text" {
                b.enabled = true;
            }
            if b.id.is_empty() {
                b.id = crate::director::new_id();
            }
        }
        let mut f = self.f().clone();
        match f.presets.iter_mut().find(|x| x.id == p.id) {
            Some(x) => *x = p.clone(),
            None => f.presets.push(p.clone()),
        }
        self.save(&f)?;
        *self.f() = f;
        Ok(p)
    }

    pub fn preset_delete(&self, id: &str) -> Result<(), String> {
        let mut f = self.f().clone();
        let n = f.presets.len();
        f.presets.retain(|p| p.id != id);
        if f.presets.len() == n {
            return Err("找不到這組預設".into());
        }
        self.save(&f)?;
        *self.f() = f;
        Ok(())
    }

    /// 頁面送來的選擇 → 腦袋。api＝預設連線、api:<id>＝那組、cloud、其他當本機（9b／35b／auto）
    fn page_brain(&self, choice: &str) -> Result<Brain, String> {
        let pid = match choice {
            "cloud" => return Ok(Brain::Cloud),
            "api" => self.profiles.default_id().ok_or(NO_API)?,
            c => match c.strip_prefix("api:") {
                Some(id) => id.to_string(),
                None => return Ok(Brain::Local(if c.is_empty() || c == "page" { "auto".into() } else { c.to_string() })),
            },
        };
        let p = self.profiles.get(&pid).filter(Profile::ready).ok_or(NO_API)?;
        Ok(Brain::Api { remote: p.remote(), vision: p.vision_known(), profile_id: pid })
    }

    /// 這次用哪顆腦袋、套哪組預設，第三個是給使用者看的提示（對照表指到的東西不見了之類）
    pub fn resolve(&self, feature: &str, page_choice: &str) -> Result<(Brain, Option<Preset>, Option<String>), String> {
        let r = self.route(feature);
        let mut hint = None;
        let brain = if r.brain == "page" {
            self.page_brain(page_choice)?
        } else {
            match self.page_brain(&r.brain) {
                Ok(b) => b,
                Err(_) => {
                    hint = Some("API 頁對照表選的連線不見了或沒填好，這次改用頁面上的選擇".to_string());
                    self.page_brain(page_choice)?
                }
            }
        };
        let preset = match &r.preset {
            None => None,
            Some(id) => {
                let p = self.preset(id);
                if p.is_none() {
                    hint = Some(hint.map_or(String::new(), |h| h + "；") + "API 頁對照表選的指令預設不見了，這次不套預設");
                }
                p
            }
        };
        Ok((brain, preset, hint))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brains(name: &str) -> (Brains, PathBuf) {
        let d = tmp(name);
        (Brains::new(&d, &legacy()), d)
    }

    #[test]
    fn resolve_page_default() {
        let (b, d) = brains("rpd");
        let (br, p, h) = b.resolve("translate", "9b").unwrap();
        assert!(matches!(br, Brain::Local(ref m) if m == "9b"));
        assert!(p.is_none() && h.is_none());
        assert!(matches!(b.resolve("translate", "cloud").unwrap().0, Brain::Cloud));
        assert!(matches!(b.resolve("translate", "api").unwrap().0, Brain::Api { ref profile_id, .. } if profile_id == DEFAULT_ID));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn resolve_route_api() {
        let (b, d) = brains("rra");
        b.routes_save(&json!({"translate": {"brain": "api:default"}})).unwrap();
        let (br, _, h) = b.resolve("translate", "9b").unwrap();
        assert!(matches!(br, Brain::Api { ref remote, .. } if remote.model == "deepseek-flash" && remote.api));
        assert!(h.is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn resolve_missing_profile_falls_back() {
        let (b, d) = brains("rmp");
        b.routes_save(&json!({"translate": {"brain": "api:gone"}})).unwrap();
        let (br, _, h) = b.resolve("translate", "35b").unwrap();
        assert!(matches!(br, Brain::Local(ref m) if m == "35b"));
        assert!(h.unwrap().contains("不見了"));
        // 頁面也選 API、但沒有可用連線：講清楚
        b.profiles.remove(DEFAULT_ID).unwrap();
        assert_eq!(b.resolve("agent", "api").err().unwrap(), NO_API);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn resolve_missing_preset_is_none() {
        let (b, d) = brains("rmpre");
        let p = b.preset_save(&json!({"name": "破限", "blocks": [{"kind": "text", "content": "A"}]})).unwrap();
        let kinds: Vec<&str> = p.blocks.iter().map(|x| x.kind.as_str()).collect();
        assert_eq!(kinds, vec!["builtin", "text", "request"], "佔位段要補回去");
        b.routes_save(&json!({"agent": {"brain": "page", "preset": p.id}})).unwrap();
        assert_eq!(b.resolve("agent", "9b").unwrap().1.unwrap().name, "破限");
        b.preset_delete(&p.id).unwrap();
        let (_, pre, h) = b.resolve("agent", "9b").unwrap();
        assert!(pre.is_none());
        assert!(h.unwrap().contains("預設不見了"));
        // 重開站讀回來
        let again = Brains::new(&d, &Map::new());
        assert_eq!(again.route("agent").preset.as_deref(), Some(p.id.as_str()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn routes_unknown_feature_rejected() {
        let (b, d) = brains("ruf");
        assert!(b.routes_save(&json!({"foo": {"brain": "9b"}})).unwrap_err().contains("foo"));
        assert!(b.routes_save(&json!({"agent": {"brain": "gpt"}})).is_err());
        assert_eq!(b.routes_view()["rewrite"]["brain"], "page");
        let _ = std::fs::remove_dir_all(&d);
    }

    fn sys(t: &str) -> Value {
        json!({"role": "system", "content": t})
    }
    fn user(t: &str) -> Value {
        json!({"role": "user", "content": t})
    }
    fn text(role: &str, t: &str, on: bool) -> Block {
        Block { id: t.into(), name: t.into(), role: role.into(), content: t.into(), enabled: on, kind: "text".into() }
    }
    fn fixture(name: &str) -> Value {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name);
        serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
    }

    #[test]
    fn assemble_none_is_identity() {
        assert_eq!(assemble(None, "SYS", &[user("hi")]), vec![sys("SYS"), user("hi")]);
        assert_eq!(assemble(None, "", &[user("hi")]), vec![user("hi")]);
    }

    #[test]
    fn assemble_order_and_disabled() {
        let mut p = Preset::new_default("t");
        p.blocks = vec![text("system", "A", true), marker("builtin"), text("system", "B", false), marker("request"), text("user", "C", true), text("system", "  ", true)];
        assert_eq!(assemble(Some(&p), "SYS", &[user("hi")]), vec![sys("A"), sys("SYS"), user("hi"), user("C")]);
    }

    #[test]
    fn request_block_cannot_be_disabled() {
        let mut p = Preset::new_default("t");
        for b in &mut p.blocks {
            b.enabled = false;
        }
        assert_eq!(assemble(Some(&p), "SYS", &[user("hi")]), vec![sys("SYS"), user("hi")]);
        // 佔位段被刪光：照樣補回來（內建在最前、請求在最後）
        p.blocks = vec![text("system", "A", true)];
        assert_eq!(assemble(Some(&p), "SYS", &[user("hi")]), vec![sys("SYS"), sys("A"), user("hi")]);
    }

    #[test]
    fn prefill_last() {
        let mut p = Preset::new_default("t");
        p.prefill = "好的，".into();
        let out = assemble(Some(&p), "SYS", &[user("hi")]);
        assert_eq!(out.last().unwrap(), &json!({"role": "assistant", "content": "好的，"}));
    }

    #[test]
    fn macros() {
        let mut p = Preset::new_default("t");
        p.user_name = "妳".into();
        p.char_name = "小克".into();
        p.blocks.insert(0, text("system", "{{char}} 跟 {{user}} {{random}}", true));
        p.prefill = "{{char}}：".into();
        let out = assemble(Some(&p), "SYS", &[user("hi")]);
        assert_eq!(out[0], sys("小克 跟 妳 {{random}}"));
        assert_eq!(out.last().unwrap()["content"], "小克：");
    }

    #[test]
    fn squash_merges_adjacent_system() {
        let mut p = Preset::new_default("t");
        p.blocks.insert(0, text("system", "A", true));
        p.squash_system = true;
        assert_eq!(assemble(Some(&p), "SYS", &[user("hi")]), vec![sys("A\n\nSYS"), user("hi")]);
        p.squash_system = false;
        assert_eq!(assemble(Some(&p), "SYS", &[user("hi")]), vec![sys("A"), sys("SYS"), user("hi")]);
    }

    #[test]
    fn assemble_keeps_image_parts() {
        let parts = json!({"role": "user", "content": [{"type": "image_url", "image_url": {"url": "data:x"}}, {"type": "text", "text": "看"}]});
        let p = Preset::new_default("t");
        assert_eq!(assemble(Some(&p), "SYS", std::slice::from_ref(&parts)), vec![sys("SYS"), parts]);
    }

    #[test]
    fn import_st_default() {
        let (p, report) = import_st(&fixture("st_like_default.json"), "Default.json").unwrap();
        assert_eq!(p.name, "Default");
        let kinds: Vec<(&str, &str)> = p.blocks.iter().map(|b| (b.kind.as_str(), b.id.as_str())).collect();
        assert_eq!(
            kinds,
            vec![("text", "main"), ("builtin", "builtin"), ("text", "enhanceDefinitions"), ("text", "nsfw"), ("request", "request"), ("text", "jailbreak")]
        );
        assert!(p.blocks[0].content.starts_with("Reply as {{char}}"));
        assert!(!p.blocks[2].enabled, "enhanceDefinitions 原本關著");
        assert_eq!(p.params.temperature, Some(1.0));
        assert_eq!(p.params.max_tokens, Some(300));
        let r = report.join("\n");
        for k in ["dialogueExamples", "charDescription", "worldInfoBefore", "scenario"] {
            assert!(r.contains(k), "報告要列出略過的 {k}：{r}");
        }
    }

    #[test]
    fn import_st_custom() {
        let (p, report) = import_st(&fixture("st_custom.json"), "大佬預設.json").unwrap();
        let ids: Vec<&str> = p.blocks.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, vec!["main", "builtin", "a1b2c3", "nsfw", "request", "d4e5f6", "jailbreak", "g7h8i9"]);
        assert_eq!(p.blocks[2].role, "user");
        assert!(!p.blocks[7].enabled);
        assert_eq!(p.prefill, "好的，{{char}} 明白了，以下是完整內容：");
        assert!(p.squash_system);
        assert_eq!(p.params, Params { temperature: Some(0.85), top_p: Some(0.9), top_k: Some(40), frequency_penalty: Some(0.1), presence_penalty: Some(0.2), max_tokens: Some(4000) });
        let r = report.join("\n");
        assert!(r.contains("第 4 層"), "{r}");
        assert!(r.contains("openai_model") && r.contains("reverse_proxy"), "{r}");
    }

    #[test]
    fn apply_splits_leading_system() {
        let msgs = json!([sys("SYS"), user("hi"), {"role": "assistant", "content": "ok"}]);
        assert_eq!(apply(None, &msgs), msgs);
        let mut p = Preset::new_default("t");
        p.blocks.insert(0, text("system", "A", true));
        p.prefill = "P".into();
        assert_eq!(apply(Some(&p), &msgs), json!([sys("A"), sys("SYS"), user("hi"), {"role": "assistant", "content": "ok"}, {"role": "assistant", "content": "P"}]));
        // 沒有 system 開頭：全部當請求
        assert_eq!(apply(Some(&Preset::new_default("t")), &json!([user("hi")])), json!([user("hi")]));
    }

    #[test]
    fn apply_params_only_filled() {
        let mut b = json!({"temperature": 0.7, "max_tokens": 100, "top_p": 0.95});
        apply_params(&mut b, &Params { temperature: Some(1.4), top_k: Some(40), ..Params::default() }, true);
        assert_eq!(b, json!({"temperature": 1.0, "max_tokens": 100, "top_p": 0.95}), "API：溫度上限 1、不放 top_k");
        let mut b = json!({"temperature": 0.7});
        apply_params(&mut b, &Params { temperature: Some(1.4), top_k: Some(40), max_tokens: Some(9), ..Params::default() }, false);
        assert_eq!(b, json!({"temperature": 1.4, "top_k": 40, "max_tokens": 9}));
    }

    #[test]
    fn import_garbage() {
        assert!(import_st(&json!({"foo": 1}), "x.json").unwrap_err().contains("找不到 prompts"));
    }

    #[test]
    fn import_no_order() {
        let v = json!({"prompts": [{"identifier": "main", "role": "system", "content": "M"}, {"identifier": "chatHistory", "marker": true}, {"identifier": "x", "role": "user", "content": "X"}]});
        let (p, report) = import_st(&v, "x.json").unwrap();
        let ids: Vec<&str> = p.blocks.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, vec!["main", "builtin", "request", "x"]);
        assert!(report.join("\n").contains("prompt_order"));
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("anima_brains_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn legacy() -> Map<String, Value> {
        json!({"api_url": "https://api.deepseek.com", "api_model": "deepseek-flash", "api_key": "sk-SECRET-TEST",
               "api_provider": "deepseek", "api_vision": {"for": "https://api.deepseek.com|deepseek-flash", "ok": false}})
        .as_object()
        .cloned()
        .unwrap()
    }

    #[test]
    fn migrate_from_cloud() {
        let d = tmp("mig");
        let p = Profiles::load(&d, &legacy());
        let g = p.get(DEFAULT_ID).expect("搬過來了");
        assert_eq!(g.url, "https://api.deepseek.com");
        assert_eq!(g.model, "deepseek-flash");
        assert_eq!(g.key.as_deref(), Some("sk-SECRET-TEST"));
        assert_eq!(g.provider, "deepseek");
        assert_eq!(g.vision_known(), Some(false));
        let disk = std::fs::read_to_string(d.join(PROFILES_FILE)).unwrap();
        // Linux 沒有 DPAPI：金鑰是明文（data/ 只給自己讀）
        #[cfg(windows)]
        assert!(!disk.contains("sk-SECRET-TEST"), "金鑰要加密");
        #[cfg(not(windows))]
        let _ = disk;
        // 重新讀一次，金鑰解得回來
        let again = Profiles::load(&d, &Map::new());
        assert_eq!(again.get(DEFAULT_ID).unwrap().key.as_deref(), Some("sk-SECRET-TEST"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn no_migration_when_file_exists() {
        let d = tmp("nomig");
        std::fs::write(d.join(PROFILES_FILE), r#"{"profiles":[]}"#).unwrap();
        let p = Profiles::load(&d, &legacy());
        assert!(p.get(DEFAULT_ID).is_none());
        assert_eq!(p.default_id(), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn upsert_key_rules() {
        let d = tmp("ups");
        let p = Profiles::load(&d, &Map::new());
        let a = p.upsert(&json!({"name": "A", "provider": "openai", "url": "https://api.openai.com/v1/", "model": "m", "key": "k1"})).unwrap();
        assert_eq!(a.url, "https://api.openai.com/v1");
        let id = a.id.clone();
        // 沒帶 key＝不動
        let b = p.upsert(&json!({"id": id, "name": "A", "url": "https://x.y", "model": "m2"})).unwrap();
        assert_eq!(b.key.as_deref(), Some("k1"));
        // 空字串＝清掉
        let c = p.upsert(&json!({"id": id, "url": "https://x.y", "model": "m2", "key": ""})).unwrap();
        assert_eq!(c.key, None);
        assert_eq!(c.name, "A", "沒給名字保留舊的");
        assert!(p.upsert(&json!({"id": id, "url": "https://x.y", "key": "a b"})).unwrap_err().contains("空白"));
        assert!(p.upsert(&json!({"url": "ftp://x"})).unwrap_err().contains("http"));
        assert!(p.remove("nope").is_err());
        p.remove(&id).unwrap();
        assert!(p.get(&id).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn list_view_hides_key() {
        let d = tmp("view");
        let p = Profiles::load(&d, &legacy());
        let v = p.list_view().to_string();
        assert!(!v.contains("sk-SECRET-TEST"));
        assert_eq!(p.list_view()[0]["has_key"], true);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn vision_resets_on_model_change() {
        let d = tmp("vis");
        let p = Profiles::load(&d, &legacy());
        p.set_vision(DEFAULT_ID, true).unwrap();
        assert_eq!(p.get(DEFAULT_ID).unwrap().vision_known(), Some(true));
        p.upsert(&json!({"id": DEFAULT_ID, "url": "https://api.deepseek.com", "model": "other"})).unwrap();
        assert_eq!(p.get(DEFAULT_ID).unwrap().vision_known(), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
