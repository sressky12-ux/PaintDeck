//! Durable, bounded diagnostic records for generation and assistant requests.
//! Only this module's marked directory is managed; media, models and settings are never collected.
mod archive;
mod context;
mod redact;
#[cfg(test)]
mod tests;
pub use archive::Export;
pub use context::{
    blocking, current, event, failure, hub_event, remember_secret, scope, snapshot, spawn,
};
use redact::{sha256, Redactor};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const OWNER: &str = "paintdeck-diagnostics-v1";
const MIB: u64 = 1024 * 1024;
const CORE_RESERVE: u64 = 32 * 1024;
const RUN_LIMIT: u64 = 100 * MIB;
const SNAPSHOT_LIMIT: usize = 4 * 1024 * 1024;
const EVENTS_LIMIT: u64 = 20 * MIB;
const ERROR_LIMIT: u64 = 4 * MIB;

pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
fn error(s: impl std::fmt::Display) -> io::Error {
    io::Error::other(s.to_string())
}
pub fn valid_id(s: &str) -> bool {
    s.starts_with("diag-")
        && s.len() < 80
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    pub success_days: u32,
    pub error_days: u32,
    pub max_mib: u32,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            success_days: 1,
            error_days: 3,
            max_mib: 500,
        }
    }
}
impl Policy {
    pub fn validate(&self) -> io::Result<()> {
        if !(1..=30).contains(&self.success_days)
            || !(1..=30).contains(&self.error_days)
            || !(50..=2048).contains(&self.max_mib)
        {
            return Err(error("保留天數需為 1～30 天，容量需為 50～2048 MiB"));
        }
        Ok(())
    }
    fn limit(&self) -> u64 {
        self.max_mib as u64 * MIB
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Record {
    pub owner: String,
    pub id: String,
    pub route: String,
    pub created: f64,
    pub updated: f64,
    pub ended: Option<f64>,
    pub status: String,
    pub active: bool,
    pub pending: Vec<String>,
    #[serde(default)]
    pub jobs: Vec<String>,
    pub http_complete: bool,
    pub failed: bool,
    pub reported: bool,
    pub interrupted: bool,
    pub cancelled: bool,
    #[serde(default)]
    pub terminal_confirmed: bool,
    pub truncated: u64,
    pub first_error: Option<String>,
    pub seq: u64,
    #[serde(default)]
    pub exports: u32,
}
impl Record {
    fn protected(&self) -> bool {
        self.active || !self.pending.is_empty() || self.exports > 0
    }
    fn finish(&mut self) {
        if self.active || !self.pending.is_empty() {
            return;
        }
        self.status = if self.failed {
            "error"
        } else if self.cancelled {
            "cancelled"
        } else if !self.terminal_confirmed && (self.interrupted || !self.http_complete) {
            "unknown"
        } else {
            "success"
        }
        .into();
        self.ended.get_or_insert_with(now);
    }
}

pub(super) struct Inner {
    pub policy: Policy,
    pub records: HashMap<String, Record>,
    pub used: u64,
    pub export_reserve: u64,
    pub warning: Option<String>,
}
pub struct Store {
    pub(super) root: PathBuf,
    pub(super) inner: Mutex<Inner>,
    pub(super) redactor: Redactor,
    ready: bool,
    logs: Vec<(String, PathBuf)>,
    log_positions: Mutex<HashMap<(String, String), u64>>,
}
pub struct Run {
    pub store: Arc<Store>,
    pub id: String,
}
impl Run {
    pub fn event(&self, kind: &str, data: Value) {
        self.store.event(&self.id, kind, data);
    }
    pub fn snapshot(&self, label: &str, data: &Value) {
        self.store.snapshot(&self.id, label, data);
    }
    pub fn fail(&self, message: &str) {
        self.store.fail(&self.id, message);
    }
    pub fn cancel(&self) {
        self.store.change(&self.id, |r| r.cancelled = true);
    }
    pub fn interrupted(&self) {
        self.store.change(&self.id, |r| {
            r.interrupted = true;
            r.status = "unknown".into();
        });
    }
    pub fn http_finished(&self, status: u16) {
        self.event("http_response", json!({"status":status}));
        self.store.change(&self.id, |r| {
            r.http_complete = true;
            if status >= 400 {
                r.failed = true;
            }
        });
    }
}
impl Drop for Run {
    fn drop(&mut self) {
        self.store.sample_logs(&self.id);
        self.store.change(&self.id, |r| {
            r.active = false;
            r.finish();
        });
        self.store.cleanup(false);
    }
}

impl Store {
    pub fn new(paths: &crate::paths::Paths) -> Arc<Self> {
        let root = paths.data.join("diagnostics");
        let mut warning = None;
        let mut records = HashMap::new();
        let mut policy = Policy::default();
        let setup = (|| -> io::Result<PathBuf> {
            fs::create_dir_all(&paths.data)?;
            if root.exists() {
                reject_link(&root)?;
                if fs::read_to_string(root.join("owner.txt"))?.trim() != OWNER {
                    return Err(error("診斷資料夾不屬於本功能"));
                }
            } else {
                fs::create_dir(&root)?;
                fs::write(root.join("owner.txt"), OWNER)?;
            }
            for d in ["runs", "exports"] {
                fs::create_dir_all(root.join(d))?;
                reject_link(&root.join(d))?;
            }
            let root = root.canonicalize()?;
            if root.join("policy.json").exists() {
                reject_link(&root.join("policy.json"))?;
                policy =
                    serde_json::from_slice(&fs::read(root.join("policy.json"))?).map_err(error)?;
                policy.validate()?;
            }
            for entry in fs::read_dir(root.join("runs"))? {
                let p = entry?.path();
                if reject_link(&p).is_err() || !p.is_dir() {
                    continue;
                }
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                if !valid_id(&name) || reject_link(&p.join("run.json")).is_err() {
                    continue;
                }
                let Ok(bytes) = read_small(&p.join("run.json"), 64 * 1024) else {
                    continue;
                };
                let Ok(mut record) = serde_json::from_slice::<Record>(&bytes) else {
                    continue;
                };
                if record.owner != OWNER || record.id != name {
                    continue;
                }
                if record.active || !record.pending.is_empty() {
                    record.active = false;
                    record.interrupted = true;
                    record.status = "unknown".into();
                    record.ended = Some(record.updated);
                }
                record.exports = 0;
                records.insert(record.id.clone(), record);
            }
            Ok(root)
        })();
        let mut ready = setup.is_ok();
        let root = match setup {
            Ok(p) => p,
            Err(e) => {
                warning = Some(format!("診斷紀錄無法保存：{e}"));
                root
            }
        };
        let used = if ready {
            match tree_bytes(&root) {
                Ok(n) => n,
                Err(e) => {
                    ready = false;
                    warning = Some(format!("診斷容量無法核對：{e}"));
                    0
                }
            }
        } else {
            0
        };
        let store = Arc::new(Self {
            root,
            inner: Mutex::new(Inner {
                policy,
                records,
                used,
                export_reserve: 0,
                warning,
            }),
            redactor: Redactor::new(paths),
            ready,
            logs: vec![
                ("comfy".into(), paths.data.join("logs/comfyui.log")),
                ("llama".into(), paths.pe_node.join("llama-server.log")),
            ],
            log_positions: Mutex::new(HashMap::new()),
        });
        if ready {
            let mut inner = store.inner.lock().unwrap_or_else(|e| e.into_inner());
            // ZIPs belong exclusively to this directory and have no consumers after process restart.
            if let Ok(entries) = fs::read_dir(store.root.join("exports")) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("diag-"))
                        && p.extension().is_some_and(|e| e == "zip")
                        && reject_link(&p).is_ok()
                        && p.is_file()
                    {
                        let _ = fs::remove_file(p);
                    }
                }
            }
            inner.used = tree_bytes(&store.root).unwrap_or(inner.used);
            let ids: Vec<_> = inner.records.keys().cloned().collect();
            for id in ids {
                let _ = store.persist(&mut inner, &id);
            }
            store.cleanup_locked(&mut inner, now(), false, 0);
        }
        store
    }

    pub fn begin(self: &Arc<Self>, route: &str, body: &Value) -> Option<Arc<Run>> {
        if !self.ready {
            return None;
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.cleanup_locked(&mut inner, now(), false, CORE_RESERVE * 3);
        if self.ensure_root().is_err()
            || inner.used + inner.export_reserve + Self::reserved(&inner) + CORE_RESERVE * 3
                > inner.policy.limit()
        {
            inner.warning = Some("診斷空間不足，這次無法完整保存紀錄".into());
            return None;
        }
        let id = format!(
            "diag-{}-{:016x}",
            (now() * 1000.0) as u64,
            rand::random::<u64>()
        );
        let dir = self.root.join("runs").join(&id);
        if let Err(e) = fs::create_dir(&dir) {
            inner.warning = Some(e.to_string());
            return None;
        }
        let t = now();
        inner.records.insert(
            id.clone(),
            Record {
                owner: OWNER.into(),
                id: id.clone(),
                route: route.into(),
                created: t,
                updated: t,
                ended: None,
                status: "running".into(),
                active: true,
                pending: vec![],
                jobs: vec![],
                http_complete: false,
                failed: false,
                reported: false,
                interrupted: false,
                cancelled: false,
                terminal_confirmed: false,
                truncated: 0,
                first_error: None,
                seq: 0,
                exports: 0,
            },
        );
        if let Err(e) = self.persist(&mut inner, &id) {
            inner.warning = Some(e.to_string());
            inner.records.remove(&id);
            return None;
        }
        drop(inner);
        let run = Arc::new(Run {
            store: self.clone(),
            id,
        });
        for (name, path) in &self.logs {
            let offset = path.metadata().map(|m| m.len()).unwrap_or(0);
            self.log_positions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert((run.id.clone(), name.clone()), offset);
        }
        run.snapshot("request", body);
        run.snapshot("environment", &json!({"site_version":crate::server::VERSION,"os":std::env::consts::OS,"arch":std::env::consts::ARCH,
            "schema_version":"1.0","process_id":std::process::id(),"timezone":"timestamps are UTC; UI uses local time",
            "coverage":{"request":true,"workflow_api":"when_submitted","workflow_ui":"unavailable_without_client_snapshot","native_gpu_driver":"unavailable","process_logs":"shared local stdout/stderr sampled during run; startup/error tails; external or cloud logs may be unavailable","images":"omitted","model_weights":"never_collected"}}));
        run.event("run_started", json!({"route":route}));
        self.startup_log(&run.id);
        Some(run)
    }

    /// ComfyUI 紀錄檔開頭那段（繪匣寫的 A 卡狀態行、PyTorch 版本、顯示卡、啟動警告）。sample_logs 只取這次操作期間新增的，
    /// 開機那段永遠收不到（2026-11-05 回報的診斷包就看不到 [繪匣] A 卡那行）
    fn startup_log(&self, id: &str) {
        use std::io::Read;
        let Some((name, path)) = self.logs.iter().find(|(n, _)| n == "comfy") else { return };
        if reject_link(path).is_err() {
            return;
        }
        let Ok(f) = fs::File::open(path) else { return };
        let mut bytes = vec![];
        if f.take(16 * 1024).read_to_end(&mut bytes).is_err() || bytes.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(&bytes);
        // 只留到 ComfyUI 開好（To see the GUI…）為止，後面是生圖的輸出，sample_logs 會另外收
        let end = text.find("To see the GUI").and_then(|i| text[i..].find('\n').map(|j| i + j + 1)).unwrap_or(text.len());
        self.event(id, "process_log_comfy_startup", json!({"source":name,"scope":"head of the shared local log (ComfyUI startup)","byte_start":0,"byte_end":end,"text":&text[..end]}));
    }

    /// Read only bytes appended during this operation; both native streams share these legacy files.
    pub fn sample_logs(&self, id: &str) {
        use std::io::{Read, Seek, SeekFrom};
        for (name, path) in &self.logs {
            if reject_link(path).is_err() {
                continue;
            }
            let Ok(mut f) = fs::File::open(path) else {
                continue;
            };
            let Ok(meta) = f.metadata() else { continue };
            let len = meta.len();
            let key = (id.to_string(), name.clone());
            let mut positions = self.log_positions.lock().unwrap_or_else(|e| e.into_inner());
            let start = *positions.entry(key.clone()).or_insert(len);
            let start = if len < start { 0 } else { start };
            if len == start {
                continue;
            }
            // Always bound reads; if more than 256 KiB arrived between samples, preserve the newest tail and mark the gap.
            let from = start.max(len.saturating_sub(256 * 1024));
            if f.seek(SeekFrom::Start(from)).is_err() {
                continue;
            }
            let mut bytes = vec![];
            if f.take(len - from).read_to_end(&mut bytes).is_err() {
                continue;
            }
            positions.insert(key, len);
            drop(positions);
            self.event(id,&format!("process_log_{name}"),json!({"source":name,"streams":"stdout_and_stderr_combined","scope":"shared local log; concurrent tasks may interleave","byte_start":from,"byte_end":len,"skipped_bytes":from-start,"text":String::from_utf8_lossy(&bytes)}));
            if from > start {
                self.change(id, |r| r.truncated += 1);
            }
        }
    }
    pub fn process_exit(self: &Arc<Self>, pid: u32, code: Option<i32>, tail: &str) {
        let data = json!({"process":"comfyui","pid":pid,"exit_code":code,"observed_exit":true,"log_scope":"shared local log last 64 KiB","log":tail});
        let ids = self.active_ids();
        if ids.is_empty() {
            if let Some(run) = self.begin("/process/comfy/exit", &data) {
                run.fail("ComfyUI process exited");
                run.http_finished(200);
            }
        } else {
            for id in ids {
                self.event(&id, "process_exit", data.clone());
            }
        }
    }
    pub fn active_ids(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .values()
            .filter(|r| r.active || !r.pending.is_empty())
            .map(|r| r.id.clone())
            .collect()
    }
    pub fn pending_jobs(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .values()
            .flat_map(|r| r.pending.clone())
            .collect()
    }
    pub fn link_job(&self, id: &str, job: &str) {
        self.change(id, |r| {
            if !r.jobs.iter().any(|p| p == job) {
                r.jobs.push(job.into());
            }
        });
    }
    pub fn bind_job(&self, id: &str, job: &str) {
        self.change(id, |r| {
            if !r.pending.iter().any(|j| j == job) {
                r.pending.push(job.into());
            }
            if !r.jobs.iter().any(|j| j == job) {
                r.jobs.push(job.into());
            }
        });
        self.event(id, "comfy_accepted", json!({"prompt_id":job}));
    }
    pub fn job_event(&self, job: &str, kind: &str, data: &Value) {
        let id = self.job_id(job);
        let Some(id) = id else { return };
        self.event(&id, kind, data.clone());
        if kind == "execution_error" {
            self.fail(&id, &data.to_string());
        }
        if matches!(
            kind,
            "execution_success" | "execution_error" | "execution_interrupted"
        ) {
            self.sample_logs(&id);
            self.change(&id, |r| {
                r.terminal_confirmed = true;
                r.pending.retain(|p| p != job);
                if kind == "execution_interrupted" {
                    r.cancelled = true;
                }
                r.finish();
            });
        }
    }
    pub fn job_missing(&self, job: &str) {
        let Some(id) = self.job_id(job) else { return };
        self.event(&id,"job_unavailable",json!({"prompt_id":job,"evidence":"absent from reachable queue and history in three consecutive checks","outcome":"unknown"}));
        self.change(&id, |r| {
            r.pending.retain(|p| p != job);
            r.interrupted = true;
            r.finish();
        });
    }
    pub fn disconnected(&self) {
        for id in self.active_ids() {
            self.change(&id, |r| {
                if !r.pending.is_empty() {
                    r.interrupted = true;
                    r.status = "unknown".into();
                }
            });
            self.event(&id, "connection_lost", json!({"source":"comfy_websocket","outcome":"unknown","note":"disconnect is not proof of process crash"}));
        }
    }

    pub fn job_id(&self, job: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .values()
            .find(|r| r.jobs.iter().any(|p| p == job))
            .map(|r| r.id.clone())
    }

    pub fn event(&self, id: &str, kind: &str, data: Value) {
        let data = self.redactor.value(&data);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(record) = inner.records.get_mut(id) else {
            return;
        };
        record.seq += 1;
        record.updated = now();
        let value = json!({"schema_version":"1.0","event_id":format!("ev-{}",record.seq),"run_id":id,"timestamp":record.updated,"type":kind,"data":data});
        let mut bytes = serde_json::to_vec(&value).unwrap_or_default();
        bytes.push(b'\n');
        let critical = kind.contains("error")
            || kind.contains("fail")
            || kind.contains("exit")
            || kind.contains("load")
            || kind == "comfy_accepted";
        let limit = if critical { ERROR_LIMIT } else { EVENTS_LIMIT };
        let file = if critical {
            "critical.jsonl"
        } else if kind == "resources" {
            "metrics.jsonl"
        } else if kind == "process_log_comfy" || kind == "process_log_comfy_startup" {
            "comfy_stdout_stderr.jsonl"
        } else if kind == "process_log_llama" {
            "llama_stdout_stderr.jsonl"
        } else {
            "events.jsonl"
        };
        let path = self.root.join("runs").join(id).join(file);
        let wrote = (|| -> io::Result<()> {
            self.check_dir(id)?;
            if path.exists() {
                reject_link(&path)?;
            }
            let len = path.metadata().map(|m| m.len()).unwrap_or(0);
            if len + bytes.len() as u64 > limit {
                return Err(error("diagnostic stream limit"));
            }
            self.room(&mut inner, id, bytes.len() as u64)?;
            let mut f = OpenOptions::new().create(true).append(true).open(path)?;
            f.write_all(&bytes)?;
            if critical {
                f.sync_data()?;
            }
            inner.used += bytes.len() as u64;
            Ok(())
        })();
        if let Err(e) = wrote {
            self.lost(&mut inner, id, e);
        }
        if let Err(e) = self.persist(&mut inner, id) {
            inner.warning = Some(e.to_string());
        }
    }

    pub fn snapshot(&self, id: &str, label: &str, data: &Value) {
        let _ = self.try_snapshot(id, label, data);
    }
    fn try_snapshot(&self, id: &str, label: &str, data: &Value) -> io::Result<()> {
        let clean = self.redactor.value(data);
        let mut bytes = serde_json::to_vec_pretty(&clean).unwrap_or_default();
        let oversized = bytes.len() > SNAPSHOT_LIMIT;
        if oversized {
            bytes = serde_json::to_vec_pretty(&json!({"status":"truncated","reason":"snapshot_limit","original_bytes":bytes.len(),"sha256":sha256(&bytes)})).unwrap();
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(record) = inner.records.get_mut(id) else {
            return Err(error("找不到診斷紀錄"));
        };
        record.seq += 1;
        record.updated = now();
        if oversized {
            record.truncated += 1;
        }
        let label: String = label
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(48)
            .collect();
        let name = format!("{:05}-{label}.json", record.seq);
        let result = (|| -> io::Result<()> {
            self.check_dir(id)?;
            if record_file_count(&self.root.join("runs").join(id))? >= 512 {
                return Err(error("diagnostic file count limit"));
            }
            self.room(&mut inner, id, bytes.len() as u64)?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(self.root.join("runs").join(id).join(name))?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            inner.used += bytes.len() as u64;
            Ok(())
        })();
        if let Err(e) = result {
            let message = e.to_string();
            self.lost(&mut inner, id, e);
            let _ = self.persist(&mut inner, id);
            return Err(error(message));
        }
        self.persist(&mut inner, id)?;
        if oversized {
            return Err(error("診斷快照超過單檔限制"));
        }
        Ok(())
    }

    pub fn fail(&self, id: &str, message: &str) {
        let message = self.redactor.text(message);
        self.event(id, "error", json!({"message":message}));
        self.change(id, |r| {
            r.failed = true;
            if r.first_error.is_none() {
                r.first_error = Some(message.chars().take(600).collect());
            }
            if !r.active && r.pending.is_empty() {
                r.finish();
            }
        });
    }

    fn change(&self, id: &str, f: impl FnOnce(&mut Record)) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = inner.records.get_mut(id) {
            f(r);
            r.updated = now();
        }
        if let Err(e) = self.persist(&mut inner, id) {
            inner.warning = Some(e.to_string());
        }
    }
    fn lost(&self, inner: &mut Inner, id: &str, e: io::Error) {
        if let Some(r) = inner.records.get_mut(id) {
            r.truncated += 1;
        }
        inner.warning = Some(format!("部分診斷資料未保存：{e}"));
        inner.used = tree_bytes(&self.root).unwrap_or(inner.used);
    }
    fn reserved(inner: &Inner) -> u64 {
        inner
            .records
            .values()
            .filter(|r| r.active || !r.pending.is_empty())
            .count() as u64
            * CORE_RESERVE
    }
    fn room(&self, inner: &mut Inner, id: &str, amount: u64) -> io::Result<()> {
        if tree_bytes(&self.root.join("runs").join(id))? + amount > RUN_LIMIT {
            return Err(error("single run quota"));
        }
        if inner.used + Self::reserved(inner) + inner.export_reserve + amount > inner.policy.limit()
        {
            // The record being updated may already be closed (manual report/export preparation).
            if let Some(r) = inner.records.get_mut(id) {
                r.exports += 1;
            }
            self.cleanup_locked(inner, now(), false, amount);
            if let Some(r) = inner.records.get_mut(id) {
                r.exports -= 1;
            }
        }
        if inner.used + Self::reserved(inner) + inner.export_reserve + amount > inner.policy.limit()
        {
            return Err(error("diagnostic quota"));
        }
        Ok(())
    }
    pub(super) fn persist(&self, inner: &mut Inner, id: &str) -> io::Result<()> {
        self.check_dir(id)?;
        let Some(r) = inner.records.get(id) else {
            return Ok(());
        };
        let bytes = serde_json::to_vec_pretty(r).map_err(error)?;
        if bytes.len() > CORE_RESERVE as usize / 2 {
            return Err(error("core metadata limit"));
        }
        let path = self.root.join("runs").join(id).join("run.json");
        let temp = path.with_extension("tmp");
        if path.exists() {
            reject_link(&path)?;
        }
        if temp.exists() {
            reject_link(&temp)?;
        }
        let old = path.metadata().map(|m| m.len()).unwrap_or(0);
        let protected_reserve =
            Self::reserved(inner).saturating_sub(if r.active || !r.pending.is_empty() {
                CORE_RESERVE
            } else {
                0
            });
        if inner.used + protected_reserve + inner.export_reserve + bytes.len() as u64
            > inner.policy.limit()
        {
            return Err(error("core metadata quota"));
        }
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(temp, path)?;
        inner.used = inner.used.saturating_sub(old) + bytes.len() as u64;
        Ok(())
    }
    pub(super) fn ensure_root(&self) -> io::Result<()> {
        if !self.ready {
            return Err(error("diagnostics unavailable"));
        }
        reject_link(&self.root)?;
        if self.root.canonicalize()? != self.root {
            return Err(error("diagnostic root changed"));
        }
        for d in ["runs", "exports"] {
            reject_link(&self.root.join(d))?;
        }
        Ok(())
    }
    pub(super) fn check_dir(&self, id: &str) -> io::Result<PathBuf> {
        self.ensure_root()?;
        if !valid_id(id) {
            return Err(error("invalid diagnostic id"));
        }
        let p = self.root.join("runs").join(id);
        reject_link(&p)?;
        if !p.is_dir() || !p.canonicalize()?.starts_with(&self.root) {
            return Err(error("invalid diagnostic directory"));
        }
        Ok(p)
    }

    pub fn status(&self) -> Value {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut records: Vec<_> = inner.records.values().cloned().collect();
        records.sort_by(|a, b| b.created.total_cmp(&a.created));
        json!({"available":self.ready,"policy":inner.policy,"bytes":inner.used,"reserved_bytes":Self::reserved(&inner)+inner.export_reserve,
            "warning":inner.warning,"records":records.into_iter().take(100).collect::<Vec<_>>(),"scope":"data/diagnostics only; downloaded files outside this folder are user managed"})
    }
    pub fn report(&self, id: &str, note: &Value) -> io::Result<()> {
        if !self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .contains_key(id)
        {
            return Err(error("找不到診斷紀錄"));
        }
        self.try_snapshot(id, "user_report", note)?;
        self.change(id, |r| {
            r.reported = true;
            if !r.active && r.pending.is_empty() {
                r.ended = Some(now());
            }
        });
        Ok(())
    }
    pub fn preferences(&self, policy: Policy) -> io::Result<()> {
        policy.validate()?;
        self.ensure_root()?;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // Do not accept a lower cap while protected data already exceeds it.
        let protected = inner
            .records
            .values()
            .filter(|r| r.protected())
            .filter_map(|r| self.check_dir(&r.id).ok())
            .map(|p| tree_bytes(&p).unwrap_or(0))
            .sum::<u64>();
        if protected + Self::reserved(&inner) + inner.export_reserve > policy.limit() {
            return Err(error("執行中或匯出中的診斷資料超過新額度，請稍後再調小"));
        }
        let path = self.root.join("policy.json");
        if path.exists() {
            reject_link(&path)?;
        }
        let data = serde_json::to_vec_pretty(&policy).map_err(error)?;
        let previous = inner.policy.clone();
        inner.policy = policy;
        let result = (|| -> io::Result<()> {
            self.cleanup_locked(&mut inner, now(), false, data.len() as u64);
            if inner.used + Self::reserved(&inner) + inner.export_reserve + data.len() as u64
                > inner.policy.limit()
            {
                return Err(error("無法套用容量額度"));
            }
            let temp = path.with_extension("tmp");
            if temp.exists() {
                reject_link(&temp)?;
            }
            let mut f = OpenOptions::new()
                .write(true)
                .truncate(true)
                .create(true)
                .open(&temp)?;
            f.write_all(&data)?;
            f.sync_all()?;
            drop(f);
            fs::rename(&temp, &path)?;
            Ok(())
        })();
        if result.is_err() {
            inner.policy = previous;
        }
        inner.used = tree_bytes(&self.root).unwrap_or(inner.used + data.len() as u64);
        result
    }

    pub fn cleanup(&self, all: bool) -> Value {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = inner.used;
        self.cleanup_locked(&mut inner, now(), all, 0);
        json!({"freed_bytes":before.saturating_sub(inner.used),"bytes":inner.used,"warning":inner.warning})
    }
    fn cleanup_locked(&self, inner: &mut Inner, t: f64, all: bool, incoming: u64) {
        if self.ensure_root().is_err() {
            return;
        }
        let pressure = inner.used + Self::reserved(inner) + inner.export_reserve + incoming
            > inner.policy.limit();
        let target = inner.policy.limit() * 4 / 5;
        let mut candidates: Vec<_> = inner
            .records
            .values()
            .filter(|r| !r.protected())
            .cloned()
            .collect();
        candidates.sort_by(|a, b| {
            let expired = |r: &Record| {
                t - r.ended.unwrap_or(r.updated)
                    >= 86400.0
                        * if r.status == "success" && !r.reported {
                            inner.policy.success_days
                        } else {
                            inner.policy.error_days
                        } as f64
            };
            (!expired(a), a.status != "success" || a.reported)
                .cmp(&(!expired(b), b.status != "success" || b.reported))
                .then(a.created.total_cmp(&b.created))
        });
        for r in candidates {
            let days = if r.status == "success" && !r.reported {
                inner.policy.success_days
            } else {
                inner.policy.error_days
            };
            let expired = t - r.ended.unwrap_or(r.updated) >= days as f64 * 86400.0;
            let need = pressure
                && inner.used + Self::reserved(inner) + inner.export_reserve + incoming > target;
            if !all && !expired && !need {
                continue;
            }
            let result = self.check_dir(&r.id).and_then(|p| {
                let disk: Record =
                    serde_json::from_slice(&read_small(&p.join("run.json"), 64 * 1024)?)
                        .map_err(error)?;
                if disk.owner != OWNER || disk.id != r.id {
                    return Err(error("unowned diagnostic directory"));
                }
                remove_owned_tree(&self.root, &p)
            });
            match result {
                Ok(()) => {
                    inner.records.remove(&r.id);
                    self.log_positions
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .retain(|(id, _), _| id != &r.id);
                }
                Err(e) => inner.warning = Some(format!("部分舊日誌無法清理：{e}")),
            }
            inner.used = tree_bytes(&self.root).unwrap_or(inner.used);
        }
    }
}

pub(super) fn reject_link(p: &Path) -> io::Result<()> {
    let m = fs::symlink_metadata(p)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return Err(error("reparse point rejected"));
        }
    }
    if m.file_type().is_symlink() {
        return Err(error("symbolic link rejected"));
    }
    Ok(())
}
pub(super) fn read_small(p: &Path, limit: u64) -> io::Result<Vec<u8>> {
    use std::io::Read;
    reject_link(p)?;
    if p.metadata()?.len() > limit {
        return Err(error("file limit"));
    }
    let mut bytes = vec![];
    fs::File::open(p)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(error("file grew beyond limit"));
    }
    Ok(bytes)
}
pub(super) fn safe_files(p: &Path) -> io::Result<Vec<PathBuf>> {
    reject_link(p)?;
    let mut pending = vec![(p.to_path_buf(), 0)];
    let mut files = vec![];
    while let Some((dir, depth)) = pending.pop() {
        if depth > 8 || files.len() + pending.len() > 100_000 {
            return Err(error("diagnostic tree limit"));
        }
        reject_link(&dir)?;
        for entry in fs::read_dir(dir)? {
            let p = entry?.path();
            reject_link(&p)?;
            if p.is_dir() {
                pending.push((p, depth + 1));
            } else if p.is_file() {
                files.push(p);
            }
        }
    }
    Ok(files)
}
fn tree_bytes(p: &Path) -> io::Result<u64> {
    safe_files(p)?
        .iter()
        .try_fold(0u64, |n, p| Ok(n.saturating_add(p.metadata()?.len())))
}
fn record_file_count(p: &Path) -> io::Result<usize> {
    Ok(safe_files(p)?.len())
}
fn remove_owned_tree(root: &Path, p: &Path) -> io::Result<()> {
    reject_link(root)?;
    reject_link(p)?;
    if !p.canonicalize()?.starts_with(root) || p == root {
        return Err(error("unsafe removal target"));
    }
    let mut files = safe_files(p)?;
    files.sort_by_key(|p| p.file_name().is_some_and(|n| n == "run.json"));
    let mut dirs = vec![p.to_path_buf()];
    let mut cursor = 0;
    while cursor < dirs.len() {
        for entry in fs::read_dir(&dirs[cursor])? {
            let child = entry?.path();
            reject_link(&child)?;
            if child.is_dir() {
                dirs.push(child);
            }
        }
        cursor += 1;
    }
    for file in &files {
        let mut parent = file.parent();
        while let Some(d) = parent.filter(|d| d.starts_with(p)) {
            if !dirs.iter().any(|x| x == d) {
                dirs.push(d.into());
            }
            if d == p {
                break;
            }
            parent = d.parent();
        }
    }
    for file in files {
        reject_link(&file)?;
        if !file.canonicalize()?.starts_with(root) {
            return Err(error("removal escaped root"));
        }
        fs::remove_file(file)?;
    }
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for dir in dirs {
        reject_link(&dir)?;
        fs::remove_dir(dir)?;
    }
    Ok(())
}
