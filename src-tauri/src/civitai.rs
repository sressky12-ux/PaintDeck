//! Civitai：按基底模型搜尋 LoRA、下載到 ComfyUI 的 loras 資料夾、存觸發詞與預覽圖。對照舊版 civitai.py。
//!
//! Qwen 2.1 的 LoRA 放在 loras\qwen21\、MOSS 的放在 loras\moss\ 子資料夾，和 Anima 的分開（生圖站看檔頭分家族，資料夾只是好整理）。
//! API 文件：https://developer.civitai.com/docs/api/public-rest
//!
//! 金鑰：下載網址會帶 token=金鑰；reqwest 的錯誤訊息預設含完整網址，所以所有網路錯誤一律走 net::explain（只寫主機名、不含網址），
//! 錯誤訊息、廣播、紀錄裡都不會出現金鑰。
//! LoRA 的下載交給 dl.rs：斷線自動接著下載（HTTP Range、.part）、逾時、重試、核對大小。
use crate::db::Db;
use crate::dl;
use crate::hub::Hub;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

pub const API: &str = "https://civitai.com/api/v1";
const UA: &str = "AnimaWeb/1.0";

/// 基底模型 → 存放的子資料夾。未知的新模型用安全且穩定的名稱隔開，不混進 Anima。
pub fn base_subdir(base: &str) -> String {
    match base {
        "Anima" => String::new(),
        "Qwen 2.1" => "qwen21".into(),
        "MOSS Image" | "MOSS Image Base" => "moss".into(),
        "Krea 2" | "Krea 2 Turbo" => "krea2".into(),
        "SDXL 1.0" => "sdxl".into(),
        "SD 1.5" => "sd15".into(),
        b => {
            let slug: String = b.to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).take(40).collect();
            let hash = b.as_bytes().iter().fold(2_166_136_261u32, |h, x| (h ^ u32::from(*x)).wrapping_mul(16_777_619));
            format!("other\\{}_{hash:08x}", slug.trim_matches('_'))
        }
    }
}

/// 網頁上的基底模型選項 → Civitai 的 baseModel。MOSS 在 C 站分兩個：SFT 版「MOSS Image」、Base 版「MOSS Image Base」，
/// 兩邊練的 LoRA 生圖站都能用（2026-10-17 查 API：兩個名稱各只有一兩個 LoRA，要一起搜）
pub fn base_models(base: &str) -> Vec<&str> {
    match base {
        // 全部基底：不加 baseModels 過濾，Civitai 回所有基底的 LoRA，使用者自己挑
        "*" => vec![],
        "MOSS Image" => vec!["MOSS Image", "MOSS Image Base"],
        // Krea 官方建議在 Raw 練 LoRA，套到 Turbo 推論；兩種標記要一起找。
        "Krea 2" | "Krea 2 Turbo" => vec!["Krea 2", "Krea 2 Turbo"],
        b => vec![b],
    }
}

fn base_is(base: &str, v: &Value) -> bool {
    if base == "*" {
        return true;
    }
    let vb = v["baseModel"].as_str().unwrap_or("").to_lowercase();
    base_models(base).iter().any(|b| b.to_lowercase() == vb)
}

static IMG_SIZE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/(original=true|width=\d+)[^/]*/").unwrap());

/// Civitai 圖片網址可指定寬度，避免下載原圖
pub fn img_small(url: &str, width: u32) -> String {
    IMG_SIZE
        .replace_all(url, format!("/width={width}/").as_str())
        .into_owned()
}

/// Python round(x, 1)（從精確的二進位值四捨五入，剛好一半取偶數）
fn round1(x: f64) -> f64 {
    format!("{x:.1}").parse().unwrap_or(x)
}

fn version_for<'a>(model: &'a Value, base: &str, version_id: Option<i64>) -> Option<&'a Value> {
    let vs = model["modelVersions"].as_array()?;
    if let Some(id) = version_id {
        if let Some(v) = vs.iter().find(|v| v["id"].as_i64() == Some(id) && base_is(base, v)) {
            return Some(v);
        }
        return None;
    }
    vs.iter().find(|v| base_is(base, v))
}

fn primary_file(v: &Value) -> Option<&Value> {
    let files = v["files"].as_array()?;
    files
        .iter()
        .find(|f| crate::py::truthy(f.get("primary")))
        .or(files.first())
}

fn vinfo(v: &Value) -> Value {
    let empty = json!({});
    let f = primary_file(v).unwrap_or(&empty);
    let kb = f["sizeKB"].as_f64().unwrap_or(0.0);
    let published: String = v["publishedAt"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(10)
        .collect();
    json!({"id": v["id"], "name": v.get("name").cloned().unwrap_or(Value::Null),
           "trainedWords": v.get("trainedWords").cloned().unwrap_or(json!([])),
           "base": v.get("baseModel").cloned().unwrap_or(Value::Null),
           "file": f.get("name").cloned().unwrap_or(Value::Null), "sizeMB": round1(kb / 1024.0), "published": published})
}

fn is_image(i: &Value) -> bool {
    i.get("type").and_then(Value::as_str).unwrap_or("image") == "image"
}

pub fn simplify(m: &Value, base: &str) -> Value {
    let v = version_for(m, base, None);
    let imgs: Vec<&Value> = v
        .and_then(|v| v["images"].as_array())
        .into_iter()
        .flatten()
        .filter(|i| is_image(i))
        .collect();
    let tags: Vec<Value> = m["tags"]
        .as_array()
        .map(|t| t.iter().take(8).cloned().collect())
        .unwrap_or_default();
    let versions: Vec<Value> = m["modelVersions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|x| base_is(base, x))
        .map(vinfo)
        .collect();
    let images: Vec<Value> = imgs
        .iter()
        .take(6)
        .map(|i| json!({"url": i["url"].as_str().map(|u| img_small(u, 450)), "nsfwLevel": i.get("nsfwLevel").cloned().unwrap_or(json!(1))}))
        .collect();
    json!({
        "id": m["id"], "name": m["name"], "type": m.get("type").cloned().unwrap_or(Value::Null), "nsfw": m.get("nsfw").cloned().unwrap_or(Value::Null),
        "creator": m["creator"]["username"], "tags": tags,
        "downloads": m["stats"].get("downloadCount").cloned().unwrap_or(json!(0)),
        "likes": m["stats"].get("thumbsUpCount").cloned().unwrap_or(json!(0)),
        "version": v.map(vinfo).unwrap_or(Value::Null), "versions": versions, "images": images,
        "url": format!("https://civitai.com/models/{}", m["id"]),
    })
}

/// 網路錯誤 → 看得懂的中文原因＋怎麼辦（net::explain）。訊息只有主機名、沒有網址，不會帶出金鑰
fn net_err(e: reqwest::Error, url: &str) -> String {
    crate::net::explain(&e, url)
}

pub struct Civitai {
    lora_dir: PathBuf,
    preview_dir: PathBuf,
    db: Arc<Db>,
    hub: Hub,
    settings_path: PathBuf,
    http: reqwest::Client,
    /// versionId（或 modelId）→ 下載狀態，保留加入順序
    pub downloads: Mutex<Map<String, Value>>,
}

impl Civitai {
    pub fn new(
        lora_dir: &Path,
        preview_dir: &Path,
        db: Arc<Db>,
        hub: Hub,
        settings_path: &Path,
    ) -> Arc<Civitai> {
        let http = reqwest::Client::builder()
            .proxy(crate::net::proxy()) // 梯子（系統代理、PAC、環境變數）：net.rs
            .pool_max_idle_per_host(0) // 不留舊連線，改了代理設定馬上生效（見 setup.rs）
            .user_agent(UA)
            // 連不上 20 秒就放棄；收資料 90 秒一個位元組都沒來就當作斷線（網路忽斷忽續時不會卡死）
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(90))
            .build()
            .expect("建立 HTTP client");
        Arc::new(Civitai {
            lora_dir: lora_dir.to_path_buf(),
            preview_dir: preview_dir.to_path_buf(),
            db,
            hub,
            settings_path: settings_path.to_path_buf(),
            http,
            downloads: Mutex::new(Map::new()),
        })
    }

    /// 每次用的時候才讀設定（使用者剛在設定頁填的金鑰馬上生效）
    fn key(&self) -> String {
        crate::settings::civitai_key(&crate::settings::load(&self.settings_path))
    }

    fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let k = self.key();
        if k.is_empty() {
            rb
        } else {
            rb.bearer_auth(k)
        }
    }

    pub(crate) async fn get_json(&self, url: &str, query: &[(&str, String)]) -> Result<Value, String> {
        let r = self
            .authed(self.http.get(url).query(query))
            .timeout(Duration::from_secs(40))
            .send()
            .await
            .map_err(|e| net_err(e, url))?;
        let status = r.status();
        if status.as_u16() >= 400 {
            let body: String = r
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect();
            return Err(format!("Civitai 回應 {}：{body}", status.as_u16()));
        }
        r.json().await.map_err(|e| net_err(e, url))
    }

    pub fn subdir(&self, base: &str) -> PathBuf {
        self.lora_dir.join(base_subdir(base))
    }

    /// 已安裝的 LoRA 檔名。base="*"（全部基底）時掃 loras 根目錄和每個基底子目錄；否則只掃該基底的子目錄
    pub fn installed_names(&self, base: &str) -> std::collections::HashSet<String> {
        let names = |dir: &Path| -> Vec<String> {
            std::fs::read_dir(dir)
                .map(|rd| rd.flatten().filter(|e| e.path().is_file()).map(|e| e.file_name().to_string_lossy().to_string()).collect())
                .unwrap_or_default()
        };
        if base != "*" {
            return names(&self.subdir(base)).into_iter().collect();
        }
        let mut out: std::collections::HashSet<String> = names(&self.lora_dir).into_iter().collect();
        if let Ok(rd) = std::fs::read_dir(&self.lora_dir) {
            for e in rd.flatten().filter(|e| e.path().is_dir()) {
                out.extend(names(&e.path()));
                // other\ 底下還有一層（未知基底）
                if let Ok(sub) = std::fs::read_dir(e.path()) {
                    for s in sub.flatten().filter(|s| s.path().is_dir()) {
                        out.extend(names(&s.path()));
                    }
                }
            }
        }
        out
    }

    /// 選項跟 Civitai 的 BaseModel enum 同步；前端仍可輸入新名稱，不用等生圖站更新。
    pub async fn bases(&self) -> Result<Vec<String>, String> {
        let data = self.get_json(&format!("{API}/enums"), &[]).await?;
        Ok(data["BaseModel"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect())
    }

    /// 參數一對一對應 Civitai 搜尋的查詢欄位，包成結構反而難對照，所以放行參數個數的警告
    #[allow(clippy::too_many_arguments)]
    pub async fn search(
        &self,
        query: &str,
        sort: &str,
        period: &str,
        nsfw: bool,
        cursor: Option<&str>,
        tag: Option<&str>,
        base: &str,
    ) -> Result<Value, String> {
        let mut q: Vec<(&str, String)> = vec![
            ("types", "LORA".into()),
            ("sort", sort.into()),
            ("period", period.into()),
            ("limit", "24".into()),
            ("nsfw", if nsfw { "true" } else { "false" }.into()),
        ];
        for b in base_models(base) {
            q.push(("baseModels", b.into()));
        }
        if !query.is_empty() {
            q.push(("query", query.into()));
        }
        if let Some(t) = tag.filter(|t| !t.is_empty()) {
            q.push(("tag", t.into()));
        }
        if let Some(c) = cursor.filter(|c| !c.is_empty()) {
            q.push(("cursor", c.into()));
        }
        let d = self.get_json(&format!("{API}/models"), &q).await?;
        let items: Vec<Value> = d["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| simplify(m, base))
            .filter(|i| !i["version"].is_null())
            .collect();
        Ok(
            json!({"items": items, "next": d["metadata"].get("nextCursor").cloned().unwrap_or(Value::Null)}),
        )
    }

    fn update(&self, key: &str, f: impl FnOnce(&mut Map<String, Value>)) -> Value {
        let mut all = self.downloads.lock().unwrap_or_else(|e| e.into_inner());
        let st = all
            .entry(key.to_string())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("下載狀態一定是物件");
        f(st);
        Value::Object(st.clone())
    }

    fn broadcast(&self, st: &Value) {
        let mut m = json!({"type": "download"});
        for (k, v) in st.as_object().into_iter().flatten() {
            m[k] = v.clone();
        }
        self.hub.send(m);
    }

    pub fn start_download(
        self: &Arc<Self>,
        model_id: i64,
        version_id: Option<i64>,
        base: &str,
    ) -> String {
        let key = version_id.unwrap_or(model_id).to_string();
        {
            let mut all = self.downloads.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(
                all.get(&key).and_then(|s| s["status"].as_str()),
                Some("starting" | "downloading")
            ) {
                return key;
            }
            all.insert(
                key.clone(),
                json!({"key": key, "status": "starting", "name": "", "done": 0, "total": 0}),
            );
        }
        let (me, k, b) = (self.clone(), key.clone(), base.to_string());
        tokio::spawn(async move {
            if let Err(e) = me.download(model_id, version_id, &k, &b).await {
                let st = me.update(&k, |s| {
                    s.insert("status".into(), json!("error"));
                    s.insert("error".into(), json!(e));
                });
                me.broadcast(&st);
            }
        });
        key
    }

    async fn download(
        &self,
        model_id: i64,
        version_id: Option<i64>,
        key: &str,
        base: &str,
    ) -> Result<(), String> {
        let m = self
            .get_json(&format!("{API}/models/{model_id}"), &[])
            .await?;
        let v = version_for(&m, base, version_id)
            .ok_or_else(|| format!("這個模型沒有 {base} 版本"))?
            .clone();
        let f = primary_file(&v).ok_or("這個版本沒有檔案")?.clone();
        let name = f["name"].as_str().ok_or("檔名不明")?.to_string();
        let name = crate::server::basename(&name); // 檔名來自網路：只留檔名
        let total = (f["sizeKB"].as_f64().unwrap_or(0.0) * 1024.0) as i64;
        self.update(key, |s| {
            s.insert("name".into(), m["name"].clone());
            s.insert("file".into(), json!(name));
            s.insert("total".into(), json!(total));
        });
        let dir = self.subdir(base);
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| e.to_string())?;
        let dest = dir.join(&name);
        // ComfyUI 列子資料夾裡的 LoRA 時名稱是「moss\檔名」「qwen21\檔名」，資訊也用同樣的名稱存
        let sub = base_subdir(base);
        let meta_name = if sub.is_empty() {
            name.clone()
        } else {
            format!("{sub}\\{name}")
        };
        if !dest.exists() {
            let mut url = f["downloadUrl"].as_str().ok_or("沒有下載網址")?.to_string();
            let k = self.key();
            if !k.is_empty() {
                url += if url.contains('?') { "&" } else { "?" };
                url += &format!("token={k}");
            }
            let st = self.update(key, |s| {
                s.insert("status".into(), json!("downloading"));
            });
            self.broadcast(&st);
            // 下載、斷線續傳（.part）、逾時、重試、登入頁的判斷都在 dl.rs。
            // .part 檔名帶版本編號：同一個檔名但不同版本的 LoRA 不會接錯檔。
            // 清單上的 sizeKB 只是約數，不拿來核對；大小以伺服器給的 Content-Length 為準
            let tag = v["id"].as_i64().map(|i| i.to_string());
            let job = dl::Job {
                tag: tag.as_deref(),
                ..dl::Job::new(&self.http, &url)
            };
            dl::fetch(&job, dl::Dest::Path(dest.clone()), &mut |done, total| {
                let st = self.update(key, |s| {
                    s.insert("done".into(), json!(done));
                    if let Some(t) = total {
                        s.insert("total".into(), json!(t));
                    }
                });
                self.broadcast(&st);
            })
            .await?;
        }
        // 觸發詞、說明、預覽圖（尺度最低的那張）
        let mut imgs: Vec<&Value> = v["images"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| is_image(i))
            .collect();
        imgs.sort_by_key(|i| i.get("nsfwLevel").and_then(Value::as_i64).unwrap_or(1));
        let mut preview = Value::Null;
        if let Some(first) = imgs.first().and_then(|i| i["url"].as_str()) {
            let pname = format!(
                "{}.jpg",
                Path::new(&name)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            );
            let r = self
                .http
                .get(img_small(first, 360))
                .timeout(Duration::from_secs(60))
                .send()
                .await;
            if let Ok(Ok(r)) = r.map(|r| r.error_for_status()) {
                if let Ok(b) = r.bytes().await {
                    if tokio::fs::write(self.preview_dir.join(&pname), &b)
                        .await
                        .is_ok()
                    {
                        preview = json!(pname);
                    }
                }
            }
        }
        let mut meta = self
            .db
            .lora_meta()
            .map_err(|e| e.to_string())?
            .get(&meta_name)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let tags: Vec<Value> = m["tags"]
            .as_array()
            .map(|t| t.iter().take(8).cloned().collect())
            .unwrap_or_default();
        for (k, val) in [
            ("title", m["name"].clone()),
            ("modelId", m["id"].clone()),
            ("versionId", v["id"].clone()),
            ("version", v.get("name").cloned().unwrap_or(Value::Null)),
            ("base_model", v.get("baseModel").cloned().unwrap_or(Value::Null)),
            (
                "triggers",
                v.get("trainedWords").cloned().unwrap_or(json!([])),
            ),
            ("tags", json!(tags)),
            ("nsfw", m.get("nsfw").cloned().unwrap_or(Value::Null)),
            (
                "url",
                json!(format!("https://civitai.com/models/{}", m["id"])),
            ),
            ("preview", preview),
        ] {
            meta.insert(k.into(), val);
        }
        meta.entry("strength").or_insert(json!(0.8));
        self.db
            .set_lora_meta(&meta_name, &Value::Object(meta))
            .map_err(|e| e.to_string())?;
        let st = self.update(key, |s| {
            s.insert("status".into(), json!("done"));
        });
        self.broadcast(&st);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn img_small_like_python() {
        assert_eq!(
            img_small("https://image.civitai.com/x/abc/original=true/1.jpeg", 450),
            "https://image.civitai.com/x/abc/width=450/1.jpeg"
        );
        assert_eq!(
            img_small(
                "https://image.civitai.com/x/abc/width=1024,quality=90/1.jpeg",
                360
            ),
            "https://image.civitai.com/x/abc/width=360/1.jpeg"
        );
        assert_eq!(round1(144.25), 144.2);
        assert_eq!(round1(0.05), 0.1); // 0.05 的二進位值略大於 0.05
    }

    #[test]
    fn moss_matches_both_civitai_bases() {
        let m = json!({"modelVersions": [{"id": 1, "baseModel": "Anima"}, {"id": 2, "baseModel": "MOSS Image Base"}, {"id": 3, "baseModel": "MOSS Image"}]});
        assert_eq!(version_for(&m, "MOSS Image", None).unwrap()["id"], 2);
        assert_eq!(version_for(&m, "Anima", None).unwrap()["id"], 1);
        let s = simplify(&json!({"id": 9, "name": "x", "creator": {"username": "u"}, "stats": {}, "modelVersions": m["modelVersions"]}), "MOSS Image");
        assert_eq!(s["versions"].as_array().unwrap().len(), 2);
        assert_eq!(base_subdir("MOSS Image"), "moss");
        assert_eq!(base_subdir("Anima"), "");
        assert_eq!(base_subdir("Qwen 2.1"), "qwen21");
        assert_eq!(base_subdir("Krea 2"), "krea2");
        assert_eq!(base_subdir("Krea 2 Turbo"), "krea2");
        let k = json!({"modelVersions": [{"id": 4, "baseModel": "Krea 2 Turbo"}]});
        assert_eq!(version_for(&k, "Krea 2", None).unwrap()["id"], 4);
        assert!(base_subdir("Flux.3").starts_with("other\\flux_3_"));
        assert!(!base_subdir("../../escape").contains(".."));
        assert!(version_for(&m, "Anima", Some(2)).is_none());
    }

    #[test]
    fn all_bases_lists_every_version_with_its_base() {
        // "*"：不加 baseModels 過濾，每個版本都算，卡片上帶各自的基底
        assert!(base_models("*").is_empty());
        let m = json!({"id": 9, "name": "x", "creator": {"username": "u"}, "stats": {},
                       "modelVersions": [{"id": 1, "baseModel": "Anima"}, {"id": 2, "baseModel": "Qwen 2.1"}]});
        let s = simplify(&m, "*");
        let vs = s["versions"].as_array().unwrap();
        assert_eq!(vs.len(), 2);
        assert_eq!(vs[0]["base"], "Anima");
        assert_eq!(vs[1]["base"], "Qwen 2.1");
        // 主版本（卡片預設）是第一個
        assert_eq!(s["version"]["base"], "Anima");
    }
}
