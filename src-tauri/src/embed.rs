//! 外掛寫法挑段用的「意思座標」（2026-10-29）：Qwen3-Embedding-0.6B 只用 CPU，自己開一顆 llama-server。
//! 使用者原話：「有沒有辦法 能讓他真正理解意思。」「肯定要吧 喂」
//!
//! 第一百六十三筆實驗（23 題有標準答案）：關鍵字挑段第一名對 17 題，意思座標（CPU）21 題，一題 0.11 秒，不占顯卡。
//! - 跟 AI 改寫的 llama-server（8199，同一時間只能一顆）分開：port 開的時候找空的；閒置 10 分鐘自己關；
//!   生圖站結束（含當掉）時由 Job Object 一起結束
//! - 每段的座標存在 skill 資料夾的 .vectors.json（照段落內容的雜湊：skill 改了只重算改到的段；換模型整份重算）
//! - 沒有模型、開不起來、算失敗：回 None，呼叫的地方退回 skills.rs 的關鍵字挑段，不擋 AI 改寫和助手
use crate::pe::{win, Core};
use crate::skills::{self, Skill};
use base64::Engine;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 查詢前面加的任務說明（Qwen3-Embedding 的用法：查詢帶 Instruct，段落不帶；跟實驗用的一字不差）
const TASK: &str = "Given a drawing request, retrieve the sections of a prompt-writing guide that help write its prompt";
/// Qwen3-Embedding 取最後一個 token 當座標（--pooling last），輸入後面要接這個
const EOT: &str = "<|endoftext|>";
const VEC_FILE: &str = ".vectors.json";
/// 閒置多久自己關（秒）
const IDLE_SECS: i64 = 600;
/// 開不起來之後多久內不再試（秒），免得每次改寫都卡在等它
const RETRY_SECS: i64 = 300;
const START_TIMEOUT: Duration = Duration::from_secs(40);
/// 一次送幾段
const BATCH: usize = 8;
/// 缺座標的段超過這麼多：這次先用關鍵字，背景慢慢算（2026-10-29 實測真的只用 CPU 時 35 段要 136 秒、一段約 4 秒，
/// 不能讓改寫等；裝好、綁好時已經在背景算過，這裡多半只會碰到 skill 改了幾段）
const INLINE_MAX: usize = 3;

struct Proc {
    child: std::process::Child,
    _job: Option<crate::studio::job::Job>,
    url: String,
    model: String,
}

pub struct Embedder {
    core: Option<Arc<Core>>,
    proc: tokio::sync::Mutex<Option<Proc>>,
    last_used: AtomicI64,
    failed_at: AtomicI64,
    /// 正在背景算座標的 skill 資料夾
    indexing: Mutex<HashSet<PathBuf>>,
    http: reqwest::Client,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn norm(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
    v
}

fn dot(a: &[f32], b: &[f32]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (*x as f64) * (*y as f64)).sum()
}

/// 一段內容的雜湊（座標快取的鍵）
fn key(text: &str) -> String {
    let d = Sha256::digest(text.as_bytes());
    d.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

/// 查詢的寫法（帶任務說明）
pub fn query_text(q: &str) -> String {
    format!("Instruct: {TASK}\nQuery:{}{EOT}", q.trim())
}

fn doc_input(text: &str) -> String {
    format!("{text}{EOT}")
}

fn encode(v: &[f32]) -> String {
    let bytes: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn decode(s: &str) -> Option<Vec<f32>> {
    let b = base64::engine::general_purpose::STANDARD.decode(s).ok()?;
    (b.len() % 4 == 0 && !b.is_empty()).then(|| b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

/// 讀 skill 資料夾的座標快取（模型不同就當作沒有）
pub fn load_cache(dir: &Path, model: &str) -> HashMap<String, Vec<f32>> {
    let Ok(raw) = std::fs::read(dir.join(VEC_FILE)) else { return HashMap::new() };
    let Ok(v) = serde_json::from_slice::<Value>(&raw) else { return HashMap::new() };
    if v["model"].as_str() != Some(model) {
        return HashMap::new();
    }
    v["vectors"].as_object().into_iter().flatten().filter_map(|(k, s)| Some((k.clone(), decode(s.as_str()?)?))).collect()
}

/// 寫回快取：只留現在這份 skill 還有的段
pub fn save_cache(dir: &Path, model: &str, all: &HashMap<String, Vec<f32>>, keep: &[String]) -> Result<(), String> {
    let vectors: Map<String, Value> = keep.iter().filter_map(|k| all.get(k).map(|v| (k.clone(), json!(encode(v))))).collect();
    let body = json!({"model": model, "vectors": vectors});
    let tmp = dir.join(format!("{VEC_FILE}.tmp"));
    std::fs::write(&tmp, body.to_string()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join(VEC_FILE)).map_err(|e| e.to_string())
}

/// 照座標排分數（高到低）：每段跟查詢的內積（都正規化過＝餘弦相似度）
pub fn rank(query: &[f32], docs: &[Vec<f32>]) -> Vec<(f64, usize)> {
    let mut out: Vec<(f64, usize)> = docs.iter().enumerate().map(|(i, d)| (dot(query, d), i)).collect();
    out.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    out
}

impl Embedder {
    pub fn new(core: Option<Arc<Core>>) -> Arc<Embedder> {
        let http = reqwest::Client::builder()
            .no_proxy() // llama-server 在本機
            .timeout(Duration::from_secs(180))
            .build()
            .expect("建立 HTTP client");
        Arc::new(Embedder {
            core,
            proc: tokio::sync::Mutex::new(None),
            last_used: AtomicI64::new(0),
            failed_at: AtomicI64::new(0),
            indexing: Mutex::new(HashSet::new()),
            http,
        })
    }

    /// 意思座標模型（沒裝就 None，挑段照關鍵字）
    pub fn model(&self) -> Option<PathBuf> {
        self.core.as_ref()?.embedding_model()
    }

    pub fn status(&self) -> Value {
        let m = self.model().and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));
        json!({"semantic": m.is_some(), "model": m})
    }

    /// 背景：閒置自動關閉
    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let mut g = me.proc.lock().await;
                if g.is_some() && now() - me.last_used.load(Ordering::SeqCst) > IDLE_SECS {
                    if let Some(p) = g.take() {
                        let pid = p.child.id() as i64;
                        tokio::task::spawn_blocking(move || win::kill(pid)).await.ok();
                        eprintln!("挑段用的 llama-server 閒置 {IDLE_SECS} 秒，已關閉");
                    }
                }
            }
        });
    }

    /// 開著就用；沒開就開一顆（CPU、找空的 port）。開不起來回 None
    async fn server(&self) -> Option<(String, String)> {
        let core = self.core.clone()?;
        let mut g = self.proc.lock().await;
        if let Some(p) = g.as_mut() {
            if matches!(p.child.try_wait(), Ok(None)) {
                return Some((p.url.clone(), p.model.clone()));
            }
            *g = None;
        }
        if now() - self.failed_at.load(Ordering::SeqCst) < RETRY_SECS {
            return None;
        }
        let model = core.embedding_model()?;
        let fail = |why: String| {
            eprintln!("挑段用的 llama-server 開不起來，這段時間先照關鍵字挑：{why}");
            self.failed_at.store(now(), Ordering::SeqCst);
            None::<(String, String)>
        };
        if !core.bin.is_file() {
            return fail(format!("找不到 {}", core.bin.display()));
        }
        let port = match std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()) {
            Ok(a) => a.port(),
            Err(e) => return fail(format!("找不到空的 port：{e}")),
        };
        let log = core.dir.join("embed-server.log");
        let (out, err) = match std::fs::File::create(&log).and_then(|f| Ok((f.try_clone()?, f))) {
            Ok(x) => x,
            Err(e) => return fail(format!("開不了 log 檔 {}：{e}", log.display())),
        };
        let mut cmd = std::process::Command::new(&core.bin);
        cmd.arg("-m").arg(&model);
        // 每段要一次塞進一個 ubatch（-ub＝-b＝-c）；-np 1 免得上下文被切給好幾個槽。
        // --device none：光 -ngl 0 還是會把大批次的運算丟到顯卡（2026-10-29 實測算長段落時多吃 5GB 顯存，
        // 常駐也佔 360MB，把 8GB 卡上的 9B 擠進共享記憶體、生成從每秒 57 字掉到 18 字）
        cmd.args(["--embedding", "--pooling", "last", "-c", "8192", "-ub", "8192", "-b", "8192", "-ngl", "0", "--device", "none", "-np", "1", "--host", "127.0.0.1", "--port"])
            .arg(port.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(out)
            .stderr(err)
            .current_dir(core.bin.parent().unwrap_or(&core.dir));
        win::no_window(&mut cmd);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return fail(format!("啟動失敗：{e}")),
        };
        let job = crate::studio::job::Job::kill_on_close().filter(|j| j.add(&child));
        let url = format!("http://127.0.0.1:{port}");
        let t0 = Instant::now();
        loop {
            if let Ok(Some(code)) = child.try_wait() {
                let tail = std::fs::read_to_string(&log).unwrap_or_default();
                let tail: Vec<&str> = tail.lines().rev().take(4).collect();
                return fail(format!("一開就結束了（{code}）：{}", tail.into_iter().rev().collect::<Vec<_>>().join(" / ")));
            }
            if self.http.get(format!("{url}/health")).timeout(Duration::from_secs(2)).send().await.is_ok_and(|r| r.status().is_success()) {
                break;
            }
            if t0.elapsed() > START_TIMEOUT {
                let pid = child.id() as i64;
                tokio::task::spawn_blocking(move || win::kill(pid)).await.ok();
                return fail(format!("{} 秒內沒有載入完成", START_TIMEOUT.as_secs()));
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        let name = model.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        eprintln!("挑段用的 llama-server 已啟動（{name}，port {port}，{:.1} 秒）", t0.elapsed().as_secs_f64());
        self.last_used.store(now(), Ordering::SeqCst);
        *g = Some(Proc { child, _job: job, url: url.clone(), model: name.clone() });
        Some((url, name))
    }

    /// 算座標（正規化過），照輸入順序
    async fn embed(&self, url: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let mut out = Vec::with_capacity(inputs.len());
        for chunk in inputs.chunks(BATCH) {
            let r = self.http.post(format!("{url}/v1/embeddings")).json(&json!({"input": chunk})).send().await.map_err(|e| e.to_string())?;
            if !r.status().is_success() {
                return Err(format!("HTTP {}：{}", r.status(), r.text().await.unwrap_or_default().chars().take(200).collect::<String>()));
            }
            let v: Value = r.json().await.map_err(|e| e.to_string())?;
            let mut rows: Vec<(usize, Vec<f32>)> = v["data"]
                .as_array()
                .ok_or("回應沒有 data")?
                .iter()
                .enumerate()
                .map(|(n, d)| {
                    let i = d["index"].as_u64().map(|x| x as usize).unwrap_or(n);
                    (i, d["embedding"].as_array().into_iter().flatten().filter_map(Value::as_f64).map(|x| x as f32).collect())
                })
                .collect();
            rows.sort_by_key(|(i, _)| *i);
            if rows.len() != chunk.len() || rows.iter().any(|(_, e)| e.is_empty()) {
                return Err(format!("送 {} 段只拿回 {} 個座標", chunk.len(), rows.len()));
            }
            out.extend(rows.into_iter().map(|(_, e)| norm(e)));
        }
        self.last_used.store(now(), Ordering::SeqCst);
        Ok(out)
    }

    /// 每段的座標（照 parts 順序）：快取有的直接用，缺的算完寫回。limit＝最多現場算幾段，超過回 Ok(None)
    async fn doc_vectors(&self, sk: &Skill, url: &str, model: &str, limit: usize) -> Result<Option<Vec<Vec<f32>>>, String> {
        let keys: Vec<String> = (0..sk.parts.len()).map(|i| key(&sk.doc_text(i))).collect();
        let mut cache = load_cache(&sk.dir, model);
        let missing: Vec<usize> = (0..keys.len()).filter(|&i| !cache.contains_key(&keys[i])).collect();
        if missing.len() > limit {
            return Ok(None);
        }
        if !missing.is_empty() {
            let inputs: Vec<String> = missing.iter().map(|&i| doc_input(&sk.doc_text(i))).collect();
            let vecs = self.embed(url, &inputs).await?;
            for (i, v) in missing.iter().zip(vecs) {
                cache.insert(keys[*i].clone(), v);
            }
            if let Err(e) = save_cache(&sk.dir, model, &cache, &keys) {
                eprintln!("挑段座標存不進 {}：{e}", sk.dir.display());
            }
        }
        Ok(Some(keys.iter().map(|k| cache.get(k).cloned().unwrap_or_default()).collect()))
    }

    /// 照意思排每段的分數（高到低）。沒模型、開不起來、算失敗、大 skill 還在背景算：None（呼叫的地方照關鍵字挑）
    pub async fn ranked(self: &Arc<Self>, sk: &Skill, query: &str) -> Option<Vec<(f64, usize)>> {
        if query.trim().is_empty() || sk.parts.is_empty() {
            return None;
        }
        let (url, model) = self.server().await?;
        let docs = match self.doc_vectors(sk, &url, &model, INLINE_MAX).await {
            Ok(Some(d)) => d,
            Ok(None) => {
                self.index_later(sk.dir.clone());
                return None;
            }
            Err(e) => {
                eprintln!("挑段座標算失敗，這次照關鍵字：{e}");
                return None;
            }
        };
        let q = match self.embed(&url, &[query_text(query)]).await {
            Ok(mut v) => v.pop()?,
            Err(e) => {
                eprintln!("挑段座標算失敗，這次照關鍵字：{e}");
                return None;
            }
        };
        Some(rank(&q, &docs))
    }

    /// 背景把整份 skill 的座標算好（裝好 skill 之後、或大 skill 第一次挑段時）
    pub fn index_later(self: &Arc<Self>, dir: PathBuf) {
        if self.model().is_none() || !self.indexing.lock().unwrap_or_else(|e| e.into_inner()).insert(dir.clone()) {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            let d = dir.clone();
            if let Some(sk) = tokio::task::spawn_blocking(move || skills::load(&d)).await.ok().flatten() {
                if let Some((url, model)) = me.server().await {
                    let t0 = Instant::now();
                    match me.doc_vectors(&sk, &url, &model, usize::MAX).await {
                        Ok(_) => eprintln!("外掛寫法「{}」的挑段座標算好了（{} 段，{:.1} 秒）", sk.name, sk.parts.len(), t0.elapsed().as_secs_f64()),
                        Err(e) => eprintln!("外掛寫法「{}」的挑段座標算失敗：{e}", sk.name),
                    }
                }
            }
            me.indexing.lock().unwrap_or_else(|e| e.into_inner()).remove(&dir);
        });
    }

    /// AI 改寫、只寫詞用（照意思挑；不行就照關鍵字）
    pub async fn pick_block(self: &Arc<Self>, sk: &Skill, query: &str, budget: usize) -> String {
        let sem = self.ranked(sk, query).await;
        sk.pick_block_by(sem.as_deref(), query, budget)
    }

    /// 助手用：挑好的段＋目錄
    pub async fn agent_block(self: &Arc<Self>, sk: &Skill, guide: &str, query: &str) -> String {
        let sem = self.ranked(sk, query).await;
        sk.agent_block_by(guide, query, sem.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors_round_trip_and_rank() {
        let v = norm(vec![3.0, 4.0]);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        assert_eq!(decode(&encode(&v)), Some(v.clone()));
        assert_eq!(decode("!!"), None);
        let docs = vec![norm(vec![0.0, 1.0]), norm(vec![1.0, 0.0]), norm(vec![1.0, 1.0])];
        let r: Vec<usize> = rank(&norm(vec![1.0, 0.1]), &docs).iter().map(|(_, i)| *i).collect();
        assert_eq!(r, vec![1, 2, 0]);
        assert!(query_text(" 逆光 ").starts_with("Instruct: Given a drawing request") && query_text("逆光").ends_with("Query:逆光<|endoftext|>"));
    }

    #[test]
    fn cache_keeps_only_current_parts_and_model() {
        let dir = std::env::temp_dir().join(format!("anima_embed_cache_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let all = HashMap::from([("a".to_string(), vec![1.0f32, 0.0]), ("b".to_string(), vec![0.0f32, 1.0])]);
        save_cache(&dir, "m.gguf", &all, &["a".to_string()]).unwrap();
        let back = load_cache(&dir, "m.gguf");
        assert_eq!(back.len(), 1);
        assert_eq!(back["a"], vec![1.0, 0.0]);
        assert!(load_cache(&dir, "other.gguf").is_empty(), "換模型要整份重算");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn semantic_scores_drive_the_pick() {
        // 意思座標排第一的段，即使關鍵字一個都沒撞到也會被挑
        let dir = std::env::temp_dir().join(format!("anima_embed_pick_{}", std::process::id()));
        let sk_dir = dir.join("demo");
        std::fs::create_dir_all(&sk_dir).unwrap();
        let body = format!(
            "---\nname: demo\ndescription: d\n---\n# Demo\n\n{}\n\n## 逆光寫實\n\n{}\n\n## 海報文字\n\n{}\n",
            "核心規則。".repeat(60),
            "背光、輪廓光、剪影。".repeat(10),
            "標題字、版面、字體。".repeat(10)
        );
        std::fs::write(sk_dir.join("SKILL.md"), body).unwrap();
        let sk = skills::load(&sk_dir).unwrap();
        let poster = sk.parts.iter().position(|p| p.title == "海報文字").unwrap();
        let light = sk.parts.iter().position(|p| p.title == "逆光寫實").unwrap();
        // 「臉年輕一點」關鍵字撞不到任何段：只剩核心段
        assert!(!sk.pick("臉年輕一點", 400).contains(&poster));
        let sem = vec![(0.9, poster), (0.2, light)];
        let picked = sk.pick_by(Some(&sem), "臉年輕一點", 400);
        assert!(picked.contains(&poster), "{picked:?}");
        assert!(sk.pick_block_by(Some(&sem), "臉年輕一點", 400).contains("海報文字"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
