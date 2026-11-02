//! 工作節點（2026-11-02，設計：design\多節點生成_設計.md）：多張本機顯卡、區網／雲端的 ComfyUI 一起畫。
//!
//! - 主節點（id main）＝原本那台 `app.comfy`，永遠在；其他節點寫在 `<資料>\data\nodes.json`。
//! - 本機節點（gpuN）：桌面版替第 N 張 N 卡另外開一台 ComfyUI（`--cuda-device`），跟主節點共用安裝、模型、input／output。
//! - 遠端節點（r…）：填網址；輸入圖先上傳過去、畫完的圖抓回本機 output（comfy.rs）。
//! - 非主節點存檔前綴加標記（AnimaWeb/t2i → AnimaWeb/t2i_g1），兩台同秒存檔才不會算出同一個檔名互相蓋掉。

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

pub const MAIN: &str = "main";
const FILE: &str = "nodes.json";
/// 系統記憶體少於這個（GB）時，大模型同一時間只准一台本機節點跑
pub const RAM_GUARD_GB: f64 = 64.0;

#[derive(Clone, Debug, PartialEq)]
pub struct NodeCfg {
    pub id: String,
    pub name: String,
    pub url: String,
    /// local／remote（主節點不寫進設定檔）
    pub kind: String,
    /// 本機節點用哪張 N 卡（nvidia-smi 的編號）
    pub gpu: Option<u32>,
    pub enabled: bool,
    pub token: Option<String>,
    /// 本機節點多加的 ComfyUI 參數
    pub args: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cfg {
    pub nodes: Vec<NodeCfg>,
    /// 家族 → 只准去哪幾個節點（空的或沒寫＝哪台都行）
    pub bind: BTreeMap<String, Vec<String>>,
    pub ram_guard: bool,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg { nodes: vec![], bind: BTreeMap::new(), ram_guard: true }
    }
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

#[cfg(windows)]
fn seal(s: &str) -> Result<String, String> {
    crate::settings::protect(s).map_err(|e| format!("加密 token 失敗：{e}"))
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

impl NodeCfg {
    fn from_value(v: &Value) -> Option<NodeCfg> {
        let id = s(v, "id");
        let kind = s(v, "kind");
        if id.is_empty() || id == MAIN || !matches!(kind.as_str(), "local" | "remote") {
            return None;
        }
        let token = v.get("token_dpapi").and_then(Value::as_str).and_then(unseal).filter(|t| !t.is_empty());
        Some(NodeCfg {
            name: Some(s(v, "name")).filter(|n| !n.is_empty()).unwrap_or_else(|| id.clone()),
            url: s(v, "url").trim_end_matches('/').to_string(),
            gpu: v.get("gpu").and_then(Value::as_u64).map(|g| g as u32),
            enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            args: s(v, "args"),
            id,
            kind,
            token,
        })
    }
    fn to_disk(&self) -> Result<Value, String> {
        let mut m = Map::new();
        m.insert("id".into(), json!(self.id));
        m.insert("name".into(), json!(self.name));
        m.insert("url".into(), json!(self.url));
        m.insert("kind".into(), json!(self.kind));
        m.insert("enabled".into(), json!(self.enabled));
        m.insert("args".into(), json!(self.args));
        if let Some(g) = self.gpu {
            m.insert("gpu".into(), json!(g));
        }
        if let Some(t) = &self.token {
            m.insert("token_dpapi".into(), json!(seal(t)?));
        }
        Ok(Value::Object(m))
    }
    pub fn view(&self) -> Value {
        json!({"id": self.id, "name": self.name, "url": self.url, "kind": self.kind, "gpu": self.gpu,
               "enabled": self.enabled, "args": self.args, "has_token": self.token.is_some()})
    }
}

impl Cfg {
    pub fn from_value(v: &Value) -> Cfg {
        let nodes = v["nodes"].as_array().into_iter().flatten().filter_map(NodeCfg::from_value).collect();
        let bind = v["bind"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, ids)| (k.clone(), ids.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_owned)).collect::<Vec<_>>()))
            .filter(|(_, ids)| !ids.is_empty())
            .collect();
        Cfg { nodes, bind, ram_guard: v.get("ram_guard").and_then(Value::as_bool).unwrap_or(true) }
    }
    pub fn load(dir: &Path) -> Cfg {
        std::fs::read(dir.join(FILE)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).map(|v| Cfg::from_value(&v)).unwrap_or_default()
    }
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let nodes = self.nodes.iter().map(NodeCfg::to_disk).collect::<Result<Vec<_>, _>>()?;
        std::fs::create_dir_all(dir).map_err(|e| format!("建立 {} 失敗：{e}", dir.display()))?;
        crate::director::write_atomic(&dir.join(FILE), &json!({"nodes": nodes, "bind": self.bind, "ram_guard": self.ram_guard}))
            .map_err(|e| format!("存節點設定失敗：{e}"))
    }
    pub fn view(&self) -> Value {
        json!({"nodes": self.nodes.iter().map(NodeCfg::view).collect::<Vec<_>>(), "bind": self.bind, "ram_guard": self.ram_guard})
    }
    pub fn get(&self, id: &str) -> Option<&NodeCfg> {
        self.nodes.iter().find(|n| n.id == id)
    }
    /// POST /api/nodes 的內容套到舊設定上 → (新設定, 要不要重開 ComfyUI)。沒送的欄位照舊。
    /// 節點：沒 id 的配一個（本機 gpu卡號、遠端 r＋6 碼）；本機的網址＝127.0.0.1:主節點＋20＋卡號；
    /// token 沒送＝保留舊的、clear_token＝清掉。綁定裡不認識的節點拿掉。本機節點（卡號、參數、啟用）有變＝要重開
    pub fn update(old: &Cfg, b: &Value, main_port: u16) -> Result<(Cfg, bool), String> {
        let mut c = old.clone();
        if let Some(list) = b.get("nodes").and_then(Value::as_array) {
            let mut nodes: Vec<NodeCfg> = vec![];
            for v in list {
                let kind = s(v, "kind");
                let gpu = v.get("gpu").and_then(Value::as_u64).map(|g| g as u32);
                let mut id = s(v, "id");
                match kind.as_str() {
                    "local" => {
                        let g = gpu.ok_or("本機節點要選一張顯示卡")?;
                        if nodes.iter().any(|n| n.kind == "local" && n.gpu == Some(g)) {
                            return Err(format!("第 {g} 號卡重複了：一張卡開一個就好"));
                        }
                        let args = s(v, "args");
                        crate::gpu::extra_args(&args)?;
                        if args.split_whitespace().any(|a| a.starts_with("--cuda-d")) {
                            return Err("卡號由上面選的顯示卡決定，不能放在額外參數裡".into());
                        }
                        id = format!("gpu{g}");
                    }
                    "remote" => {
                        let url = s(v, "url");
                        if !(url.starts_with("http://") || url.starts_with("https://")) {
                            return Err(format!("網址要 http:// 或 https:// 開頭：{url}"));
                        }
                        let host = url.split("://").nth(1).unwrap_or("").split(['/', ':']).next().unwrap_or("");
                        if port_of(&url) == Some(main_port) && matches!(host, "127.0.0.1" | "localhost") {
                            return Err("這就是主節點（生圖站自己的 ComfyUI），不用再加".into());
                        }
                        if id.is_empty() || id == MAIN || old.get(&id).is_none_or(|n| n.kind != "remote") {
                            id = format!("r{:06x}", rand::random::<u32>() & 0xff_ffff);
                        }
                    }
                    _ => return Err(format!("不認識的節點種類：{kind}")),
                }
                let mut n = NodeCfg::from_value(&json!({"id": id, "kind": kind, "name": s(v, "name"), "url": s(v, "url"),
                    "gpu": gpu, "enabled": v.get("enabled").and_then(Value::as_bool).unwrap_or(true), "args": s(v, "args")}))
                    .ok_or("節點設定不對")?;
                if kind == "local" {
                    n.url = format!("http://127.0.0.1:{}", local_port(main_port, gpu.unwrap_or(0)));
                    if s(v, "name").is_empty() {
                        n.name = format!("第 {} 號卡", gpu.unwrap_or(0));
                    }
                }
                n.token = if v.get("clear_token").and_then(Value::as_bool) == Some(true) {
                    None
                } else {
                    Some(s(v, "token")).filter(|t| !t.is_empty()).or_else(|| old.get(&n.id).and_then(|o| o.token.clone()))
                };
                nodes.push(n);
            }
            c.nodes = nodes;
        }
        if let Some(bind) = b.get("bind").filter(|x| x.is_object()) {
            c.bind = Cfg::from_value(&json!({"bind": bind})).bind;
        }
        if let Some(g) = b.get("ram_guard").and_then(Value::as_bool) {
            c.ram_guard = g;
        }
        let known: Vec<String> = c.nodes.iter().map(|n| n.id.clone()).chain([MAIN.to_string()]).collect();
        for ids in c.bind.values_mut() {
            ids.retain(|i| known.contains(i));
        }
        c.bind.retain(|_, ids| !ids.is_empty());
        let restart = c.locals() != old.locals();
        Ok((c, restart))
    }
    fn locals(&self) -> Vec<(u32, String, bool)> {
        self.nodes.iter().filter(|n| n.kind == "local").map(|n| (n.gpu.unwrap_or(0), n.args.clone(), n.enabled)).collect()
    }
    fn kind_of(&self, id: &str) -> &str {
        if id == MAIN {
            return "main";
        }
        self.get(id).map(|n| n.kind.as_str()).unwrap_or("")
    }
}

/// 一個節點現在的狀況（Pool 從每台的即時狀態整理）
#[derive(Clone, Debug, PartialEq)]
pub struct Load {
    pub id: String,
    pub online: bool,
    /// 還沒跑完的件數（ComfyUI 的 queue_remaining）
    pub queue: i64,
    /// 上一個派給它的家族（同家族優先，省得換模型）
    pub family: String,
}

/// 大模型：Qwen 2.1、影片（含 H3）
pub fn heavy(family: &str, video: bool) -> bool {
    video || family == crate::families::QWEN21
}

/// 挑節點。順序：手動指定 → 家族綁定 → 影片不去遠端 → 記憶體防呆 → 在線的裡佇列最短、同家族優先、主節點優先。
/// busy_heavy＝現在正在跑（或排著）大模型的本機節點
pub fn pick(cfg: &Cfg, loads: &[Load], family: &str, manual: Option<&str>, video: bool, ram_gb: f64, busy_heavy: &[String]) -> Result<String, String> {
    let enabled = |id: &str| id == MAIN || cfg.get(id).is_some_and(|n| n.enabled);
    let mut cand: Vec<&Load> = loads.iter().filter(|l| l.online && enabled(&l.id)).collect();
    if let Some(m) = manual.filter(|m| !m.is_empty() && *m != "auto") {
        return cand.iter().find(|l| l.id == m).map(|l| l.id.clone()).ok_or_else(|| format!("選的節點「{m}」不在線或沒啟用：改選「自動」或到設定頁看節點狀態"));
    }
    if let Some(ids) = cfg.bind.get(family).filter(|ids| !ids.is_empty()) {
        cand.retain(|l| ids.contains(&l.id));
    }
    if video {
        cand.retain(|l| cfg.kind_of(&l.id) != "remote");
    }
    if cfg.ram_guard && ram_gb > 0.0 && ram_gb < RAM_GUARD_GB && heavy(family, video) {
        let local = |id: &str| cfg.kind_of(id) != "remote";
        let busy: Vec<&String> = busy_heavy.iter().filter(|id| local(id)).collect();
        if !busy.is_empty() {
            cand.retain(|l| !local(&l.id) || busy.contains(&&l.id));
            if cand.is_empty() {
                return Err(format!("電腦記憶體 {ram_gb:.0}GB，兩台同時跑大模型會卡死：等「{}」跑完，或到設定頁關掉記憶體防呆", busy[0]));
            }
        }
    }
    cand.sort_by_key(|l| (l.queue, l.family != family, l.id != MAIN, l.id.clone()));
    cand.first().map(|l| l.id.clone()).ok_or_else(|| {
        if loads.iter().any(|l| l.online) {
            format!("沒有可以跑「{family}」的節點：看設定頁的家族綁定")
        } else {
            "ComfyUI 沒有連線。解法：重新開啟「繪匣 PaintDeck」。".into()
        }
    })
}

/// 節點 id → 存檔標記（主節點沒有）
pub fn tag_of(id: &str) -> Option<String> {
    if id == MAIN {
        return None;
    }
    match id.strip_prefix("gpu") {
        Some(n) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => Some(format!("g{n}")),
        _ => Some(id.to_string()),
    }
}

/// 每個 filename_prefix 最後一段後面加 _標記：AnimaWeb/t2i → AnimaWeb/t2i_g1
pub fn tag_prefixes(nodes: &mut Value, tag: &str) {
    let Some(o) = nodes.as_object_mut() else { return };
    for n in o.values_mut() {
        if let Some(Value::String(p)) = n.get_mut("inputs").and_then(|i| i.get_mut("filename_prefix")) {
            *p = format!("{p}_{tag}");
        }
    }
}

/// 節點圖用到的 input 資料夾檔案（遠端節點要先上傳）
pub fn input_files(nodes: &Value) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for n in nodes.as_object().into_iter().flat_map(|o| o.values()) {
        let class = n["class_type"].as_str().unwrap_or("");
        if !matches!(class, "LoadImage" | "LoadImageMask") {
            continue;
        }
        if let Some(f) = n["inputs"]["image"].as_str().filter(|f| !f.is_empty()) {
            if !out.iter().any(|x| x == f) {
                out.push(f.to_string());
            }
        }
    }
    out
}

/// 這台電腦的記憶體（GB）；問不到回 0（記憶體防呆就不作用）
/// 本機的 N 卡，照 nvidia-smi 的順序編號（＝--cuda-device 的卡號；開多卡時會設 CUDA_DEVICE_ORDER=PCI_BUS_ID 讓 PyTorch 跟著這個順序）：
/// [{index, name, vram_mb}]。gpus＝gpu::detect 的結果
pub fn cuda_list(gpus: &[Value]) -> Value {
    Value::Array(
        gpus.iter()
            .filter(|g| g["vendor"] == "nvidia")
            .enumerate()
            .map(|(i, g)| json!({"index": i, "name": g["name"], "vram_mb": g["vram_mb"]}))
            .collect(),
    )
}

/// 網址裡的 port（http://127.0.0.1:8209 → 8209）
pub fn port_of(url: &str) -> Option<u16> {
    let host = url.split("://").nth(1).unwrap_or(url).split('/').next()?;
    host.rsplit_once(':')?.1.parse().ok()
}

/// 本機第 gpu 張卡的 ComfyUI 用的 port：主節點＋20＋卡號（＋10 會撞到 llama-server 的 8199；8189～8199 生圖站自己在用）
pub fn local_port(main: u16, gpu: u32) -> u16 {
    main.saturating_add(20).saturating_add(gpu as u16)
}

pub fn ram_gb() -> f64 {
    crate::setup::ram_total().map(|b| b as f64 / (1u64 << 30) as f64).unwrap_or(0.0)
}

// ---------------------------------------------------------------- 池子：主節點＋設定裡的其他節點

use crate::comfy::{Comfy, NodeInfo};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct Pool {
    start: bool,
    main: Arc<Comfy>,
    db: Arc<crate::db::Db>,
    hub: crate::hub::Hub,
    data: PathBuf,
    input: PathBuf,
    output: PathBuf,
    cfg: Mutex<Cfg>,
    others: Mutex<Vec<Arc<Comfy>>>,
    /// 工作 → 節點（重開站後不見；找不到時看資料庫 params 的 _node）
    by_job: Mutex<HashMap<String, String>>,
    /// 節點 → 上一個派給它的家族
    last: Mutex<HashMap<String, (String, bool)>>,
}

fn lk<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Pool {
    /// data＝放 nodes.json 的資料夾；input／output＝本機 ComfyUI 的資料夾（遠端節點上傳輸入圖、抓回成品用）
    pub fn new(main: Arc<Comfy>, db: Arc<crate::db::Db>, hub: crate::hub::Hub, data: &Path, input: &Path, output: &Path) -> Pool {
        Pool::build(main, db, hub, data, input, output, true)
    }

    /// start＝要不要開每台的 websocket（測試不開，用 set_live_for_test 設狀態）
    pub fn build(main: Arc<Comfy>, db: Arc<crate::db::Db>, hub: crate::hub::Hub, data: &Path, input: &Path, output: &Path, start: bool) -> Pool {
        let p = Pool {
            start,
            main,
            db,
            hub,
            data: data.to_path_buf(),
            input: input.to_path_buf(),
            output: output.to_path_buf(),
            cfg: Mutex::new(Cfg::default()),
            others: Mutex::new(vec![]),
            by_job: Mutex::new(HashMap::new()),
            last: Mutex::new(HashMap::new()),
        };
        p.apply(Cfg::load(data));
        p
    }

    /// 換設定：舊的非主節點全部停掉、照新設定重建（要在 tokio 裡叫：會啟動 websocket）
    fn apply(&self, cfg: Cfg) {
        for c in lk(&self.others).drain(..) {
            c.close();
        }
        let mut list = vec![];
        for n in cfg.nodes.iter().filter(|n| n.enabled && !n.url.is_empty()) {
            let remote = (n.kind == "remote").then(|| (self.input.clone(), self.output.clone()));
            let info = NodeInfo { id: n.id.clone(), tag: tag_of(&n.id).unwrap_or_else(|| n.id.clone()), remote };
            let c = Comfy::new_node(&n.url, self.db.clone(), self.hub.clone(), Some(info), n.token.as_deref());
            if self.start && tokio::runtime::Handle::try_current().is_ok() {
                c.start();
            }
            list.push(c);
        }
        *lk(&self.others) = list;
        *lk(&self.cfg) = cfg;
    }

    /// 存設定並套用
    pub fn reload(&self, cfg: Cfg) -> Result<(), String> {
        cfg.save(&self.data)?;
        self.apply(cfg);
        Ok(())
    }

    pub fn cfg(&self) -> Cfg {
        lk(&self.cfg).clone()
    }

    /// 主節點在最前面
    pub fn all(&self) -> Vec<Arc<Comfy>> {
        std::iter::once(self.main.clone()).chain(lk(&self.others).iter().cloned()).collect()
    }

    pub fn get(&self, id: &str) -> Option<Arc<Comfy>> {
        self.all().into_iter().find(|c| c.node_id() == id)
    }

    /// 只有主節點（沒加任何節點）＝跟以前一模一樣
    pub fn single(&self) -> bool {
        lk(&self.others).is_empty()
    }

    /// 派給這台、還沒結束的件數（連續送很多張時 ComfyUI 的佇列數還沒回報，只看它會全部擠到同一台）。
    /// 順手把已經結束的從 by_job 拿掉
    fn open_count(&self, id: &str) -> i64 {
        let mut map = lk(&self.by_job);
        let mut n = 0;
        map.retain(|pid, node| {
            let open = match self.db.job(pid) {
                Ok(Some(j)) => matches!(j["status"].as_str(), Some("queued" | "running")),
                Ok(None) => true, // 剛送出、還沒登記
                Err(_) => false,
            };
            if open && node == id {
                n += 1;
            }
            open
        });
        n
    }

    pub fn loads(&self) -> Vec<Load> {
        let last = lk(&self.last).clone();
        self.all()
            .iter()
            .map(|c| {
                let l = c.live();
                let id = c.node_id().to_string();
                let queue = l.queue_remaining.max(self.open_count(&id));
                Load { family: last.get(&id).map(|x| x.0.clone()).unwrap_or_default(), id, online: l.online, queue }
            })
            .collect()
    }

    /// 正在跑（或還排著）大模型的節點
    fn busy_heavy(&self) -> Vec<String> {
        let last = lk(&self.last).clone();
        self.all()
            .iter()
            .filter(|c| c.live().queue_remaining > 0 && last.get(c.node_id()).is_some_and(|x| x.1))
            .map(|c| c.node_id().to_string())
            .collect()
    }

    /// 挑一台（缺模型的排除重挑）並送出 → (prompt_id, 節點 id)。只有主節點時直接送主節點（跟以前一樣）
    pub async fn submit(&self, nodes: &Value, extra: &Value, family: &str, manual: Option<&str>, video: bool) -> Result<(String, String), String> {
        if self.single() {
            let pid = self.main.submit(nodes, extra).await.map_err(|e| e.0)?;
            return Ok((pid, MAIN.into()));
        }
        let cfg = self.cfg();
        let ram = ram_gb();
        let mut skip: Vec<String> = vec![];
        let mut why: Vec<String> = vec![];
        loop {
            let loads: Vec<Load> = self.loads().into_iter().filter(|l| !skip.contains(&l.id)).collect();
            let id = match pick(&cfg, &loads, family, manual, video, ram, &self.busy_heavy()) {
                Ok(id) => id,
                Err(e) if why.is_empty() => return Err(e),
                Err(_) => return Err(format!("每個節點都缺檔：{}", why.join("；"))),
            };
            let c = self.get(&id).ok_or("節點不見了")?;
            // 主節點照舊不先查（ComfyUI 驗證會講）；其他節點缺檔就換一台
            if id != MAIN {
                if let Ok(miss) = c.missing_models(nodes).await {
                    if !miss.is_empty() {
                        why.push(format!("「{id}」沒有 {}", miss.join("、")));
                        skip.push(id);
                        continue;
                    }
                }
            }
            let pid = c.submit(nodes, extra).await.map_err(|e| e.0)?;
            lk(&self.by_job).insert(pid.clone(), id.clone());
            lk(&self.last).insert(id.clone(), (family.to_string(), heavy(family, video)));
            return Ok((pid, id));
        }
    }

    /// 這件工作在哪台（記憶體裡沒有就看資料庫記的 _node；都沒有＝主節點）
    pub fn for_job(&self, pid: &str) -> Arc<Comfy> {
        let id = lk(&self.by_job).get(pid).cloned().or_else(|| {
            self.db.job(pid).ok().flatten().map(|j| crate::comfy::owner_of(&j["params"]).to_string())
        });
        id.and_then(|i| self.get(&i)).unwrap_or_else(|| self.main.clone())
    }

    /// 取消這幾件：正在跑的對它那台 /interrupt，還在排的從它那台佇列拿掉並標取消
    pub async fn cancel(&self, pids: &[String]) {
        for pid in pids {
            let c = self.for_job(pid);
            if c.live().running.as_deref() == Some(pid.as_str()) {
                let _ = c.post("/interrupt", &json!({})).await;
            } else {
                let _ = c.post("/queue", &json!({"delete": [pid]})).await;
                let _ = self.db.set_job(pid, "cancelled", None);
                self.hub.send(json!({"type": "finish", "job": pid, "status": "cancelled"}));
            }
        }
    }

    /// 每台都送（全部停止、清佇列）；回主節點的結果
    pub async fn broadcast(&self, path: &str, payload: &Value) -> Result<Value, crate::comfy::ComfyError> {
        let others = lk(&self.others).clone();
        for c in others {
            let _ = c.post(path, payload).await;
        }
        self.main.post(path, payload).await
    }

    /// 送出後補收（跑太快、websocket 比登記早到）：找對的那台
    pub async fn sync_job(&self, pid: &str) {
        self.for_job(pid).sync_job(pid).await;
    }

    /// 網頁用：每台的狀態
    pub fn status(&self) -> Value {
        let cfg = self.cfg();
        let rows: Vec<Value> = self
            .all()
            .iter()
            .map(|c| {
                let l = c.live();
                let id = c.node_id();
                let (name, kind) = match cfg.get(id) {
                    Some(n) => (n.name.clone(), n.kind.clone()),
                    None => ("主節點".to_string(), "main".to_string()),
                };
                json!({"id": id, "name": name, "kind": kind, "url": c.url(), "online": l.online, "queue": l.queue_remaining, "running": l.running})
            })
            .collect();
        Value::Array(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, kind: &str) -> NodeCfg {
        NodeCfg { id: id.into(), name: id.into(), url: format!("http://127.0.0.1/{id}"), kind: kind.into(), gpu: None, enabled: true, token: None, args: String::new() }
    }
    fn cfg() -> Cfg {
        Cfg { nodes: vec![node("gpu1", "local"), node("rabc123", "remote")], ..Cfg::default() }
    }
    fn load(id: &str, queue: i64, family: &str) -> Load {
        Load { id: id.into(), online: true, queue, family: family.into() }
    }
    fn loads() -> Vec<Load> {
        vec![load("main", 0, ""), load("gpu1", 0, ""), load("rabc123", 0, "")]
    }

    #[test]
    fn pick_manual_wins() {
        assert_eq!(pick(&cfg(), &loads(), "anima", Some("rabc123"), false, 128.0, &[]).unwrap(), "rabc123");
        assert_eq!(pick(&cfg(), &loads(), "anima", Some("auto"), false, 128.0, &[]).unwrap(), "main");
        assert!(pick(&cfg(), &loads(), "anima", Some("gone"), false, 128.0, &[]).unwrap_err().contains("gone"));
    }

    #[test]
    fn pick_respects_bind() {
        let mut c = cfg();
        c.bind.insert("qwen21".into(), vec!["gpu1".into()]);
        assert_eq!(pick(&c, &loads(), "qwen21", None, false, 128.0, &[]).unwrap(), "gpu1");
        assert_eq!(pick(&c, &loads(), "anima", None, false, 128.0, &[]).unwrap(), "main", "沒綁的家族哪台都行");
    }

    #[test]
    fn pick_skips_offline_and_disabled() {
        let mut l = loads();
        l[0].online = false;
        assert_eq!(pick(&cfg(), &l, "anima", None, false, 128.0, &[]).unwrap(), "gpu1");
        let mut c = cfg();
        c.nodes[0].enabled = false;
        assert_eq!(pick(&c, &l, "anima", None, false, 128.0, &[]).unwrap(), "rabc123");
        let off: Vec<Load> = loads().into_iter().map(|mut x| { x.online = false; x }).collect();
        assert!(pick(&cfg(), &off, "anima", None, false, 128.0, &[]).unwrap_err().contains("ComfyUI 沒有連線"));
    }

    #[test]
    fn pick_least_queue_then_same_family() {
        let l = vec![load("main", 2, "anima"), load("gpu1", 1, "moss"), load("rabc123", 1, "anima")];
        assert_eq!(pick(&cfg(), &l, "anima", None, false, 128.0, &[]).unwrap(), "rabc123", "同樣排 1 件，挑同家族的");
        let l = vec![load("main", 0, ""), load("gpu1", 0, "")];
        assert_eq!(pick(&cfg(), &l, "anima", None, false, 128.0, &[]).unwrap(), "main", "全部一樣，主節點優先");
    }

    #[test]
    fn pick_video_never_remote() {
        let l = vec![load("main", 5, ""), load("rabc123", 0, "")];
        assert_eq!(pick(&cfg(), &l, "video", None, true, 128.0, &[]).unwrap(), "main");
    }

    #[test]
    fn pick_ram_guard_keeps_heavy_on_one() {
        let busy = vec!["main".to_string()];
        // 32GB：主節點正在跑 Qwen，再一張 Qwen 只准排主節點或遠端
        let l = vec![load("main", 3, "qwen21"), load("gpu1", 0, "")];
        assert_eq!(pick(&cfg(), &l, "qwen21", None, false, 32.0, &busy).unwrap(), "main");
        // 關掉防呆、或記憶體夠大：照佇列
        let mut c = cfg();
        c.ram_guard = false;
        assert_eq!(pick(&c, &l, "qwen21", None, false, 32.0, &busy).unwrap(), "gpu1");
        assert_eq!(pick(&cfg(), &l, "qwen21", None, false, 128.0, &busy).unwrap(), "gpu1");
        // 小模型不受限
        assert_eq!(pick(&cfg(), &l, "anima", None, false, 32.0, &busy).unwrap(), "gpu1");
        // 只綁到別台：講清楚
        let mut c = cfg();
        c.bind.insert("qwen21".into(), vec!["gpu1".into()]);
        assert!(pick(&c, &l, "qwen21", None, false, 32.0, &busy).unwrap_err().contains("記憶體"));
    }

    #[test]
    fn tag_prefixes_only_non_main() {
        assert_eq!(tag_of("main"), None);
        assert_eq!(tag_of("gpu1").as_deref(), Some("g1"));
        assert_eq!(tag_of("rabc123").as_deref(), Some("rabc123"));
        let mut n = json!({"9": {"class_type": "SaveImage", "inputs": {"images": ["8", 0], "filename_prefix": "AnimaWeb/t2i"}},
                           "10": {"class_type": "SaveVideo", "inputs": {"filename_prefix": "AnimaWeb/video"}},
                           "3": {"class_type": "KSampler", "inputs": {"seed": 1}}});
        tag_prefixes(&mut n, "g1");
        assert_eq!(n["9"]["inputs"]["filename_prefix"], "AnimaWeb/t2i_g1");
        assert_eq!(n["10"]["inputs"]["filename_prefix"], "AnimaWeb/video_g1");
        assert!(n["3"]["inputs"].get("filename_prefix").is_none());
    }

    #[test]
    fn input_files_dedup() {
        let n = json!({"1": {"class_type": "LoadImage", "inputs": {"image": "a.png"}}, "2": {"class_type": "LoadImage", "inputs": {"image": "a.png"}},
                       "3": {"class_type": "LoadImageMask", "inputs": {"image": "m.png", "channel": "alpha"}}, "4": {"class_type": "CLIPTextEncode", "inputs": {"text": "x.png"}}});
        let mut f = input_files(&n);
        f.sort();
        assert_eq!(f, vec!["a.png", "m.png"]);
    }

    #[test]
    fn cfg_token_encrypted() {
        let d = std::env::temp_dir().join(format!("anima_nodes_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mut c = cfg();
        c.nodes[1].token = Some("TOKEN-SECRET-1".into());
        c.bind.insert("anima".into(), vec!["gpu1".into()]);
        c.save(&d).unwrap();
        let disk = std::fs::read_to_string(d.join(FILE)).unwrap();
        #[cfg(windows)]
        assert!(!disk.contains("TOKEN-SECRET-1"));
        assert_eq!(Cfg::load(&d), c);
        assert!(!c.view().to_string().contains("TOKEN-SECRET-1"));
        assert_eq!(c.view()["nodes"][1]["has_token"], true);
        // 壞檔、主節點、不認得的種類都不收
        let bad = Cfg::from_value(&json!({"nodes": [{"id": "main", "kind": "local"}, {"id": "x", "kind": "cloud"}, {"id": "gpu2", "kind": "local", "gpu": 2}]}));
        assert_eq!(bad.nodes.len(), 1);
        assert_eq!(bad.nodes[0].gpu, Some(2));
        assert!(bad.ram_guard, "沒寫＝開");
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod update_tests {
    use super::*;

    fn old() -> Cfg {
        Cfg {
            nodes: vec![NodeCfg { id: "r1a2b3c".into(), name: "書房".into(), url: "http://192.168.1.5:8188".into(), kind: "remote".into(), gpu: None, enabled: true, token: Some("secret".into()), args: String::new() }],
            ..Cfg::default()
        }
    }

    #[test]
    fn update_keeps_token_and_assigns_ids() {
        let b = json!({"nodes": [
            {"id": "r1a2b3c", "name": "書房", "kind": "remote", "url": "http://192.168.1.5:8188/", "enabled": true},
            {"kind": "local", "gpu": 1, "enabled": true, "args": "--lowvram"},
            {"kind": "remote", "name": "租的", "url": "https://x.example.com", "token": "t2"}
        ], "bind": {"qwen21": ["gpu1", "nope"], "anima": []}, "ram_guard": false});
        let (c, restart) = Cfg::update(&old(), &b, 8188).unwrap();
        assert_eq!(c.nodes[0].token.as_deref(), Some("secret"), "沒送 token＝保留舊的");
        assert_eq!(c.nodes[0].url, "http://192.168.1.5:8188");
        assert_eq!((c.nodes[1].id.as_str(), c.nodes[1].url.as_str(), c.nodes[1].name.as_str()), ("gpu1", "http://127.0.0.1:8209", "第 1 號卡"));
        assert!(c.nodes[2].id.starts_with('r') && c.nodes[2].id.len() == 7);
        assert_eq!(c.nodes[2].token.as_deref(), Some("t2"));
        assert_eq!(c.bind.get("qwen21"), Some(&vec!["gpu1".to_string()]), "不認識的節點從綁定拿掉");
        assert!(!c.bind.contains_key("anima"));
        assert!(!c.ram_guard);
        assert!(restart, "多了本機節點要重開 ComfyUI");
        // 只改遠端：不用重開
        let (_, restart) = Cfg::update(&c, &json!({"nodes": [c.nodes[0].view(), c.nodes[1].view()]}), 8188).unwrap();
        assert!(!restart);
        // 清掉 token
        let (c2, _) = Cfg::update(&old(), &json!({"nodes": [{"id": "r1a2b3c", "kind": "remote", "url": "http://192.168.1.5:8188", "clear_token": true}]}), 8188).unwrap();
        assert_eq!(c2.nodes[0].token, None);
        // 只送 bind：節點不動
        let (c3, restart) = Cfg::update(&old(), &json!({"bind": {"video": ["main"]}}), 8188).unwrap();
        assert_eq!(c3.nodes, old().nodes);
        assert!(!restart);
    }

    #[test]
    fn update_rejects_bad_nodes() {
        assert!(Cfg::update(&Cfg::default(), &json!({"nodes": [{"kind": "remote", "url": "192.168.1.5:8188"}]}), 8188).is_err(), "網址要 http(s)://");
        assert!(Cfg::update(&Cfg::default(), &json!({"nodes": [{"kind": "local"}]}), 8188).is_err(), "本機節點要卡號");
        assert!(Cfg::update(&Cfg::default(), &json!({"nodes": [{"kind": "local", "gpu": 1}, {"kind": "local", "gpu": 1}]}), 8188).is_err(), "同一張卡開兩次");
        assert!(Cfg::update(&Cfg::default(), &json!({"nodes": [{"kind": "local", "gpu": 1, "args": "--port 1"}]}), 8188).is_err(), "參數照全域那套擋");
        assert!(Cfg::update(&Cfg::default(), &json!({"nodes": [{"kind": "remote", "url": "http://127.0.0.1:8188"}]}), 8188).is_err(), "跟主節點同一台");
    }
}

#[cfg(test)]
mod gpu_tests {
    use super::*;

    #[test]
    fn cuda_list_numbers_nvidia_only() {
        let g = vec![
            json!({"vendor": "nvidia", "name": "RTX 3070", "vram_mb": 8192}),
            json!({"vendor": "amd", "name": "Radeon"}),
            json!({"vendor": "nvidia", "name": "RTX 4090", "vram_mb": 24564}),
        ];
        assert_eq!(cuda_list(&g), json!([{"index": 0, "name": "RTX 3070", "vram_mb": 8192}, {"index": 1, "name": "RTX 4090", "vram_mb": 24564}]));
        assert_eq!(cuda_list(&[]), json!([]));
    }

    #[test]
    fn local_port_from_url() {
        assert_eq!(port_of("http://127.0.0.1:8198"), Some(8198));
        assert_eq!(port_of("http://127.0.0.1:8198/"), Some(8198));
        assert_eq!(port_of("http://127.0.0.1"), None);
        assert_eq!(local_port(8188, 1), 8209, "＋10 會撞到 llama-server 的 8199");
    }
}

#[cfg(test)]
mod pool_tests {
    use super::*;
    use axum::routing::{get, post};
    use axum::{extract::State, Json, Router};

    type Log = Arc<Mutex<Vec<(String, Value)>>>;

    /// 假的 ComfyUI：/prompt 回 <名字>-<編號>，其他照收；收到的 (路徑, 內容) 記在 log
    async fn fake(name: &'static str) -> (String, Log) {
        let log: Log = Arc::new(Mutex::new(vec![]));
        let rec = |path: &'static str| {
            move |State(l): State<Log>, Json(v): Json<Value>| async move {
                let mut g = l.lock().unwrap();
                g.push((path.to_string(), v));
                let n = g.iter().filter(|x| x.0 == "/prompt").count();
                Json(if path == "/prompt" { json!({"prompt_id": format!("{name}-{n}"), "number": n, "node_errors": {}}) } else { json!({}) })
            }
        };
        let app = Router::new()
            .route("/prompt", post(rec("/prompt")))
            .route("/queue", post(rec("/queue")))
            .route("/interrupt", post(rec("/interrupt")))
            .route("/object_info", get(|| async { Json(json!({})) }))
            .route("/object_info/{n}", get(|| async { Json(json!({})) }))
            .route("/history/{id}", get(|| async { Json(json!({})) }))
            .with_state(log.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (url, log)
    }

    fn paths(log: &Log, p: &str) -> Vec<Value> {
        log.lock().unwrap().iter().filter(|x| x.0 == p).map(|x| x.1.clone()).collect()
    }

    async fn setup(name: &str, extra: bool) -> (Pool, Log, Log, PathBuf) {
        let d = std::env::temp_dir().join(format!("anima_pool_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let (u1, l1) = fake("main").await;
        let (u2, l2) = fake("g1").await;
        if extra {
            let c = Cfg { nodes: vec![NodeCfg { id: "gpu1".into(), name: "二號卡".into(), url: u2, kind: "local".into(), gpu: Some(1), enabled: true, token: None, args: String::new() }], ..Cfg::default() };
            c.save(&d).unwrap();
        }
        let db = Arc::new(crate::db::Db::open(&d.join("t.db")).unwrap());
        let hub = crate::hub::Hub::new();
        let main = Comfy::new(&u1, db.clone(), hub.clone());
        let pool = Pool::build(main, db, hub, &d, &d, &d, false);
        for c in pool.all() {
            c.set_live_for_test(true, 0);
        }
        (pool, l1, l2, d)
    }

    fn graph() -> Value {
        json!({"9": {"class_type": "SaveImage", "inputs": {"filename_prefix": "AnimaWeb/t2i"}}})
    }

    #[tokio::test]
    async fn single_node_unchanged() {
        let (pool, l1, _, d) = setup("single", false).await;
        assert!(pool.single());
        let (pid, node) = pool.submit(&graph(), &json!({}), "anima", None, false).await.unwrap();
        assert_eq!((pid.as_str(), node.as_str()), ("main-1", "main"));
        let sent = paths(&l1, "/prompt");
        assert_eq!(sent[0]["prompt"], graph(), "只有主節點：節點圖逐字不變（沒加標記）");
        assert_eq!(sent[0]["client_id"], crate::comfy::client_id_for(std::env::var("ANIMA_PORT").ok().as_deref()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn submit_spreads_by_queue() {
        let (pool, l1, l2, d) = setup("spread", true).await;
        let mut nodes = vec![];
        for _ in 0..4 {
            nodes.push(pool.submit(&graph(), &json!({}), "anima", None, false).await.unwrap().1);
        }
        assert_eq!(nodes, vec!["main", "gpu1", "main", "gpu1"], "連續送：ComfyUI 還沒回報佇列也要分開");
        assert_eq!(paths(&l1, "/prompt").len(), 2);
        let g1 = paths(&l2, "/prompt");
        assert_eq!(g1.len(), 2);
        assert_eq!(g1[0]["prompt"]["9"]["inputs"]["filename_prefix"], "AnimaWeb/t2i_g1", "非主節點加標記");
        assert!(g1[0]["client_id"].as_str().unwrap().ends_with("-g1"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn post_job_routes_to_owner() {
        let (pool, l1, l2, d) = setup("route", true).await;
        let a = pool.submit(&graph(), &json!({}), "anima", None, false).await.unwrap();
        let b = pool.submit(&graph(), &json!({}), "anima", None, false).await.unwrap();
        assert_eq!((a.1.as_str(), b.1.as_str()), ("main", "gpu1"));
        // b 正在 gpu1 跑：中斷要打 gpu1，不能打主節點
        pool.get("gpu1").unwrap().set_live_for_test(true, 1);
        pool.cancel(std::slice::from_ref(&b.0)).await;
        assert!(paths(&l1, "/interrupt").is_empty() && paths(&l1, "/queue").is_empty(), "主節點不該收到");
        assert_eq!(paths(&l2, "/queue")[0]["delete"], json!([b.0]), "還在排：從 gpu1 佇列拿掉");
        // 全部停止：每台都送
        pool.broadcast("/interrupt", &json!({})).await.unwrap();
        assert_eq!(paths(&l1, "/interrupt").len(), 1);
        assert_eq!(paths(&l2, "/interrupt").len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn for_job_falls_back_to_db() {
        let (pool, _, _, d) = setup("fallback", true).await;
        // 重開站：記憶體裡沒有，資料庫記著 _node
        pool.db.add_job("old-1", &json!({"_node": "gpu1"}), &json!({})).unwrap();
        assert_eq!(pool.for_job("old-1").node_id(), "gpu1");
        pool.db.add_job("old-2", &json!({}), &json!({})).unwrap();
        assert_eq!(pool.for_job("old-2").node_id(), "main");
        assert_eq!(pool.for_job("never").node_id(), "main");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn offline_node_skipped() {
        let (pool, _, l2, d) = setup("offline", true).await;
        pool.get("main").unwrap().set_live_for_test(false, 0);
        for _ in 0..2 {
            assert_eq!(pool.submit(&graph(), &json!({}), "anima", None, false).await.unwrap().1, "gpu1");
        }
        assert_eq!(paths(&l2, "/prompt").len(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }
}
