//! 圖庫、任務、預設組、LoRA 備註的 SQLite 存取（data/anima.db）。逐項對照舊版 anima_ui/app/db.py：
//! 舊資料庫會在開啟時遷移：只有尚在圖庫的圖片需要檔名唯一，回收區可保留同名舊圖。
//!
//! 查詢結果用 JSON 物件回傳（欄位名 → 值），跟 Python 的 dict(row) 一樣直接給 API 用。
use crate::py::{dumps, now, or, truthy};
use rusqlite::types::{Value as Sql, ValueRef};
use rusqlite::{params, params_from_iter, Connection, ErrorCode, Params};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

pub type Row = Map<String, Value>;
pub type DbResult<T> = rusqlite::Result<T>;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS images (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  filename TEXT NOT NULL, subfolder TEXT NOT NULL DEFAULT '', type TEXT NOT NULL DEFAULT 'output',
  mode TEXT, prompt TEXT, negative TEXT, seed INTEGER, width INTEGER, height INTEGER,
  params TEXT, job TEXT, created REAL, fav INTEGER DEFAULT 0, deleted INTEGER DEFAULT 0
);
CREATE INDEX IF NOT EXISTS images_created ON images(created DESC);
CREATE TABLE IF NOT EXISTS jobs (
  id TEXT PRIMARY KEY, params TEXT, info TEXT, status TEXT, created REAL, finished REAL, error TEXT
);
CREATE TABLE IF NOT EXISTS presets (
  id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, data TEXT NOT NULL, created REAL
);
CREATE TABLE IF NOT EXISTS lora_meta (
  name TEXT PRIMARY KEY, data TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS prompt_lib (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL DEFAULT 'set', name TEXT NOT NULL, folder TEXT NOT NULL DEFAULT '',
  tags TEXT NOT NULL DEFAULT '[]', family TEXT NOT NULL DEFAULT '', fav INTEGER NOT NULL DEFAULT 0,
  pos REAL NOT NULL DEFAULT 0, note TEXT NOT NULL DEFAULT '', data TEXT NOT NULL, thumb TEXT,
  created REAL, updated REAL, used REAL, uses INTEGER NOT NULL DEFAULT 0
);
";

/// 提示詞庫列表要的欄位（縮圖另外抓：/api/lib/{id}/thumb）
const LIB_COLS: &str = "id, kind, name, folder, tags, family, fav, pos, note, data, created, updated, used, uses, \
                        thumb IS NOT NULL AS has_thumb";

/// 把 {src}.presets 的預設組抄進提示詞庫（整套設定、未分類），已有同名整套設定的不抄。
/// 第一次開啟新版（src = main）和救回舊版備份（src = old，備份裡還沒有提示詞庫）共用；?1 = 現在時間
pub fn lib_from_presets_sql(src: &str) -> String {
    format!(
        "INSERT INTO main.prompt_lib(kind, name, data, pos, created, updated) \
         SELECT 'set', p.name, p.data, (SELECT COUNT(*) FROM {src}.presets p2 WHERE p2.name < p.name), \
                COALESCE(p.created, ?1), COALESCE(p.created, ?1) \
         FROM {src}.presets p WHERE p.name NOT IN (SELECT name FROM main.prompt_lib WHERE kind='set')"
    )
}

/// 提示詞庫的一筆：kind＝set（整套設定：生成頁表單）或 snippet（片段：data.text）；family 空白＝通用
#[derive(Debug, Clone)]
pub struct LibItem {
    pub kind: String,
    pub name: String,
    pub folder: String,
    pub tags: Vec<String>,
    pub family: String,
    pub fav: bool,
    pub note: String,
    pub data: Value,
}

/// 存檔時縮圖怎麼處理（Set 是 webp 的 base64）
pub enum Thumb {
    Keep,
    Set(String),
    Clear,
}

fn lib_row(mut r: Row) -> Row {
    parse_field(&mut r, "data");
    let tags = r
        .get("tags")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .filter(Value::is_array)
        .unwrap_or_else(|| json!([]));
    r.insert("tags".into(), tags);
    r
}

/// 圖庫列表要的欄位（不含 params 全文）
const COLS: &str = "id, filename, subfolder, mode, prompt, idea, seed, width, height, created, fav, trashed, \
                    json_extract(params, '$.model') AS model, json_extract(params, '$.family') AS family";

/// 圖庫查詢條件；date_from／date_to 是 Unix 秒數；sort：new／old／fav
#[derive(Debug, Clone)]
pub struct GalleryQuery {
    pub offset: i64,
    pub limit: i64,
    pub fav: bool,
    pub mode: Option<String>,
    pub q: Option<String>,
    pub trash: bool,
    pub model: Option<String>,
    pub family: Option<String>,
    pub lora: Option<String>,
    pub date_from: Option<f64>,
    pub date_to: Option<f64>,
    pub sort: String,
}

impl Default for GalleryQuery {
    fn default() -> Self {
        GalleryQuery {
            offset: 0,
            limit: 60,
            fav: false,
            mode: None,
            q: None,
            trash: false,
            model: None,
            family: None,
            lora: None,
            date_from: None,
            date_to: None,
            sort: "new".into(),
        }
    }
}

pub struct Db {
    con: Mutex<Connection>,
}

fn to_json(v: ValueRef) -> Value {
    match v {
        ValueRef::Null | ValueRef::Blob(_) => Value::Null,
        ValueRef::Integer(i) => i.into(),
        ValueRef::Real(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
    }
}

/// 要寫進資料庫的 JSON 值 → SQLite 值（True／False 跟 Python 一樣存成 1／0）
pub fn to_sql(v: Option<&Value>) -> Sql {
    match v {
        None | Some(Value::Null) => Sql::Null,
        Some(Value::Bool(b)) => Sql::Integer(*b as i64),
        Some(Value::Number(n)) => n
            .as_i64()
            .map(Sql::Integer)
            .unwrap_or_else(|| Sql::Real(n.as_f64().unwrap_or(0.0))),
        Some(Value::String(s)) => Sql::Text(s.clone()),
        Some(other) => Sql::Text(dumps(other)),
    }
}

/// json.loads(text or "{}")
fn parse_or_empty(v: Option<&Value>) -> Value {
    match v.and_then(Value::as_str) {
        Some(s) if !s.is_empty() => serde_json::from_str(s).unwrap_or_else(|_| json!({})),
        _ => json!({}),
    }
}

fn parse_field(r: &mut Row, k: &str) {
    let v = parse_or_empty(r.get(k));
    r.insert(k.into(), v);
}

fn qmarks(n: usize) -> String {
    vec!["?"; n].join(",")
}

/// 拿 JSON 值當 HashMap 的鍵（任務編號一般是字串）
fn key_of(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string())
}

impl Db {
    pub fn open(path: &Path) -> DbResult<Db> {
        let mut con = Connection::open(path)?;
        let had_lib = !columns(&con, "prompt_lib")?.is_empty();
        con.execute_batch(SCHEMA)?;
        if !had_lib {
            // 第一次開啟有提示詞庫的版本：舊的預設組抄進來（presets 表原樣保留）
            con.execute(&lib_from_presets_sql("main"), [now()])?;
        }
        let cols = columns(&con, "images")?;
        if !cols.contains("idea") {
            // AI 改寫：原始想法（改寫結果就是 prompt；兩者也都在 params 裡）
            con.execute("ALTER TABLE images ADD COLUMN idea TEXT", [])?;
        }
        if !cols.contains("trashed") {
            // 移到回收區的時間（deleted=1 的圖）；deleted=2＝已清空或檔案遺失，不再顯示
            con.execute("ALTER TABLE images ADD COLUMN trashed REAL", [])?;
        }
        // 舊版的全域 UNIQUE 也鎖住回收區的檔名。ComfyUI 重用檔名時，
        // INSERT 會誤回傳回收區舊圖的 id，令新圖變成未追蹤檔案。
        let table_sql: String = con.query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='images'",
            [],
            |r| r.get(0),
        )?;
        if table_sql.contains("UNIQUE(subfolder, filename)") {
            let tx = con.transaction()?;
            tx.execute_batch(
                "CREATE TABLE images_new (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   filename TEXT NOT NULL, subfolder TEXT NOT NULL DEFAULT '', type TEXT NOT NULL DEFAULT 'output',
                   mode TEXT, prompt TEXT, negative TEXT, seed INTEGER, width INTEGER, height INTEGER,
                   params TEXT, job TEXT, created REAL, fav INTEGER DEFAULT 0, deleted INTEGER DEFAULT 0,
                   idea TEXT, trashed REAL
                 );
                 INSERT INTO images_new (id, filename, subfolder, type, mode, prompt, negative, seed,
                   width, height, params, job, created, fav, deleted, idea, trashed)
                 SELECT id, filename, subfolder, type, mode, prompt, negative, seed,
                   width, height, params, job, created, fav, deleted, idea, trashed FROM images;
                 DROP TABLE images;
                 ALTER TABLE images_new RENAME TO images;",
            )?;
            tx.commit()?;
        }
        con.execute_batch(
            "CREATE INDEX IF NOT EXISTS images_created ON images(created DESC);
             CREATE UNIQUE INDEX IF NOT EXISTS images_active_file
             ON images(subfolder, filename) WHERE deleted=0;",
        )?;
        if !columns(&con, "jobs")?.contains("started") {
            // 開始執行的時間：算每張花幾秒、預估剩餘時間
            con.execute("ALTER TABLE jobs ADD COLUMN started REAL", [])?;
        }
        Ok(Db {
            con: Mutex::new(con),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.con.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 整段操作期間都鎖住資料庫（恢復原廠的備份、ATTACH 舊資料庫抄資料）
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> T) -> T {
        f(&self.lock())
    }

    /// 查詢，每列轉成 {欄位: 值}
    pub fn q(&self, sql: &str, args: impl Params) -> DbResult<Vec<Row>> {
        let con = self.lock();
        let mut st = con.prepare_cached(sql)?;
        let names: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
        let mut rows = st.query(args)?;
        let mut out = vec![];
        while let Some(r) = rows.next()? {
            let mut m = Map::new();
            for (i, n) in names.iter().enumerate() {
                m.insert(n.clone(), to_json(r.get_ref(i)?));
            }
            out.push(m);
        }
        Ok(out)
    }

    pub fn q1(&self, sql: &str, args: impl Params) -> DbResult<Option<Row>> {
        Ok(self.q(sql, args)?.into_iter().next())
    }

    /// 執行寫入，回傳最後插入的 rowid
    pub fn run(&self, sql: &str, args: impl Params) -> DbResult<i64> {
        let con = self.lock();
        con.prepare_cached(sql)?.execute(args)?;
        Ok(con.last_insert_rowid())
    }

    // ---------------------------------------------------------------- jobs
    pub fn add_job(&self, pid: &str, params: &Value, info: &Value) -> DbResult<()> {
        self.run(
            "INSERT OR REPLACE INTO jobs(id, params, info, status, created) VALUES(?,?,?,?,?)",
            params![pid, dumps(params), dumps(info), "queued", now()],
        )?;
        Ok(())
    }

    pub fn job(&self, pid: &str) -> DbResult<Option<Row>> {
        let mut r = self.q1("SELECT * FROM jobs WHERE id=?", [pid])?;
        if let Some(r) = r.as_mut() {
            parse_field(r, "params");
            parse_field(r, "info");
        }
        Ok(r)
    }

    pub fn set_job(&self, pid: &str, status: &str, error: Option<&str>) -> DbResult<()> {
        if status == "running" {
            self.run(
                "UPDATE jobs SET status=?, started=? WHERE id=?",
                params![status, now(), pid],
            )?;
        } else {
            self.run(
                "UPDATE jobs SET status=?, error=?, finished=? WHERE id=?",
                params![status, error, now(), pid],
            )?;
        }
        Ok(())
    }

    pub fn open_jobs(&self) -> DbResult<Vec<String>> {
        Ok(self
            .q(
                "SELECT id FROM jobs WHERE status IN ('queued','running')",
                [],
            )?
            .iter()
            .map(|r| key_of(&r["id"]))
            .collect())
    }

    /// 最近的任務（任務面板用），附上摘要和產生的圖
    pub fn jobs(&self, limit: i64) -> DbResult<Vec<Row>> {
        let mut rows = self.q("SELECT * FROM jobs ORDER BY created DESC LIMIT ?", [limit])?;
        if rows.is_empty() {
            return Ok(rows);
        }
        let ids: Vec<Sql> = rows.iter().map(|r| to_sql(r.get("id"))).collect();
        let mut imgs: HashMap<String, Vec<Value>> = HashMap::new();
        let sql = format!(
            "SELECT id, job, deleted FROM images WHERE job IN ({}) ORDER BY id",
            qmarks(ids.len())
        );
        for r in self.q(&sql, params_from_iter(ids))? {
            imgs.entry(key_of(&r["job"]))
                .or_default()
                .push(json!({"id": r["id"], "deleted": r["deleted"]}));
        }
        for r in rows.iter_mut() {
            let p = parse_or_empty(r.get("params"));
            let info = parse_or_empty(r.get("info"));
            let get = |o: Option<&Value>| o.cloned().unwrap_or(Value::Null);
            let prompt: String = or(info.get("prompt"), p.get("prompt"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .take(200)
                .collect();
            let summary = json!({
                "mode": get(p.get("mode")), "model": get(p.get("model")), "prompt": prompt,
                "seed": get(info.get("seed")), "width": get(or(info.get("width"), p.get("width"))),
                "height": get(or(info.get("height"), p.get("height"))), "steps": get(p.get("steps")),
                "idea": if truthy(p.get("idea")) { p["idea"].clone() } else { json!("") },
            });
            let images = imgs.remove(&key_of(&r["id"])).unwrap_or_default();
            r.insert("params".into(), p);
            r.insert("summary".into(), summary);
            r.remove("info");
            r.insert("images".into(), Value::Array(images));
        }
        Ok(rows)
    }

    /// 同模型、同模式最近幾張平均花幾秒（排隊中的任務估時間用）
    pub fn avg_seconds(
        &self,
        model: Option<&str>,
        mode: Option<&str>,
        n: i64,
    ) -> DbResult<Option<f64>> {
        let mut sql = String::from("SELECT finished - started AS s FROM jobs WHERE status='done' AND started IS NOT NULL AND finished > started");
        let mut args: Vec<Sql> = vec![];
        if let Some(m) = model.filter(|m| !m.is_empty()) {
            sql += " AND json_extract(params, '$.model')=?";
            args.push(Sql::Text(m.into()));
        }
        if let Some(m) = mode.filter(|m| !m.is_empty()) {
            sql += " AND json_extract(params, '$.mode')=?";
            args.push(Sql::Text(m.into()));
        }
        sql += " ORDER BY finished DESC LIMIT ?";
        args.push(Sql::Integer(n));
        let rows = self.q(&sql, params_from_iter(args))?;
        if rows.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            rows.iter()
                .map(|r| r["s"].as_f64().unwrap_or(0.0))
                .sum::<f64>()
                / rows.len() as f64,
        ))
    }

    // ---------------------------------------------------------------- images
    pub fn add_image(
        &self,
        filename: &str,
        subfolder: &str,
        typ: &str,
        info: &Value,
        params: &Value,
        job: Option<&str>,
    ) -> DbResult<i64> {
        let g = |k: &str| to_sql(info.get(k));
        let idea = if truthy(info.get("idea")) {
            g("idea")
        } else {
            Sql::Null
        };
        let args = vec![
            Sql::Text(filename.into()),
            Sql::Text(subfolder.into()),
            Sql::Text(typ.into()),
            g("mode"),
            g("prompt"),
            g("negative"),
            g("seed"),
            g("width"),
            g("height"),
            Sql::Text(dumps(params)),
            job.map(|j| Sql::Text(j.into())).unwrap_or(Sql::Null),
            Sql::Real(now()),
            idea,
        ];
        const INSERT: &str = "INSERT INTO images(filename, subfolder, type, mode, prompt, negative, seed, width, height, params, job, created, idea) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)";
        let r = self.run(INSERT, params_from_iter(args.clone()));
        match r {
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == ErrorCode::ConstraintViolation =>
            {
                // 同一個檔案已經在圖庫裡：回傳原本的編號
                let row = self.q1(
                    "SELECT id, job FROM images WHERE subfolder=? AND filename=? AND deleted=0",
                    [subfolder, filename],
                )?;
                let Some((iid, old_job)) = row.and_then(|r| Some((r["id"].as_i64()?, r["job"].as_str().map(str::to_owned)))) else {
                    return Err(rusqlite::Error::SqliteFailure(e, None));
                };
                // 別的任務生出同名檔：ComfyUI 只在舊檔不在了才會重用檔名（例如在檔案總管刪掉），
                // 舊紀錄的檔案已經遺失（deleted=2），新圖另開一筆（不然新圖會掛著舊的提示詞和舊縮圖，也對不到這個任務）
                if job.is_some() && old_job.as_deref() != job {
                    self.run("UPDATE images SET deleted=2 WHERE id=?", [iid])?;
                    return self.run(INSERT, params_from_iter(args));
                }
                Ok(iid)
            }
            other => other,
        }
    }

    pub fn image(&self, iid: i64) -> DbResult<Option<Row>> {
        let mut r = self.q1("SELECT * FROM images WHERE id=?", [iid])?;
        if let Some(r) = r.as_mut() {
            parse_field(r, "params");
        }
        Ok(r)
    }

    /// 圖庫查詢，回傳（符合的總數, 這一頁）
    pub fn gallery(&self, g: &GalleryQuery) -> DbResult<(i64, Vec<Row>)> {
        let mut wh = vec!["deleted=?"];
        let mut args: Vec<Sql> = vec![Sql::Integer(g.trash as i64)];
        let text = |s: &Option<String>| s.clone().filter(|x| !x.is_empty());
        if g.fav {
            wh.push("fav=1");
        }
        if let Some(m) = text(&g.mode) {
            if m == "comic" {
                // 漫畫台畫的整頁、修好角色的整頁（mode 欄記的是 txt2img／multiref，要看參數裡的 comic）
                wh.push("json_extract(params, '$.comic.kind') IN ('page', 'fix')");
            } else {
                wh.push("mode=?");
                args.push(Sql::Text(m));
            }
        }
        if let Some(q) = text(&g.q) {
            wh.push("(prompt LIKE ? OR negative LIKE ? OR idea LIKE ? OR CAST(seed AS TEXT)=?)");
            let like = format!("%{q}%");
            args.extend([
                Sql::Text(like.clone()),
                Sql::Text(like.clone()),
                Sql::Text(like),
                Sql::Text(q),
            ]);
        }
        if let Some(m) = text(&g.model) {
            wh.push("json_extract(params, '$.model')=?");
            args.push(Sql::Text(m));
        }
        if let Some(f) = text(&g.family) {
            // 匯入的舊圖沒記家族：沒寫的當成 Anima（生圖站一開始只有 Anima）
            wh.push("COALESCE(json_extract(params, '$.family'), 'anima')=?");
            args.push(Sql::Text(f));
        }
        if let Some(l) = text(&g.lora) {
            wh.push("EXISTS (SELECT 1 FROM json_each(images.params, '$.loras') WHERE json_extract(value, '$.name')=?)");
            args.push(Sql::Text(l));
        }
        // 網址參數沒填（空字串）時由伺服器給 None
        if let Some(d) = g.date_from {
            wh.push("created>=?");
            args.push(Sql::Real(d));
        }
        if let Some(d) = g.date_to {
            wh.push("created<?");
            args.push(Sql::Real(d));
        }
        let w = format!(" WHERE {}", wh.join(" AND "));
        let total = self
            .q1(
                &format!("SELECT COUNT(*) AS n FROM images{w}"),
                params_from_iter(args.iter()),
            )?
            .and_then(|r| r["n"].as_i64())
            .unwrap_or(0);
        let order = if g.trash {
            "trashed DESC, id DESC"
        } else {
            match g.sort.as_str() {
                "old" => "created ASC, id ASC",
                "fav" => "fav DESC, created DESC, id DESC",
                _ => "created DESC, id DESC",
            }
        };
        args.extend([Sql::Integer(g.limit), Sql::Integer(g.offset)]);
        let rows = self.q(
            &format!("SELECT {COLS} FROM images{w} ORDER BY {order} LIMIT ? OFFSET ?"),
            params_from_iter(args),
        )?;
        Ok((total, rows))
    }

    /// 漫畫台某個作品畫過的整頁（kind=page）和修好角色的整頁（kind=fix，src＝修的是哪一頁），新的在前
    pub fn comic_images(&self, comic_id: &str) -> DbResult<Vec<Row>> {
        self.q(
            "SELECT id, seed, created, json_extract(params, '$.comic.kind') AS kind, json_extract(params, '$.comic.from') AS src \
             FROM images WHERE deleted=0 AND json_extract(params, '$.comic.id')=? \
             AND json_extract(params, '$.comic.kind') IN ('page', 'fix') ORDER BY id DESC LIMIT 300",
            [comic_id],
        )
    }

    /// 篩選選單用：圖庫裡出現過的模型、LoRA，以及各分類張數
    pub fn facets(&self) -> DbResult<Value> {
        let models: Vec<Value> = self
            .q("SELECT DISTINCT json_extract(params, '$.model') AS m FROM images WHERE deleted=0 AND m IS NOT NULL AND m <> '' ORDER BY m", [])?
            .into_iter().map(|mut r| r.remove("m").unwrap_or(Value::Null)).collect();
        let loras: Vec<Value> = self
            .q("SELECT DISTINCT json_extract(j.value, '$.name') AS n FROM images, json_each(images.params, '$.loras') AS j \
                WHERE images.deleted=0 AND n IS NOT NULL ORDER BY n", [])?
            .into_iter().map(|mut r| r.remove("n").unwrap_or(Value::Null)).collect();
        let counts = self
            .q1("SELECT SUM(deleted=0) AS alive, SUM(deleted=1) AS trash, SUM(deleted=0 AND fav=1) AS fav FROM images", [])?
            .unwrap_or_default();
        let mut out = json!({"models": models, "loras": loras});
        for (k, v) in counts {
            out[k] = if truthy(Some(&v)) { v } else { json!(0) };
        }
        Ok(out)
    }

    /// 每張圖花了多久：存進圖庫的時間 − 任務開始跑的時間（排隊不算）。匯入的圖、沒記開始時間的任務不加。
    /// 另外算、不放進 image()／gallery()：那兩個要跟舊版 Python 的輸出逐欄一樣（golden 測試）
    pub fn add_gen_seconds(&self, rows: &mut [Row]) -> DbResult<()> {
        let ids: Vec<i64> = rows.iter().filter_map(|r| r.get("id").and_then(Value::as_i64)).collect();
        if ids.is_empty() {
            return Ok(());
        }
        let secs: HashMap<i64, f64> = self
            .q(
                &format!(
                    "SELECT i.id, i.created - j.started AS s FROM images i JOIN jobs j ON j.id = i.job \
                     WHERE i.id IN ({}) AND j.started IS NOT NULL AND i.created >= j.started",
                    qmarks(ids.len())
                ),
                params_from_iter(&ids),
            )?
            .into_iter()
            .filter_map(|r| Some((r["id"].as_i64()?, r["s"].as_f64()?)))
            .collect();
        for r in rows.iter_mut() {
            if let Some(s) = r.get("id").and_then(Value::as_i64).and_then(|i| secs.get(&i)) {
                r.insert("gen_seconds".into(), json!((s * 10.0).round() / 10.0));
            }
        }
        Ok(())
    }

    pub fn images_by_ids(&self, ids: &[i64]) -> DbResult<Vec<Row>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let mut rows = self.q(
            &format!("SELECT * FROM images WHERE id IN ({})", qmarks(ids.len())),
            params_from_iter(ids),
        )?;
        for r in rows.iter_mut() {
            parse_field(r, "params");
        }
        Ok(rows)
    }

    pub fn known_files(&self) -> DbResult<HashSet<(String, String)>> {
        Ok(self
            .q("SELECT subfolder, filename FROM images", [])?
            .iter()
            .map(|r| (key_of(&r["subfolder"]), key_of(&r["filename"])))
            .collect())
    }

    /// 只看仍在 output 裡的檔案；回收區的同名舊圖不阻止重新匯入。
    pub fn active_files(&self) -> DbResult<HashSet<(String, String)>> {
        Ok(self
            .q("SELECT subfolder, filename FROM images WHERE deleted=0", [])?
            .iter()
            .map(|r| (key_of(&r["subfolder"]), key_of(&r["filename"])))
            .collect())
    }

    // ---------------------------------------------------------------- presets
    pub fn presets(&self) -> DbResult<Vec<Row>> {
        let mut out = self.q("SELECT * FROM presets ORDER BY name", [])?;
        for r in out.iter_mut() {
            let v = r
                .get("data")
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Value::Null);
            r.insert("data".into(), v);
        }
        Ok(out)
    }

    pub fn save_preset(&self, name: &str, data: &Value) -> DbResult<i64> {
        if let Some(ex) = self.q1("SELECT id FROM presets WHERE name=?", [name])? {
            let id = ex["id"].as_i64().unwrap_or(0);
            self.run(
                "UPDATE presets SET data=? WHERE id=?",
                params![dumps(data), id],
            )?;
            return Ok(id);
        }
        self.run(
            "INSERT INTO presets(name, data, created) VALUES(?,?,?)",
            params![name, dumps(data), now()],
        )
    }

    // ---------------------------------------------------------------- 提示詞庫
    /// 全部（不含縮圖本體）：置頂收藏在前，再依自訂順序
    pub fn lib(&self) -> DbResult<Vec<Row>> {
        Ok(self
            .q(&format!("SELECT {LIB_COLS} FROM prompt_lib ORDER BY fav DESC, pos, id"), [])?
            .into_iter()
            .map(lib_row)
            .collect())
    }

    pub fn lib_item(&self, id: i64) -> DbResult<Option<Row>> {
        Ok(self
            .q1(&format!("SELECT {LIB_COLS} FROM prompt_lib WHERE id=?"), [id])?
            .map(lib_row))
    }

    pub fn lib_thumb(&self, id: i64) -> DbResult<Option<String>> {
        Ok(self
            .q1("SELECT thumb FROM prompt_lib WHERE id=?", [id])?
            .and_then(|r| r.get("thumb").and_then(Value::as_str).map(str::to_owned)))
    }

    /// 新增（id 為 None，排到自訂順序最前面）或修改；要改的那筆不存在回傳 None
    pub fn lib_save(&self, id: Option<i64>, it: &LibItem, thumb: &Thumb) -> DbResult<Option<i64>> {
        let mut guard = self.lock();
        // data 和縮圖一起提交；呼叫端收到錯誤時才能安全撤回新複製的檔案。
        let con = guard.transaction()?;
        let (tags, data, t) = (dumps(&json!(it.tags)), dumps(&it.data), now());
        let id = match id {
            Some(id) => {
                let n = con.execute(
                    "UPDATE prompt_lib SET kind=?, name=?, folder=?, tags=?, family=?, fav=?, note=?, data=?, updated=? WHERE id=?",
                    params![it.kind, it.name, it.folder, tags, it.family, it.fav as i64, it.note, data, t, id],
                )?;
                if n == 0 {
                    return Ok(None);
                }
                id
            }
            None => {
                let pos: f64 = con.query_row("SELECT COALESCE(MIN(pos), 0) - 1 FROM prompt_lib", [], |r| r.get(0))?;
                con.execute(
                    "INSERT INTO prompt_lib(kind, name, folder, tags, family, fav, note, data, pos, created, updated) \
                     VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                    params![it.kind, it.name, it.folder, tags, it.family, it.fav as i64, it.note, data, pos, t, t],
                )?;
                con.last_insert_rowid()
            }
        };
        match thumb {
            Thumb::Keep => 0,
            Thumb::Set(b64) => con.execute("UPDATE prompt_lib SET thumb=? WHERE id=?", params![b64, id])?,
            Thumb::Clear => con.execute("UPDATE prompt_lib SET thumb=NULL WHERE id=?", [id])?,
        };
        con.commit()?;
        Ok(Some(id))
    }

    /// 匯入設定檔：同類型、同資料夾、同名的覆蓋，其餘新增
    pub fn lib_import(&self, it: &LibItem, thumb: Option<String>) -> DbResult<i64> {
        let ex = self
            .q1(
                "SELECT id FROM prompt_lib WHERE kind=? AND folder=? AND name=?",
                params![it.kind, it.folder, it.name],
            )?
            .and_then(|r| r["id"].as_i64());
        let th = thumb.map(Thumb::Set).unwrap_or(Thumb::Keep);
        Ok(self.lib_save(ex, it, &th)?.unwrap_or(0))
    }

    /// 匯出設定檔用：連縮圖一起
    pub fn lib_export(&self) -> DbResult<Vec<Row>> {
        Ok(self
            .q("SELECT kind, name, folder, tags, family, fav, pos, note, data, thumb FROM prompt_lib ORDER BY fav DESC, pos, id", [])?
            .into_iter()
            .map(lib_row)
            .collect())
    }

    pub fn lib_used(&self, id: i64) -> DbResult<()> {
        self.run("UPDATE prompt_lib SET used=?, uses=uses+1 WHERE id=?", params![now(), id])?;
        Ok(())
    }

    /// 拖拉排序：照 ids 的順序重新編號（沒列到的維持原本的位置）
    pub fn lib_order(&self, ids: &[i64]) -> DbResult<()> {
        let con = self.lock();
        let tx = con.unchecked_transaction()?;
        for (i, id) in ids.iter().enumerate() {
            tx.execute("UPDATE prompt_lib SET pos=? WHERE id=?", params![i as f64, id])?;
        }
        tx.commit()
    }

    /// 資料夾改名（to 空白＝整個資料夾變回未分類）；回傳搬了幾筆
    pub fn lib_rename_folder(&self, from: &str, to: &str) -> DbResult<usize> {
        let con = self.lock();
        con.execute(
            "UPDATE prompt_lib SET folder=?, updated=? WHERE folder=?",
            params![to, now(), from],
        )
    }

    // ---------------------------------------------------------------- lora meta
    pub fn lora_meta(&self) -> DbResult<Map<String, Value>> {
        Ok(self
            .q("SELECT * FROM lora_meta", [])?
            .into_iter()
            .map(|r| {
                let data = r
                    .get("data")
                    .and_then(Value::as_str)
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or(Value::Null);
                (key_of(&r["name"]), data)
            })
            .collect())
    }

    pub fn set_lora_meta(&self, name: &str, data: &Value) -> DbResult<()> {
        self.run(
            "INSERT OR REPLACE INTO lora_meta(name, data) VALUES(?,?)",
            params![name, dumps(data)],
        )?;
        Ok(())
    }
}

fn columns(con: &Connection, table: &str) -> DbResult<HashSet<String>> {
    let mut st = con.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = st.query_map([], |r| r.get::<_, String>(1))?.collect();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reused_filename_gets_new_record() {
        let dir = std::env::temp_dir().join(format!("anima_db_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let db = Db::open(&path).unwrap();
        let info = |p: &str| json!({"mode": "txt2img", "prompt": p});
        let a = db.add_image("x_00001_.png", "AnimaWeb", "output", &info("old"), &json!({}), Some("job-a")).unwrap();
        // 同一個任務再收一次（websocket＋history）：同一筆
        assert_eq!(db.add_image("x_00001_.png", "AnimaWeb", "output", &info("old"), &json!({}), Some("job-a")).unwrap(), a);
        // 掃描匯入（沒有任務）：同一筆
        assert_eq!(db.add_image("x_00001_.png", "AnimaWeb", "output", &info("?"), &json!({}), None).unwrap(), a);
        // 別的任務重用了檔名：舊的標成遺失，新圖另開一筆
        let b = db.add_image("x_00001_.png", "AnimaWeb", "output", &info("new"), &json!({}), Some("job-b")).unwrap();
        assert_ne!(a, b);
        assert_eq!(db.q1("SELECT deleted FROM images WHERE id=?", [a]).unwrap().unwrap()["deleted"], json!(2));
        let r = db.image(b).unwrap().unwrap();
        assert_eq!((r["prompt"].as_str(), r["job"].as_str()), (Some("new"), Some("job-b")));
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn comic_images_find_pages_and_fixes_of_one_project() {
        let dir = std::env::temp_dir().join(format!("anima_db_comic_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let db = Db::open(&path).unwrap();
        let info = json!({"mode": "txt2img", "prompt": "p"});
        let add = |name: &str, params: Value| db.add_image(name, "AnimaWeb", "output", &info, &params, None).unwrap();
        let page = add("a.png", json!({"comic": {"id": "c1", "kind": "page"}}));
        let fix = add("b.png", json!({"comic": {"id": "c1", "kind": "fix", "from": page}}));
        add("c.png", json!({"comic": {"id": "c1", "kind": "fix_part", "from": page}}));
        add("d.png", json!({"comic": {"id": "c2", "kind": "page"}}));
        add("e.png", json!({}));
        let gone = add("f.png", json!({"comic": {"id": "c1", "kind": "page"}}));
        db.run("UPDATE images SET deleted=1 WHERE id=?", [gone]).unwrap();
        let rows = db.comic_images("c1").unwrap();
        let got: Vec<(i64, String, Option<i64>)> = rows.iter().map(|r| (r["id"].as_i64().unwrap(), r["kind"].as_str().unwrap_or("").to_string(), r["src"].as_i64())).collect();
        assert_eq!(got, [(fix, "fix".into(), Some(page)), (page, "page".into(), None)]);
        // 圖庫「模式」篩選選「漫畫台」：兩個作品的整頁、修好的整頁都算，修到一半的單格（fix_part）和別的圖不算
        let (n, _) = db.gallery(&GalleryQuery { mode: Some("comic".into()), limit: 50, ..Default::default() }).unwrap();
        assert_eq!(n, 3);
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gen_seconds_from_job_start() {
        let dir = std::env::temp_dir().join(format!("anima_db_gs_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let db = Db::open(&path).unwrap();
        db.add_job("job-a", &json!({}), &json!({})).unwrap();
        db.run("UPDATE jobs SET started=? WHERE id='job-a'", [now() - 42.04]).unwrap();
        let a = db.add_image("a.png", "AnimaWeb", "output", &json!({}), &json!({}), Some("job-a")).unwrap();
        // 匯入的圖（沒有任務）、任務還沒記開始時間：都不加
        let b = db.add_image("b.png", "AnimaWeb", "output", &json!({}), &json!({}), None).unwrap();
        db.add_job("job-c", &json!({}), &json!({})).unwrap();
        let c = db.add_image("c.png", "AnimaWeb", "output", &json!({}), &json!({}), Some("job-c")).unwrap();
        let mut rows: Vec<Row> = [a, b, c].iter().map(|&i| db.image(i).unwrap().unwrap()).collect();
        db.add_gen_seconds(&mut rows).unwrap();
        // 從記開始時間到存圖之間也在計時；GitHub 的機器慢，實測會多出 0.3 秒，所以給幾秒的餘裕
        let gs = rows[0]["gen_seconds"].as_f64().unwrap();
        assert!((42.0..45.0).contains(&gs), "{gs}");
        assert!(!rows[1].contains_key("gen_seconds") && !rows[2].contains_key("gen_seconds"));
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prompt_lib_migrates_presets_and_saves() {
        let dir = std::env::temp_dir().join(format!("anima_lib_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        {
            // 舊版資料庫：只有預設組
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE presets (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, data TEXT NOT NULL, created REAL);
                 INSERT INTO presets(name, data, created) VALUES('乙', '{\"prompt\":\"b\"}', 1), ('甲', '{\"prompt\":\"a\"}', 2);",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let lib = db.lib().unwrap();
        assert_eq!(lib.len(), 2);
        assert_eq!(lib[0]["kind"], json!("set"));
        assert_eq!(lib[0]["data"]["prompt"], json!("b"));
        drop(db);
        // 再開一次不會重複抄
        let db = Db::open(&path).unwrap();
        assert_eq!(db.lib().unwrap().len(), 2);
        assert_eq!(db.presets().unwrap().len(), 2);

        let snip = LibItem {
            kind: "snippet".into(),
            name: "夕陽".into(),
            folder: "光影".into(),
            tags: vec!["暖色".into()],
            family: String::new(),
            fav: false,
            note: String::new(),
            data: json!({"text": "sunset"}),
        };
        let id = db.lib_save(None, &snip, &Thumb::Set("AAAA".into())).unwrap().unwrap();
        let lib = db.lib().unwrap();
        // 新的排最前面、有縮圖、標籤是陣列
        assert_eq!(lib[0]["id"], json!(id));
        assert_eq!(lib[0]["has_thumb"], json!(1));
        assert_eq!(lib[0]["tags"], json!(["暖色"]));
        assert_eq!(db.lib_thumb(id).unwrap().as_deref(), Some("AAAA"));
        // 改：縮圖拿掉、收藏後排最前
        let (yi, other) = (lib[1]["id"].as_i64().unwrap(), lib[2]["id"].as_i64().unwrap());
        let mut fav = snip.clone();
        fav.fav = true;
        db.lib_save(Some(id), &fav, &Thumb::Clear).unwrap();
        assert_eq!(db.lib_item(id).unwrap().unwrap()["has_thumb"], json!(0));
        assert!(db.lib_save(Some(999), &fav, &Thumb::Keep).unwrap().is_none());
        db.lib_order(&[other, id, yi]).unwrap();
        db.lib_used(other).unwrap();
        let lib = db.lib().unwrap();
        assert_eq!(lib[0]["id"], json!(id));
        assert_eq!(lib[1]["id"], json!(other));
        assert_eq!(lib[1]["uses"], json!(1));
        // 匯入：同類型、同資料夾、同名的覆蓋
        let mut imp = snip.clone();
        imp.data = json!({"text": "sunset, orange sky"});
        assert_eq!(db.lib_import(&imp, None).unwrap(), id);
        assert_eq!(db.lib_item(id).unwrap().unwrap()["data"]["text"], json!("sunset, orange sky"));
        imp.folder = "別的".into();
        assert_ne!(db.lib_import(&imp, None).unwrap(), id);
        assert_eq!(db.lib_rename_folder("光影", "燈光").unwrap(), 1);
        assert_eq!(db.lib_item(id).unwrap().unwrap()["folder"], json!("燈光"));
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
