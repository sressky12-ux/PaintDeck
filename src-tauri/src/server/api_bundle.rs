//! 設定檔整包匯出入（api_gen::settings_export／settings_import 用）：把使用者的客製化收進同一個 JSON，換電腦一次帶走。
//!
//! 匯出檔（"app": "anima_ui"）的鍵：
//! - 一直都有的：settings、presets、prompt_lib、lora_meta
//! - 這一版加的：bundle（格式版本，2）、
//!   image_workflows［{id, set}］＝data\image_workflows\*.json、
//!   video_workflows［{id, workflow}］＝data\video_workflows\*.json、
//!   pe_prompts［{id, text}］＝data\pe_prompts\<編號>.txt（只放使用者改過的）、
//!   export_notes［文字］＝匯出時動過的地方（有才有）：工作流節點裡填了 API key／token／密碼的欄位，匯出的複本會清成空字串
//!
//!   舊版程式只認第一排，多出來的鍵直接忽略；新版匯入沒有第二排的舊檔，照舊處理。
//!
//! 不放進來的（原因）：
//! - Civitai API key、RunPod／雲端金鑰（data\agent\cloud\cloud.json）：金鑰不外流。
//! - model_dirs.json、net.json、comfy.json、config.json：是這台電腦的路徑、網路環境、顯示卡選擇。
//! - 圖庫（images、jobs、縮圖、預覽）、導演台／漫畫台／生圖助手的作品與對話、拆幀台：是作品內容，連著圖檔，不是設定。
//! - data\tags（Danbooru 字典）、data\models、data\tools、data\logs：是資料或程式，換電腦重新下載。
//!
//! 匯入絕不刪使用者的檔：同代號、內容不一樣的舊檔先搬進 <root>\待刪除\匯入前_<日期時間>\ 再寫新的；內容一樣的略過。
//! 檔案進來之前先用存檔時同一套檢查（image_wf::analyze／video_wf::analyze）驗證，看不懂的略過並寫進 warnings。
use crate::{image_wf, maint, pe, py, settings, video_wf};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

/// 匯出檔的格式版本（舊的匯出檔沒有 bundle 這一欄）
pub const BUNDLE_VERSION: u32 = 2;
const TRASH: &str = "待刪除";
const BACKUP_PREFIX: &str = "匯入前_";
const PROMPTS_DIR: &str = "pe_prompts";

/// JSON 值的「種類」：數字不分整數小數（舊版：型別一樣，或兩邊都是數字）
fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "num",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// 工作流的代號（檔名）：跟 image_wf／video_wf 存檔時同一套規則，所以不會跳出資料夾
fn id_ok(id: &str) -> bool {
    !id.is_empty() && video_wf::id_of(id) == id
}

fn prompts_dir(data: &Path) -> PathBuf {
    data.join(PROMPTS_DIR)
}

// ---------------------------------------------------------------- 匯出
/// 匯出的設定：只留設定頁認得的欄位，不含 Civitai API key（也不會夾帶設定檔裡任何不認得的欄位）
pub fn export_settings(s: &Map<String, Value>) -> Map<String, Value> {
    let defaults = settings::defaults();
    settings::without_key(s)
        .into_iter()
        .filter(|(k, _)| defaults.contains_key(k))
        .collect()
}

/// 資料夾裡的 *.json（代號合規則的、讀得懂的），照代號排序
fn json_files(dir: &Path) -> Vec<(String, Value)> {
    let mut out: Vec<(String, Value)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let id = path.file_stem()?.to_str()?.to_string();
            if path.extension()?.to_str()? != "json" || !id_ok(&id) {
                return None;
            }
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
            v.is_object().then_some((id, v))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// 欄位名稱像金鑰（API key、token、密碼…）嗎。tokenizer 的 pad_token、max_tokens 這類不算
fn secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase().replace(['-', ' '], "_");
    ["api_key", "apikey", "secret", "password", "passwd", "authorization", "bearer"]
        .iter()
        .any(|m| k.contains(m))
        || matches!(k.as_str(), "token" | "access_token" | "auth_token" | "api_token" | "refresh_token" | "hf_token" | "civitai_token")
}

/// 工作流裡某些自訂節點會把 API key 填在欄位裡：匯出的複本把有填的這類欄位清成空字串（原檔不動），記下是哪個節點的哪個欄位
fn scrub_secrets(v: &mut Value, hits: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            let class = m.get("class_type").and_then(Value::as_str).map(str::to_string);
            if let (Some(class), Some(inp)) = (class, m.get_mut("inputs").and_then(Value::as_object_mut)) {
                for (k, val) in inp.iter_mut() {
                    if secret_key(k) && val.as_str().is_some_and(|s| !s.trim().is_empty()) {
                        *val = json!("");
                        hits.push(format!("{class}.{k}"));
                    }
                }
            }
            for c in m.values_mut() {
                scrub_secrets(c, hits);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|c| scrub_secrets(c, hits)),
        _ => {}
    }
}

/// 一個資料夾的工作流 → [{id, <field>: 內容}]；金鑰欄位清掉的記進 notes
fn collect_sets(dir: &Path, field: &str, label: &str, notes: &mut Vec<String>) -> Vec<Value> {
    json_files(dir)
        .into_iter()
        .map(|(id, mut v)| {
            let mut hits = vec![];
            scrub_secrets(&mut v, &mut hits);
            if !hits.is_empty() {
                notes.push(format!("{label}「{id}」：{} 是金鑰欄位，匯出時已清空", hits.join("、")));
            }
            json!({"id": id, field: v})
        })
        .collect()
}

pub fn collect_image_workflows(data: &Path, notes: &mut Vec<String>) -> Vec<Value> {
    collect_sets(&image_wf::dir(data), "set", "生圖工作流", notes)
}

pub fn collect_video_workflows(data: &Path, notes: &mut Vec<String>) -> Vec<Value> {
    collect_sets(&video_wf::dir(data), "workflow", "影片工作流", notes)
}

/// 使用者改過的系統提示詞：跟 pe::Core::override_text 一樣，去頭尾空白後空的不算
pub fn collect_pe_prompts(data: &Path) -> Vec<Value> {
    let dir = prompts_dir(data);
    pe::Service::prompt_rules()
        .into_iter()
        .filter_map(|(id, _)| {
            let text = std::fs::read_to_string(dir.join(format!("{id}.txt"))).ok()?;
            let text = py::strip(&text);
            (!text.is_empty()).then(|| json!({"id": id, "text": text}))
        })
        .collect()
}

/// 匯出檔裡「這一版加的」鍵。export_notes（有東西才有）＝匯出時動過的地方，給人看
pub fn export_parts(data: &Path) -> Map<String, Value> {
    let mut notes = vec![];
    let mut m = Map::new();
    m.insert("bundle".into(), json!(BUNDLE_VERSION));
    m.insert("image_workflows".into(), Value::Array(collect_image_workflows(data, &mut notes)));
    m.insert("video_workflows".into(), Value::Array(collect_video_workflows(data, &mut notes)));
    m.insert("pe_prompts".into(), Value::Array(collect_pe_prompts(data)));
    if !notes.is_empty() {
        m.insert("export_notes".into(), json!(notes));
    }
    m
}

// ---------------------------------------------------------------- 匯入的結果
/// 一類東西的結果：寫進去幾個（新增或換掉）、內容一樣所以略過幾個
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Tally {
    pub written: usize,
    pub unchanged: usize,
}

#[derive(Debug, Default)]
pub struct Report {
    pub image_workflows: Tally,
    pub video_workflows: Tally,
    pub pe_prompts: Tally,
    /// 套用了幾個設定欄位（不含 feature_flows）
    pub settings: usize,
    /// 套用了幾筆「取代內建功能」
    pub feature_flows: usize,
    /// 搬進待刪除（或備份）的舊檔數
    pub backed_up: usize,
    /// 待刪除\匯入前_<時間>（有備份才有）
    pub backup_dir: Option<String>,
    pub warnings: Vec<String>,
}

impl Report {
    pub fn to_json(&self) -> Value {
        json!({
            "image_workflows": self.image_workflows.written,
            "video_workflows": self.video_workflows.written,
            "pe_prompts": self.pe_prompts.written,
            "unchanged": {
                "image_workflows": self.image_workflows.unchanged,
                "video_workflows": self.video_workflows.unchanged,
                "pe_prompts": self.pe_prompts.unchanged,
            },
            "settings": self.settings,
            "feature_flows": self.feature_flows,
            "backed_up": self.backed_up,
            "backup_dir": self.backup_dir,
            "warnings": self.warnings,
        })
    }
}

// ---------------------------------------------------------------- 待刪除備份
/// 這一次匯入的備份資料夾：<root>\待刪除\匯入前_<時間>\<類別>\<檔名>。第一次真的要搬檔才建
struct Backup {
    base: PathBuf,
    stamp: String,
    dir: Option<PathBuf>,
    count: usize,
}

impl Backup {
    fn new(root: &Path) -> Backup {
        Backup {
            base: root.join(TRASH),
            stamp: chrono::Local::now().format("%Y%m%d_%H%M%S").to_string(),
            dir: None,
            count: 0,
        }
    }

    fn dir(&mut self) -> io::Result<PathBuf> {
        if let Some(d) = &self.dir {
            return Ok(d.clone());
        }
        let mut d = self.base.join(format!("{BACKUP_PREFIX}{}", self.stamp));
        let mut n = 1;
        while d.exists() {
            d = self.base.join(format!("{BACKUP_PREFIX}{}_{n}", self.stamp));
            n += 1;
        }
        std::fs::create_dir_all(&d)?;
        self.dir = Some(d.clone());
        Ok(d)
    }

    /// 備份資料夾裡 sub\name 的位置；撞名就加編號，不覆蓋
    fn slot(&mut self, sub: &str, name: &Path) -> io::Result<PathBuf> {
        let dir = self.dir()?;
        let folder = if sub.is_empty() { dir } else { dir.join(sub) };
        let stem = name.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let ext = name.extension().map(|e| e.to_string_lossy().to_string());
        let mut dst = folder.join(name);
        let mut n = 1;
        while dst.exists() {
            dst = folder.join(match &ext {
                Some(e) => format!("{stem}_{n}.{e}"),
                None => format!("{stem}_{n}"),
            });
            n += 1;
        }
        Ok(dst)
    }

    /// 把舊檔搬進備份。回傳搬到哪
    fn take(&mut self, src: &Path, sub: &str) -> io::Result<PathBuf> {
        let name = PathBuf::from(src.file_name().ok_or_else(|| io::Error::other("檔名不對"))?);
        let dst = self.slot(sub, &name)?;
        maint::move_path(src, &dst)?;
        self.count += 1;
        Ok(dst)
    }

    /// 存一份文字檔進備份（設定檔用）
    fn put_text(&mut self, name: &str, text: &str) -> io::Result<()> {
        let dst = self.slot("", Path::new(name))?;
        std::fs::write(dst, text)?;
        self.count += 1;
        Ok(())
    }

    /// 寫新檔失敗時，把剛搬走的舊檔搬回原位
    fn undo(&mut self, original: &Path, moved_to: &Path) {
        if maint::move_path(moved_to, original).is_ok() {
            self.count -= 1;
        }
    }

    /// 給回應用：待刪除\匯入前_<時間>
    fn shown(&self) -> Option<String> {
        let name = self.dir.as_ref()?.file_name()?.to_string_lossy().to_string();
        Some(format!("{TRASH}\\{name}"))
    }
}

/// 先寫暫存檔、舊檔（有的話）搬進備份、再改名成正式檔。任何一步失敗都不會留下半個檔，舊檔也搬得回來
fn replace_file(path: &Path, text: &str, backup: Option<(&mut Backup, &str)>) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("建立資料夾失敗：{e}"))?;
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let tmp = path.with_extension(format!("{ext}.tmp"));
    std::fs::write(&tmp, text).map_err(|e| format!("存檔失敗：{e}"))?;
    let mut moved = None;
    if let Some((b, sub)) = backup {
        match b.take(path, sub) {
            Ok(dst) => moved = Some((b, dst)),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(format!("舊檔搬進待刪除失敗：{e}"));
            }
        }
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        if let Some((b, dst)) = moved {
            b.undo(path, &dst);
        }
        return Err(format!("存檔失敗：{e}"));
    }
    Ok(())
}

// ---------------------------------------------------------------- 檢查（跟存檔時同一套）
/// 檢查的結果：(要寫的內容, 提醒) 或不收的原因
type Checked = Result<(Value, Vec<String>), String>;

/// 生圖工作流組：每個模式的圖都用 image_wf::analyze 看過，看不懂的模式拿掉（跟 api_wf 整組匯入一樣）；一個都不能用就整組不收。
/// 回傳 (要寫的內容, 提醒)
fn check_image_set(raw: &Value) -> Checked {
    let norm = image_wf::normalize(raw.clone());
    let modes = image_wf::modes_of(&norm);
    if modes.is_empty() {
        return Err("裡面沒有任何工作流".into());
    }
    let (mut good, mut notes, mut first_err) = (vec![], vec![], None);
    for mode in modes {
        match image_wf::analyze(&norm["graphs"][mode], mode) {
            Ok(_) => good.push(mode),
            Err(e) => {
                notes.push(format!("{}的工作流看不懂（{e}），這個模式沒收", image_wf::mode_name(mode)));
                first_err.get_or_insert(e);
            }
        }
    }
    if good.is_empty() {
        return Err(format!("看不懂：{}", first_err.unwrap_or_default()));
    }
    let mut out = raw.clone();
    // 舊格式（graph）只有文生圖一個模式，能走到這裡就是看得懂；新格式把看不懂的模式拿掉
    if let Some(g) = out.get_mut("graphs").and_then(Value::as_object_mut) {
        g.retain(|k, _| good.contains(&k.as_str()));
    }
    Ok((out, notes))
}

/// 影片工作流：graph 要過 video_wf::analyze
fn check_video(raw: &Value) -> Checked {
    let graph = raw.get("graph").ok_or("裡面沒有工作流（graph）")?;
    video_wf::analyze(graph).map_err(|e| format!("看不懂：{e}"))?;
    Ok((raw.clone(), vec![]))
}

/// 一類 JSON 檔（生圖工作流組、影片工作流）怎麼匯入
struct JsonSet {
    /// 匯出檔裡的鍵
    key: &'static str,
    /// 每一筆裡放內容的欄位
    field: &'static str,
    dir: PathBuf,
    /// 備份資料夾裡的子資料夾
    sub: &'static str,
    /// 給人看的名稱
    label: &'static str,
    check: fn(&Value) -> Checked,
    /// 比較「內容一樣」前先整理成同一種格式（舊格式的生圖工作流和新格式算一樣）
    norm: fn(Value) -> Value,
}

struct Importer<'a> {
    data: &'a Path,
    backup: Backup,
    report: Report,
}

impl Importer<'_> {
    fn warn(&mut self, msg: String) {
        self.report.warnings.push(msg);
    }

    fn json_sets(&mut self, body: &Value, spec: &JsonSet) -> Tally {
        let (mut t, mut seen) = (Tally::default(), HashSet::new());
        let label = spec.label;
        for item in body.get(spec.key).and_then(Value::as_array).into_iter().flatten() {
            let id = item.get("id").and_then(Value::as_str).unwrap_or("");
            if !id_ok(id) {
                self.warn(format!("{label}「{id}」的代號不合規則，已略過"));
                continue;
            }
            if !seen.insert(id) {
                self.warn(format!("{label}「{id}」在檔案裡出現了不只一次，只收第一個"));
                continue;
            }
            let Some(raw) = item.get(spec.field).filter(|v| v.is_object()) else {
                self.warn(format!("{label}「{id}」沒有內容，已略過"));
                continue;
            };
            let (mut value, notes) = match (spec.check)(raw) {
                Ok(x) => x,
                Err(e) => {
                    self.warn(format!("{label}「{id}」{e}，已略過"));
                    continue;
                }
            };
            for n in notes {
                self.warn(format!("{label}「{id}」{n}"));
            }
            if !value.get("name").and_then(Value::as_str).is_some_and(|n| !n.trim().is_empty()) {
                value["name"] = json!(id);
            }
            let path = spec.dir.join(format!("{id}.json"));
            let exists = path.is_file();
            if exists {
                let old: Option<Value> = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok());
                if old.is_some_and(|o| (spec.norm)(o) == (spec.norm)(value.clone())) {
                    t.unchanged += 1;
                    continue;
                }
            }
            let text = match serde_json::to_string_pretty(&value) {
                Ok(t) => t,
                Err(e) => {
                    self.warn(format!("{label}「{id}」轉成文字失敗：{e}"));
                    continue;
                }
            };
            let backup = if exists { Some((&mut self.backup, spec.sub)) } else { None };
            match replace_file(&path, &text, backup) {
                Ok(()) => t.written += 1,
                Err(e) => self.warn(format!("{label}「{id}」{e}")),
            }
        }
        t
    }

    fn pe_prompts(&mut self, body: &Value) -> Tally {
        let (mut t, mut seen) = (Tally::default(), HashSet::new());
        let rules = pe::Service::prompt_rules();
        let dir = prompts_dir(self.data);
        for item in body.get("pe_prompts").and_then(Value::as_array).into_iter().flatten() {
            let id = item.get("id").and_then(Value::as_str).unwrap_or("");
            let Some((_, keep)) = rules.iter().find(|(i, _)| *i == id) else {
                self.warn(format!("自訂提示詞「{id}」這個版本沒有這一項，已略過"));
                continue;
            };
            if !seen.insert(id) {
                self.warn(format!("自訂提示詞「{id}」在檔案裡出現了不只一次，只收第一個"));
                continue;
            }
            let text = py::strip(item.get("text").and_then(Value::as_str).unwrap_or(""));
            if text.is_empty() {
                self.warn(format!("自訂提示詞「{id}」的內容是空的，已略過"));
                continue;
            }
            if text.chars().count() > pe::Service::PROMPT_MAX_CHARS {
                self.warn(format!("自訂提示詞「{id}」太長了（上限六萬字），已略過"));
                continue;
            }
            let missing: Vec<&str> = keep.iter().copied().filter(|k| !text.contains(k)).collect();
            if !missing.is_empty() {
                self.warn(format!("自訂提示詞「{id}」少了 {}，程式要靠它們填入內容，已略過", missing.join("、")));
                continue;
            }
            let path = dir.join(format!("{id}.txt"));
            let old = std::fs::read_to_string(&path).ok();
            let old_text = old.as_deref().map(py::strip).unwrap_or("");
            if old_text == text {
                t.unchanged += 1;
                continue;
            }
            // 現有的檔有東西（或讀不了）才需要備份；空白檔本來就不算自訂
            let backup = if path.is_file() && (old.is_none() || !old_text.is_empty()) {
                Some((&mut self.backup, PROMPTS_DIR))
            } else {
                None
            };
            match replace_file(&path, text, backup) {
                Ok(()) => t.written += 1,
                Err(e) => self.warn(format!("自訂提示詞「{id}」{e}")),
            }
        }
        t
    }

    /// 取代內建功能：匯入的幾筆，要指到「這台電腦上存在、而且有那個模式」的工作流組才收；原本的其他筆保留
    fn merge_flows(&mut self, s: &mut Map<String, Value>, incoming: &Value) {
        let mut flows = s.get("feature_flows").and_then(Value::as_object).cloned().unwrap_or_default();
        let dir = image_wf::dir(self.data);
        for (key, v) in incoming.as_object().into_iter().flatten() {
            let id = v.as_str().map(str::trim).unwrap_or("");
            if id.is_empty() {
                continue; // 空的＝用內建，不用記
            }
            if !image_wf::flow_key_ok(key) {
                self.warn(format!("取代內建功能「{key}」的格式不對（要寫成「家族.模式」），已略過"));
                continue;
            }
            let mode = key.split_once('.').map_or("", |(_, m)| m);
            match image_wf::load(&dir, id) {
                Ok(set) if image_wf::has_mode(&set, mode) => {
                    flows.insert(key.clone(), json!(id));
                    self.report.feature_flows += 1;
                }
                Ok(_) => self.warn(format!("取代內建功能「{key}」：工作流「{id}」沒有{}的工作流，已略過", image_wf::mode_name(mode))),
                Err(_) => self.warn(format!("取代內建功能「{key}」：這台電腦上沒有工作流「{id}」，已略過")),
            }
        }
        s.insert("feature_flows".into(), Value::Object(flows));
    }

    /// 設定：只收認得的欄位、永遠不動 API key；feature_flows 和 video_engine 要指到存在的工作流才收
    fn merge_settings(&mut self, cur: &Map<String, Value>, incoming: Option<&Value>) -> Map<String, Value> {
        let defaults = settings::defaults();
        let mut s = cur.clone();
        for (k, v) in incoming.and_then(Value::as_object).into_iter().flatten() {
            let Some(d) = defaults.get(k) else { continue };
            if k == settings::KEY || kind(v) != kind(d) {
                continue;
            }
            match k.as_str() {
                "feature_flows" => self.merge_flows(&mut s, v),
                "video_engine" => {
                    let id = v.as_str().unwrap_or("");
                    if video_wf::is_builtin(id) || video_wf::load(&video_wf::dir(self.data), id).is_ok() {
                        s.insert(k.clone(), v.clone());
                        self.report.settings += 1;
                    } else {
                        self.warn(format!("影片模型「{id}」這台電腦上沒有這個影片工作流，維持原本的選擇"));
                    }
                }
                _ => {
                    s.insert(k.clone(), v.clone());
                    self.report.settings += 1;
                }
            }
        }
        s
    }
}

/// 匯入整個匯出檔裡「檔案和設定」的部分（提示詞庫、預設組、LoRA 備註在資料庫，由 api_gen 接著處理）。
/// 呼叫的人要先拿 settings_lock。順序：工作流組、影片工作流、自訂提示詞 → 設定（要等工作流進來才知道 feature_flows、video_engine 有沒有指對）
pub fn import_bundle(data: &Path, root: &Path, settings_path: &Path, body: &Value) -> Result<Report, String> {
    let mut imp = Importer { data, backup: Backup::new(root), report: Report::default() };
    let sets = [
        JsonSet {
            key: "image_workflows",
            field: "set",
            dir: image_wf::dir(data),
            sub: "image_workflows",
            label: "生圖工作流",
            check: check_image_set,
            norm: image_wf::normalize,
        },
        JsonSet {
            key: "video_workflows",
            field: "workflow",
            dir: video_wf::dir(data),
            sub: "video_workflows",
            label: "影片工作流",
            check: check_video,
            norm: |v| v,
        },
    ];
    imp.report.image_workflows = imp.json_sets(body, &sets[0]);
    imp.report.video_workflows = imp.json_sets(body, &sets[1]);
    imp.report.pe_prompts = imp.pe_prompts(body);

    let cur = settings::load(settings_path);
    let merged = imp.merge_settings(&cur, body.get("settings"));
    if merged != cur {
        // 設定檔本身也是使用者的東西：被換掉之前留一份（拿掉金鑰）
        let old = settings::to_text(&Value::Object(settings::without_key(&cur)));
        imp.backup.put_text("settings.json", &old).map_err(|e| format!("備份原本的設定失敗：{e}"))?;
        settings::save(settings_path, &merged).map_err(|e| format!("存設定失敗：{e}"))?;
    }
    imp.report.backed_up = imp.backup.count;
    imp.report.backup_dir = imp.backup.shown();
    Ok(imp.report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sdxl() -> Value {
        json!({
            "4": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "sdxl.safetensors"}},
            "5": {"class_type": "EmptyLatentImage", "inputs": {"width": 1024, "height": 1024, "batch_size": 4}},
            "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "a cat", "clip": ["4", 1]}},
            "7": {"class_type": "CLIPTextEncode", "inputs": {"text": "blurry", "clip": ["4", 1]}},
            "3": {"class_type": "KSampler", "inputs": {"seed": 1, "steps": 20, "cfg": 7, "sampler_name": "euler", "scheduler": "normal", "denoise": 1,
                   "model": ["4", 0], "positive": ["6", 0], "negative": ["7", 0], "latent_image": ["5", 0]}},
            "8": {"class_type": "VAEDecode", "inputs": {"samples": ["3", 0], "vae": ["4", 2]}},
            "9": {"class_type": "SaveImage", "inputs": {"images": ["8", 0], "filename_prefix": "ComfyUI"}}
        })
    }

    fn sdxl_img2img() -> Value {
        let mut g = sdxl();
        let o = g.as_object_mut().unwrap();
        o.remove("5");
        o.insert("10".into(), json!({"class_type": "LoadImage", "inputs": {"image": "example.png"}}));
        o.insert("11".into(), json!({"class_type": "VAEEncode", "inputs": {"pixels": ["10", 0], "vae": ["4", 2]}}));
        g["3"]["inputs"]["latent_image"] = json!(["11", 0]);
        g["3"]["inputs"]["denoise"] = json!(0.5);
        g
    }

    fn wan() -> Value {
        json!({
            "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "a cat", "clip": ["38", 0]}},
            "7": {"class_type": "CLIPTextEncode", "inputs": {"text": "blurry", "clip": ["38", 0]}},
            "38": {"class_type": "CLIPLoader", "inputs": {"clip_name": "umt5.safetensors", "type": "wan"}},
            "52": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
            "50": {"class_type": "WanImageToVideo", "inputs": {"positive": ["6", 0], "negative": ["7", 0], "start_image": ["52", 0],
                   "width": 832, "height": 480, "length": 81, "batch_size": 1}},
            "3": {"class_type": "KSampler", "inputs": {"seed": 123, "positive": ["50", 0], "negative": ["50", 1], "latent_image": ["50", 2]}},
            "8": {"class_type": "VAEDecode", "inputs": {"samples": ["3", 0]}},
            "57": {"class_type": "CreateVideo", "inputs": {"images": ["8", 0], "fps": 16}},
            "58": {"class_type": "SaveVideo", "inputs": {"video": ["57", 0], "filename_prefix": "video/ComfyUI", "format": "auto", "codec": "auto"}},
        })
    }

    const AGENT_OK: &str = "你是助手。{model_guide}\n尺寸：{sizes}\n{loras}";

    /// 乾淨的測試資料夾：<tmp>\anima_bundle_<pid>_<tag>\{data}，root 就是它
    fn root(tag: &str) -> PathBuf {
        let r = std::env::temp_dir().join(format!("anima_bundle_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&r);
        std::fs::create_dir_all(r.join("data")).unwrap();
        r
    }

    fn put(dir: &Path, name: &str, text: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), text).unwrap();
    }

    fn pretty(v: &Value) -> String {
        serde_json::to_string_pretty(v).unwrap()
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    fn set_sdxl() -> Value {
        json!({"name": "我的 SDXL", "added": 1.5, "family": "sdxl", "source": "import", "options": {"page_sampler": true},
               "graphs": {"txt2img": sdxl(), "img2img": sdxl_img2img()}})
    }

    fn set_legacy() -> Value {
        json!({"name": "舊的", "added": 2.5, "graph": sdxl()})
    }

    fn video_wan() -> Value {
        json!({"name": "Wan 2.2", "added": 3.5, "graph": wan()})
    }

    /// 來源電腦：兩個生圖工作流組、一個影片工作流、兩份自訂提示詞（外加不該被匯出的雜物）、一份設定
    fn make_source(r: &Path) -> PathBuf {
        let data = r.join("data");
        put(&image_wf::dir(&data), "我的 SDXL.json", &pretty(&set_sdxl()));
        put(&image_wf::dir(&data), "舊的.json", &pretty(&set_legacy()));
        put(&image_wf::dir(&data), "a.b.json", &pretty(&set_legacy())); // 代號不合規則
        put(&image_wf::dir(&data), "壞掉.json", "{not json");
        put(&image_wf::dir(&data), "暫存.json.tmp", "{}");
        put(&video_wf::dir(&data), "Wan 2_2.json", &pretty(&video_wan()));
        put(&data.join("pe_prompts"), "t2i.txt", "自訂的文生圖說明\n");
        put(&data.join("pe_prompts"), "agent.txt", AGENT_OK);
        put(&data.join("pe_prompts"), "adult.txt", "  \n"); // 空白不算自訂
        put(&data.join("pe_prompts"), "unknown.txt", "不是可以自訂的項目");
        let mut s = settings::defaults();
        s.insert("negative".into(), json!("我的負面詞"));
        s.insert("video_engine".into(), json!("Wan 2_2"));
        s.insert("feature_flows".into(), json!({"sdxl.txt2img": "我的 SDXL"}));
        settings::save(&data.join("settings.json"), &s).unwrap();
        data
    }

    /// 匯出檔：跟 settings_export 一樣組起來，再繞一圈文字（像真的下載再上傳）
    fn bundle_of(data: &Path) -> Value {
        let mut b = json!({"app": "anima_ui", "version": "test", "exported": 1.0,
                           "settings": export_settings(&settings::load(&data.join("settings.json"))),
                           "presets": [], "prompt_lib": [], "lora_meta": {}});
        b.as_object_mut().unwrap().extend(export_parts(data));
        serde_json::from_str(&settings::to_text(&b)).unwrap()
    }

    fn backup_dirs(r: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(r.join(TRASH))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(BACKUP_PREFIX)))
            .collect();
        v.sort();
        v
    }

    fn run(r: &Path, body: &Value) -> Report {
        import_bundle(&r.join("data"), r, &r.join("data").join("settings.json"), body).unwrap()
    }

    #[test]
    fn export_shape_and_filters() {
        let r = root("shape");
        let data = make_source(&r);
        let b = bundle_of(&data);
        assert_eq!(b["bundle"], BUNDLE_VERSION);
        // 舊版認得的鍵都還在
        for k in ["app", "version", "exported", "settings", "presets", "prompt_lib", "lora_meta"] {
            assert!(b.get(k).is_some(), "{k}");
        }
        let ids = |k: &str| -> Vec<String> { b[k].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap().to_string()).collect() };
        assert_eq!(ids("image_workflows"), vec!["我的 SDXL", "舊的"], "壞檔、代號不合規則、暫存檔不匯出");
        assert_eq!(b["image_workflows"][0]["set"], set_sdxl());
        assert_eq!(ids("video_workflows"), vec!["Wan 2_2"]);
        assert_eq!(b["video_workflows"][0]["workflow"], video_wan());
        // 只放改過的、看得到的項目：空白檔和不認得的項目不放；文字去掉頭尾空白
        assert_eq!(ids("pe_prompts"), vec!["t2i", "agent"]);
        assert_eq!(b["pe_prompts"][0]["text"], "自訂的文生圖說明");
        assert_eq!(b["settings"]["negative"], "我的負面詞");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn export_settings_has_no_secrets_or_unknown_fields() {
        let mut s = settings::defaults();
        s.insert(settings::KEY.into(), json!("SECRET-TEST-VALUE"));
        s.insert("civitai_key_dpapi".into(), json!("SECRET-BLOB"));
        s.insert("runpod_key_dpapi".into(), json!("SECRET-BLOB-2"));
        s.insert("model_dirs".into(), json!(["D:\\models"]));
        let out = Value::Object(export_settings(&s));
        let text = out.to_string();
        assert!(!text.contains("SECRET") && !text.contains("D:\\\\models"), "{text}");
        assert!(out.get("negative").is_some() && out.get("feature_flows").is_some() && out.get("video_engine").is_some());
        assert!(out.get("civitai_key").is_none());
    }

    #[test]
    fn export_blanks_secret_fields_in_workflows() {
        let r = root("scrub");
        let data = r.join("data");
        // 圖裡多一個填了 API key 的自訂節點（max_tokens 是數字、pad_token 是 tokenizer 的設定，都不是金鑰）
        let mut g = sdxl();
        g["99"] = json!({"class_type": "SomeLLMNode", "inputs": {"api_key": "sk-SECRET-1", "max_tokens": 512, "pad_token": "<pad>", "Authorization": "Bearer SECRET-2", "token": "  "}});
        let set = json!({"name": "含金鑰", "graphs": {"txt2img": g}});
        put(&image_wf::dir(&data), "含金鑰.json", &pretty(&set));
        let mut vg = wan();
        vg["98"] = json!({"class_type": "HfDownloader", "inputs": {"hf_token": "hf_SECRET-3"}});
        put(&video_wf::dir(&data), "含金鑰影片.json", &pretty(&json!({"name": "含金鑰影片", "graph": vg})));
        let parts = Value::Object(export_parts(&data));
        let text = parts.to_string();
        assert!(!text.contains("SECRET"), "{text}");
        let n = &parts["image_workflows"][0]["set"]["graphs"]["txt2img"]["99"]["inputs"];
        assert_eq!((n["api_key"].as_str(), n["Authorization"].as_str()), (Some(""), Some("")));
        assert_eq!((n["max_tokens"].as_i64(), n["pad_token"].as_str()), (Some(512), Some("<pad>")));
        let notes = parts["export_notes"].to_string();
        assert!(notes.contains("SomeLLMNode.api_key") && notes.contains("HfDownloader.hf_token") && !notes.contains("pad_token"), "{notes}");
        // 原檔沒被動
        assert!(read(&image_wf::dir(&data).join("含金鑰.json")).contains("sk-SECRET-1"));
        // 沒有金鑰欄位就沒有 export_notes
        let clean = root("scrub_clean");
        make_source(&clean);
        assert!(export_parts(&clean.join("data")).get("export_notes").is_none());
        let _ = std::fs::remove_dir_all(&r);
        let _ = std::fs::remove_dir_all(&clean);
    }

    #[test]
    fn round_trip_into_empty_data_dir() {
        let src_root = root("rt_src");
        let src = make_source(&src_root);
        let body = bundle_of(&src);

        let dst_root = root("rt_dst");
        let r = run(&dst_root, &body);
        let dst = dst_root.join("data");
        assert_eq!((r.image_workflows.written, r.video_workflows.written, r.pe_prompts.written), (2, 1, 2));
        assert_eq!((r.image_workflows.unchanged, r.video_workflows.unchanged, r.pe_prompts.unchanged), (0, 0, 0));
        assert_eq!(r.backed_up, 1, "只有設定檔被換掉，留一份");
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(r.feature_flows, 1);
        // 檔案逐字相同
        for (a, b) in [
            (image_wf::dir(&src).join("我的 SDXL.json"), image_wf::dir(&dst).join("我的 SDXL.json")),
            (image_wf::dir(&src).join("舊的.json"), image_wf::dir(&dst).join("舊的.json")),
            (video_wf::dir(&src).join("Wan 2_2.json"), video_wf::dir(&dst).join("Wan 2_2.json")),
            (src.join("pe_prompts").join("t2i.txt"), dst.join("pe_prompts").join("t2i.txt")),
            (src.join("pe_prompts").join("agent.txt"), dst.join("pe_prompts").join("agent.txt")),
        ] {
            assert_eq!(read(&a).trim(), read(&b).trim(), "{}", b.display());
        }
        assert_eq!(read(&image_wf::dir(&src).join("我的 SDXL.json")), read(&image_wf::dir(&dst).join("我的 SDXL.json")));
        assert_eq!(read(&video_wf::dir(&src).join("Wan 2_2.json")), read(&video_wf::dir(&dst).join("Wan 2_2.json")));
        // 不該來的沒來
        assert!(!image_wf::dir(&dst).join("a.b.json").exists() && !dst.join("pe_prompts").join("unknown.txt").exists());
        // 設定也跟過來，沒有 .tmp 殘留
        let s = settings::load(&dst.join("settings.json"));
        assert_eq!((s["negative"].as_str(), s["video_engine"].as_str()), (Some("我的負面詞"), Some("Wan 2_2")));
        assert_eq!(s["feature_flows"], json!({"sdxl.txt2img": "我的 SDXL"}));
        assert!(std::fs::read_dir(image_wf::dir(&dst)).unwrap().flatten().all(|e| e.path().extension().is_some_and(|x| x == "json")));
        // 再匯一次：全部內容一樣，什麼都不寫、不備份
        let again = run(&dst_root, &body);
        assert_eq!((again.image_workflows, again.video_workflows, again.pe_prompts), (Tally { written: 0, unchanged: 2 }, Tally { written: 0, unchanged: 1 }, Tally { written: 0, unchanged: 2 }));
        assert_eq!(again.backed_up, 0);
        assert_eq!(backup_dirs(&dst_root).len(), 1, "第二次沒有新的備份資料夾");
        let _ = std::fs::remove_dir_all(&src_root);
        let _ = std::fs::remove_dir_all(&dst_root);
    }

    #[test]
    fn different_content_moves_old_file_to_trash() {
        let src_root = root("cf_src");
        let body = bundle_of(&make_source(&src_root));

        let r = root("cf_dst");
        let data = r.join("data");
        // 同代號、內容不一樣：生圖工作流組「我的 SDXL」、影片工作流、t2i 提示詞；內容一樣：「舊的」和 agent
        let old_set = json!({"name": "舊版內容", "added": 9.0, "graphs": {"txt2img": sdxl()}});
        put(&image_wf::dir(&data), "我的 SDXL.json", &pretty(&old_set));
        put(&image_wf::dir(&data), "舊的.json", &pretty(&set_legacy()));
        let old_video = json!({"name": "舊影片", "graph": wan()});
        put(&video_wf::dir(&data), "Wan 2_2.json", &pretty(&old_video));
        put(&data.join("pe_prompts"), "t2i.txt", "我原本寫的");
        put(&data.join("pe_prompts"), "agent.txt", &format!("{AGENT_OK}\n\n"));
        // 這台的設定：負面詞不同
        let mut s = settings::defaults();
        s.insert("negative".into(), json!("這台原本的負面詞"));
        settings::save(&data.join("settings.json"), &s).unwrap();

        let rep = run(&r, &body);
        assert_eq!((rep.image_workflows, rep.video_workflows, rep.pe_prompts),
                   (Tally { written: 1, unchanged: 1 }, Tally { written: 1, unchanged: 0 }, Tally { written: 1, unchanged: 1 }));
        assert_eq!(rep.backed_up, 4, "三個舊檔＋原本的設定");
        let dirs = backup_dirs(&r);
        assert_eq!(dirs.len(), 1);
        let bk = &dirs[0];
        assert_eq!(rep.backup_dir.as_deref(), Some(format!("待刪除\\{}", bk.file_name().unwrap().to_string_lossy()).as_str()));
        // 舊檔原封不動在備份裡
        assert_eq!(read(&bk.join("image_workflows").join("我的 SDXL.json")), pretty(&old_set));
        assert_eq!(read(&bk.join("video_workflows").join("Wan 2_2.json")), pretty(&old_video));
        assert_eq!(read(&bk.join("pe_prompts").join("t2i.txt")), "我原本寫的");
        assert!(!bk.join("image_workflows").join("舊的.json").exists(), "內容一樣的不搬");
        assert!(!bk.join("pe_prompts").join("agent.txt").exists());
        let kept = read(&bk.join("settings.json"));
        assert!(kept.contains("這台原本的負面詞") && !kept.contains("civitai_key"), "{kept}");
        // 新的寫進去了
        assert_eq!(read(&image_wf::dir(&data).join("我的 SDXL.json")), pretty(&set_sdxl()));
        assert_eq!(read(&video_wf::dir(&data).join("Wan 2_2.json")), pretty(&video_wan()));
        assert_eq!(read(&data.join("pe_prompts").join("t2i.txt")), "自訂的文生圖說明");
        assert_eq!(settings::load(&data.join("settings.json"))["negative"], "我的負面詞");
        // 同一秒再匯一次不同內容：備份資料夾和檔名撞名時加編號，不覆蓋
        put(&data.join("pe_prompts"), "t2i.txt", "又改了一次");
        let rep2 = run(&r, &body);
        assert_eq!(rep2.pe_prompts.written, 1);
        let dirs = backup_dirs(&r);
        let all: Vec<String> = dirs.iter().flat_map(|d| std::fs::read_dir(d.join("pe_prompts")).unwrap().flatten().map(|e| read(&e.path()))).collect();
        assert!(all.contains(&"我原本寫的".to_string()) && all.contains(&"又改了一次".to_string()), "{all:?}");
        let _ = std::fs::remove_dir_all(&src_root);
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn invalid_items_are_skipped_with_warnings() {
        let r = root("bad");
        let data = r.join("data");
        let body = json!({
            "app": "anima_ui",
            "image_workflows": [
                {"id": "壞的", "set": {"name": "壞的", "graphs": {"txt2img": {"nope": 1}}}},
                {"id": "半好", "set": {"name": "半好", "graphs": {"txt2img": sdxl(), "upscale": {"bad": 1}}}},
                {"id": "../evil", "set": set_sdxl()},
                {"id": "a/b", "set": set_sdxl()},
                {"id": "沒內容"},
                {"id": "空組", "set": {"name": "x", "graphs": {}}},
                {"id": "重複", "set": set_sdxl()},
                {"id": "重複", "set": set_legacy()},
                {"id": "CON", "set": set_sdxl()},
            ],
            "video_workflows": [
                {"id": "壞影片", "workflow": {"name": "x", "graph": {"nodes": [], "links": []}}},
                {"id": "h3", "workflow": video_wan()},
                {"id": "沒graph", "workflow": {"name": "x"}},
            ],
            "pe_prompts": [
                {"id": "nope", "text": "x"},
                {"id": "agent", "text": "沒有佔位符"},
                {"id": "t2i", "text": "   "},
                {"id": "adult_pose", "text": "只有 {orientation} {style}"},
            ],
        });
        let rep = run(&r, &body);
        assert_eq!((rep.image_workflows.written, rep.video_workflows.written, rep.pe_prompts.written), (2, 0, 0), "{:?}", rep.warnings);
        let names = |d: PathBuf| -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
            v.sort();
            v
        };
        assert_eq!(names(image_wf::dir(&data)), vec!["半好.json", "重複.json"]);
        assert!(names(video_wf::dir(&data)).is_empty() && names(data.join("pe_prompts")).is_empty());
        // 半好：看不懂的放大模式被拿掉，文生圖留著
        let half: Value = serde_json::from_str(&read(&image_wf::dir(&data).join("半好.json"))).unwrap();
        assert_eq!(half["graphs"].as_object().unwrap().keys().collect::<Vec<_>>(), vec!["txt2img"]);
        // 重複的只收第一個
        let dup: Value = serde_json::from_str(&read(&image_wf::dir(&data).join("重複.json"))).unwrap();
        assert_eq!(dup, set_sdxl());
        // 沒有東西跑出資料夾
        assert!(!data.join("evil.json").exists() && !data.join("image_workflows").join("evil.json").exists() && !r.join("evil.json").exists());
        let w = rep.warnings.join("\n");
        for needle in ["壞的", "放大", "../evil", "a/b", "沒內容", "空組", "重複", "CON", "壞影片", "「h3」", "沒graph", "nope", "{model_guide}", "內容是空的", "{act}"] {
            assert!(w.contains(needle), "少了 {needle}：\n{w}");
        }
        assert_eq!(rep.backed_up, 0);
        assert!(backup_dirs(&r).is_empty());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn feature_flows_and_video_engine_stay_valid() {
        let r = root("flows");
        let data = r.join("data");
        // 這台原本就有的組（有文生圖、局部重繪），和指向它的取代設定
        put(&image_wf::dir(&data), "本機組.json", &pretty(&json!({"name": "本機組", "graphs": {"txt2img": sdxl(), "inpaint": {"x": 1}}})));
        let mut s = settings::defaults();
        s.insert("feature_flows".into(), json!({"anima.upscale": "本機組", "anima.inpaint": "本機組"}));
        settings::save(&data.join("settings.json"), &s).unwrap();
        let body = json!({
            "app": "anima_ui",
            "settings": {
                "civitai_key": "SECRET-IN-FILE", "civitai_key_dpapi": "SECRET-BLOB", "unknown_field": 1,
                "negative": "新負面詞", "blur_nsfw": "不是布林",
                "feature_flows": {
                    "sdxl.img2img": "匯入組",         // 組在檔案裡、有這個模式 → 收
                    "anima.inpaint": "匯入組",        // 組有，但沒有局部重繪 → 丟
                    "sdxl.txt2img": "不存在的組",     // 這台沒有 → 丟
                    "壞鍵": "匯入組",                 // 鍵的格式不對 → 丟
                    "qwen21.txt2img": "",             // 空＝內建 → 不用記
                },
                "video_engine": "沒這個影片",
            },
            "image_workflows": [{"id": "匯入組", "set": {"name": "匯入組", "graphs": {"txt2img": sdxl(), "img2img": sdxl_img2img()}}}],
        });
        let rep = run(&r, &body);
        let s = settings::load(&data.join("settings.json"));
        assert_eq!(s["feature_flows"], json!({"anima.upscale": "本機組", "anima.inpaint": "本機組", "sdxl.img2img": "匯入組"}), "原本的保留，只加進有效的");
        assert_eq!(rep.feature_flows, 1);
        assert_eq!(s["video_engine"], video_wf::BUILTIN, "指到不存在的影片工作流：維持原本的");
        assert_eq!(s["negative"], "新負面詞");
        assert_eq!(s["blur_nsfw"], json!(true), "型別不對的欄位不收");
        let disk = read(&data.join("settings.json"));
        assert!(!disk.contains("SECRET") && !disk.contains("unknown_field"), "{disk}");
        let w = rep.warnings.join("\n");
        for needle in ["anima.inpaint", "沒有局部重繪", "不存在的組", "壞鍵", "沒有這個影片工作流"] {
            assert!(w.contains(needle), "少了 {needle}：\n{w}");
        }
        assert!(!w.contains("qwen21"), "空字串直接略過，不用吵人：\n{w}");

        // 影片工作流在同一個檔案裡：video_engine 指過去就收
        let body = json!({"app": "anima_ui", "settings": {"video_engine": "Wan 2_2"}, "video_workflows": [{"id": "Wan 2_2", "workflow": video_wan()}]});
        let rep = run(&r, &body);
        assert_eq!(settings::load(&data.join("settings.json"))["video_engine"], "Wan 2_2");
        assert_eq!((rep.video_workflows.written, rep.settings), (1, 1));
        // H3 影片台（不是工作流）照收
        run(&r, &json!({"app": "anima_ui", "settings": {"video_engine": "h3"}}));
        assert_eq!(settings::load(&data.join("settings.json"))["video_engine"], "h3");
        // 內建的永遠收
        run(&r, &json!({"app": "anima_ui", "settings": {"video_engine": video_wf::BUILTIN}}));
        assert_eq!(settings::load(&data.join("settings.json"))["video_engine"], video_wf::BUILTIN);
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn old_export_file_without_new_keys_imports_as_before() {
        let r = root("old");
        let data = r.join("data");
        let body = json!({"app": "anima_ui", "version": "1.0.0", "exported": 1.0,
                          "settings": {"negative": "舊檔的負面詞", "pe_idle": 30, "civitai_key": "SECRET"},
                          "presets": [], "prompt_lib": [], "lora_meta": {}});
        let rep = run(&r, &body);
        let j = rep.to_json();
        for k in ["image_workflows", "video_workflows", "pe_prompts", "feature_flows"] {
            assert_eq!(j[k], 0, "{k}");
        }
        assert_eq!(j["warnings"], json!([]));
        assert_eq!(rep.settings, 2);
        let s = settings::load(&data.join("settings.json"));
        assert_eq!((s["negative"].as_str(), s["pe_idle"].as_i64()), (Some("舊檔的負面詞"), Some(30)));
        assert!(!read(&data.join("settings.json")).contains("SECRET"));
        // 設定被換掉，所以留了一份原本的（預設值）
        let dirs = backup_dirs(&r);
        assert_eq!((dirs.len(), rep.backed_up), (1, 1));
        assert!(dirs[0].join("settings.json").is_file());
        // 完全一樣的設定再匯一次：不寫、不備份
        let again = run(&r, &body);
        assert_eq!((again.backed_up, again.settings), (0, 2));
        assert_eq!(backup_dirs(&r).len(), 1);
        assert!(again.backup_dir.is_none());
        let _ = std::fs::remove_dir_all(&r);
    }
}
