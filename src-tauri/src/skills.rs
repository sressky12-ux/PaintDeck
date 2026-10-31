//! 外掛寫法（2026-10-29）：使用者自備的提示詞 skill（SKILL.md＋references），給生圖助手和 AI 改寫照模型家族用。
//! 使用者原話：「話說，能不能。讓站裡面的llm也用。」「照這個設計做外掛寫法」
//!
//! 網路上的 skill 幾萬到幾十萬字，9B 的上下文只有 16K，整份塞不下，所以切成小段：
//! 給目錄、照需求挑相關的段，助手還能用 guide 工具按段讀（guide {"name":"moss","part":7}）。
//! 程式和安裝包不帶任何 skill 內容（授權各自不同，例如 CC BY-NC-ND 不准改寫後散布），只讀 <data>\skills\<id>\。
//! 綁定存在 settings.json 的 skill_bind：{"<寫法名>": "<skill id>"}，寫法名跟 agent::GUIDES 一樣。
use crate::tags::fold_zh;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// 一段最多幾 token（估的）：9B 一次讀一段不會把對話擠掉
pub const PART_TOKENS: usize = 1500;
/// AI 改寫、只寫詞一次挑多少（它們不能查，一次給夠）
pub const PICK_WRITE: usize = 2400;
/// 助手每輪挑多少（不夠它可以自己用 guide 工具讀）
pub const PICK_AGENT: usize = 1500;
pub const BIND_KEY: &str = "skill_bind";
/// 從 GitHub 裝：最多幾個檔、總共多大
pub const MAX_FILES: usize = 300;
pub const MAX_BYTES: u64 = 3 << 20;
const TOC_LINES: usize = 40;
const TITLE_CHARS: usize = 40;
/// 核心段：SKILL.md 第一個至少這麼長的節（更短的多半只有標題）
const CORE_CHARS: usize = 200;
const SKILL_MD: &str = "SKILL.md";
/// 不當成 skill 內容的檔（給人看的說明、版本紀錄、授權）
const SKIP_STEMS: &[&str] = &["readme", "changelog", "license", "licence", "copying", "notice", "contributing", "code_of_conduct", "security"];
/// 交給模型時的說明：skill 是給別的 AI 寫的，格式、參數、工具那些不要照做
const FORMAT_NOTE: &str = "Use its rules for what you write. Keep the reply format your instructions require; skip the skill's own rules about output format, sampling settings, tools and scripts.";
/// 請求裡的虛字：連續漢字在這些字上斷開，免得「光的」「的写」這種跨詞的兩字組去撞到內文
const FILLER: &str = "的了我你他她它们帮给要想把让和与并在是有个张这那吗呢吧啊着被将从向也都还就很不没又而则或为以到之";
/// 挑段：分數不到第一名這個比例的不挑
const MIN_RATIO: f64 = 0.3;
/// 挑段：標題撞到的加分（標題是作者自己寫的這節摘要，比內文碰巧撞到可靠）
const TITLE_BONUS: f64 = 8.0;
/// 挑段：同一節其他段裡的證據算幾成（一節被切成好幾段，每段只握有一部分）
const SECTION_SHARE: f64 = 0.5;
/// 挑段：一個詞的 IDF 上限。兩字組會跨詞（「舌头回」的「头回」），只在內文出現一次的碰巧組合不該壓過整節都在講的詞
const IDF_CAP: f64 = 2.0;
/// 挑段時不算的兩字組（請求裡的客套話）
const STOP: &[&str] = &["帮我", "给我", "一个", "一张", "画一", "我要", "我想", "可以", "这个", "那个", "一下", "然后"];

pub struct Part {
    pub file: String,
    pub section: usize,
    pub title: String,
    pub text: String,
    pub tokens: usize,
}

pub struct Section {
    pub file: String,
    pub title: String,
    /// 這節的第一段和最後一段（parts 的索引，從 0 起）
    pub first: usize,
    pub last: usize,
}

pub struct Skill {
    pub id: String,
    /// skill 資料夾（意思座標的快取 .vectors.json 放這裡，見 embed.rs）
    pub dir: PathBuf,
    pub name: String,
    pub desc: String,
    /// source.json（從 GitHub 裝的才有：網址、庫、授權、安裝時間）
    pub source: Value,
    pub files: usize,
    pub parts: Vec<Part>,
    pub sections: Vec<Section>,
}

/// 估 token：全形字一個算 1，其他 4 個字元算 1（寧可高估，免得塞爆 9B）
pub fn tokens(s: &str) -> usize {
    let (mut wide, mut narrow) = (0usize, 0usize);
    for c in s.chars() {
        if wide_char(c) {
            wide += 1;
        } else {
            narrow += 1;
        }
    }
    wide + narrow.div_ceil(4)
}

fn wide_char(c: char) -> bool {
    c as u32 >= 0x2E80
}

fn cjk(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x9FFF | 0xF900..=0xFAFF)
}

fn clip(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n).collect::<String>() + "…"
}

/// 目錄、段落標題用：拿掉 Markdown 記號
fn plain(s: &str) -> String {
    s.replace("**", "").replace('`', "").trim().to_string()
}

pub fn root(data: &Path) -> PathBuf {
    data.join("skills")
}

/// 資料夾名稱能不能當 skill id（不能跳出 skills 資料夾；點開頭的是安裝到一半的暫存）
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && !id.starts_with('.') && !id.contains(['/', '\\', ':']) && id.len() <= 120
}

/// 安裝時的資料夾名稱：英數、- _ . 以外換成 -
pub fn safe_id(s: &str) -> String {
    let t: String = s.trim().chars().map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '-' }).collect();
    let t = t.trim_matches(['-', '.']).chars().take(60).collect::<String>();
    if t.is_empty() { "skill".into() } else { t }
}

/// SKILL.md 開頭的 --- 區塊：(name, description, 正文)。description 可能是 >- 摺疊的好幾行
pub fn front_matter(text: &str) -> (String, String, &str) {
    let t = text.trim_start_matches('\u{feff}');
    let first_end = t.find('\n').map(|i| i + 1).unwrap_or(t.len());
    if t[..first_end].trim() != "---" {
        return (String::new(), String::new(), t);
    }
    let after = &t[first_end..];
    let mut pos = 0;
    let mut body = None;
    for line in after.split_inclusive('\n') {
        if line.trim() == "---" {
            body = Some(pos + line.len());
            break;
        }
        pos += line.len();
    }
    let Some(b) = body else {
        return (String::new(), String::new(), t);
    };
    let mut keys: Map<String, Value> = Map::new();
    let mut folding: Option<String> = None;
    for line in after[..pos].lines() {
        if let Some(k) = &folding {
            if line.starts_with([' ', '\t']) || line.trim().is_empty() {
                let v = keys.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                let joined = format!("{v} {}", line.trim());
                keys.insert(k.clone(), json!(joined.trim()));
                continue;
            }
            folding = None;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        if line.starts_with([' ', '\t']) {
            continue;
        }
        let (k, v) = (k.trim().to_string(), v.trim());
        if matches!(v, ">" | ">-" | ">+" | "|" | "|-" | "|+") {
            keys.insert(k.clone(), json!(""));
            folding = Some(k);
        } else {
            keys.insert(k, json!(v.trim_matches(['"', '\''])));
        }
    }
    let get = |k: &str| keys.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    (get("name"), get("description"), &after[b..])
}

fn fence(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("```") || t.starts_with("~~~")
}

/// 照 `## ` 切節（``` 區塊裡的不算）：(標題, 內容含標題行)。第一個 `## ` 之前那段的標題是 `# ` 那行（沒有就空字串）
fn sections_of(body: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![(String::new(), String::new())];
    let mut inside = false;
    for line in body.split_inclusive('\n') {
        let l = line.trim_end();
        if fence(l) {
            inside = !inside;
        } else if !inside {
            if let Some(h) = l.strip_prefix("## ") {
                out.push((plain(h), String::new()));
            } else if let Some(h) = l.strip_prefix("# ").filter(|_| out.len() == 1 && out[0].0.is_empty()) {
                out[0].0 = plain(h);
            }
        }
        if let Some(last) = out.last_mut() {
            last.1.push_str(line);
        }
    }
    out.retain(|(_, b)| !b.trim().is_empty());
    out
}

/// 一節太長就切：照段落（空行；``` 區塊不拆）裝箱，一個段落還太長就照行切，一行還太長就照字切
fn pack(text: &str, budget: usize) -> Vec<String> {
    if tokens(text) <= budget {
        return vec![text.to_string()];
    }
    let mut paras = vec![];
    let (mut cur, mut inside) = (String::new(), false);
    for line in text.split_inclusive('\n') {
        if fence(line) {
            inside = !inside;
        }
        cur.push_str(line);
        if !inside && line.trim().is_empty() {
            paras.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        paras.push(cur);
    }
    let mut pieces = vec![];
    for p in paras {
        if tokens(&p) <= budget {
            pieces.push(p);
            continue;
        }
        for line in p.split_inclusive('\n') {
            if tokens(line) <= budget {
                pieces.push(line.to_string());
                continue;
            }
            // 四分之一 token 為單位：全形字 4、其他 1
            let (mut s, mut n) = (String::new(), 0);
            for c in line.chars() {
                let w = if wide_char(c) { 4 } else { 1 };
                if n + w > budget * 4 {
                    pieces.push(std::mem::take(&mut s));
                    n = 0;
                }
                s.push(c);
                n += w;
            }
            pieces.push(s);
        }
    }
    let mut out = vec![];
    let mut cur = String::new();
    for p in pieces {
        if !cur.trim().is_empty() && tokens(&cur) + tokens(&p) > budget {
            out.push(std::mem::take(&mut cur));
        }
        cur.push_str(&p);
    }
    if cur.trim().is_empty() {
        if let Some(last) = out.last_mut() {
            last.push_str(&cur);
        }
    } else {
        out.push(cur);
    }
    out
}

fn skip_file(name: &str) -> bool {
    let low = name.to_lowercase();
    let stem = low.rsplit_once('.').map(|(s, _)| s).unwrap_or(&low);
    SKIP_STEMS.contains(&stem) || stem.starts_with("licen")
}

fn text_file(name: &str) -> bool {
    let low = name.to_lowercase();
    low.ends_with(".md") || low.ends_with(".txt")
}

/// skill 的內容檔：(相對路徑用 /, 完整路徑)，SKILL.md 排第一、其他照路徑排
fn skill_files(dir: &Path) -> Vec<(String, PathBuf)> {
    fn walk(dir: &Path, rel: &str, depth: usize, out: &mut Vec<(String, PathBuf)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let p = e.path();
            if p.is_dir() {
                if depth < 6 {
                    walk(&p, &r, depth + 1, out);
                }
            } else if text_file(&name) && !skip_file(&name) {
                out.push((r, p));
            }
        }
    }
    let mut out = vec![];
    walk(dir, "", 0, &mut out);
    out.sort_by_key(|(r, _)| (!r.eq_ignore_ascii_case(SKILL_MD), r.to_lowercase()));
    out
}

/// 讀一個 skill 資料夾（沒有 SKILL.md 就不是）
pub fn load(dir: &Path) -> Option<Skill> {
    let id = dir.file_name()?.to_str()?.to_string();
    if !valid_id(&id) {
        return None;
    }
    let files = skill_files(dir);
    if !files.first().is_some_and(|(r, _)| r.eq_ignore_ascii_case(SKILL_MD)) {
        return None;
    }
    let source = std::fs::read(dir.join("source.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
    let mut sk = Skill { id: id.clone(), dir: dir.to_path_buf(), name: String::new(), desc: String::new(), source, files: 0, parts: vec![], sections: vec![] };
    let mut read = 0u64;
    for (i, (rel, path)) in files.iter().enumerate() {
        // 整個資料夾讀超過上限（有人放了一大包資料進來）就停，前面讀到的照用
        if read > MAX_BYTES * 2 {
            break;
        }
        let Ok(raw) = std::fs::read_to_string(path) else { continue };
        read += raw.len() as u64;
        sk.files += 1;
        let body = if i == 0 {
            let (n, d, b) = front_matter(&raw);
            sk.name = n;
            sk.desc = d;
            b.to_string()
        } else {
            raw
        };
        for (title, text) in sections_of(&body) {
            let title = match title.is_empty() {
                false => title,
                true if i == 0 => SKILL_MD.to_string(),
                true => rel.clone(),
            };
            let first = sk.parts.len();
            for t in pack(&text, PART_TOKENS) {
                sk.parts.push(Part { file: rel.clone(), section: sk.sections.len(), title: title.clone(), tokens: tokens(&t), text: t });
            }
            if sk.parts.len() > first {
                sk.sections.push(Section { file: rel.clone(), title, first, last: sk.parts.len() - 1 });
            }
        }
    }
    if sk.name.trim().is_empty() {
        sk.name = id;
    }
    Some(sk)
}

/// skills 資料夾裡的全部 skill（照 id 排）
pub fn list(root: &Path) -> Vec<Skill> {
    let Ok(rd) = std::fs::read_dir(root) else { return vec![] };
    let mut out: Vec<Skill> = rd.flatten().filter(|e| e.path().is_dir()).filter_map(|e| load(&e.path())).collect();
    out.sort_by_key(|a| a.id.to_lowercase());
    out
}

pub fn find(root: &Path, id: &str) -> Option<Skill> {
    valid_id(id).then(|| load(&root.join(id))).flatten()
}

pub fn bound_id(s: &Map<String, Value>, guide: &str) -> Option<String> {
    s.get(BIND_KEY)?.get(guide)?.as_str().map(str::trim).filter(|v| !v.is_empty()).map(str::to_string)
}

/// 這份寫法接的外掛 skill（沒接、資料夾不見了都是 None，當作沒接）
pub fn for_guide(data: &Path, settings_path: &Path, guide: &str) -> Option<Skill> {
    let id = bound_id(&crate::settings::load(settings_path), guide)?;
    find(&root(data), &id)
}

/// 挑段用的詞：連續漢字的兩字組＋三個字母以上的英文字（都先折成簡體、小寫，skill 多半是簡體）
fn terms(q: &str) -> Vec<String> {
    let f = fold_zh(&q.to_lowercase());
    let mut out: Vec<String> = vec![];
    let (mut run, mut word): (Vec<char>, String) = (vec![], String::new());
    for c in f.chars().chain(std::iter::once(' ')) {
        if cjk(c) && !FILLER.contains(c) {
            run.push(c);
        } else {
            out.extend(run.windows(2).map(|w| w.iter().collect::<String>()));
            run.clear();
        }
        if c.is_ascii_alphanumeric() {
            word.push(c);
        } else {
            if word.len() >= 3 {
                out.push(word.clone());
            }
            word.clear();
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|t| !STOP.contains(&t.as_str()) && seen.insert(t.clone()));
    out
}

impl Skill {
    fn file0(&self) -> &str {
        self.parts.first().map(|p| p.file.as_str()).unwrap_or(SKILL_MD)
    }

    pub fn total_tokens(&self) -> usize {
        self.parts.iter().map(|p| p.tokens).sum()
    }

    /// 目錄：一節一行「7-8 · 標題」，別的檔的節前面帶檔名
    pub fn toc(&self) -> String {
        let mut lines: Vec<String> = self
            .sections
            .iter()
            .take(TOC_LINES)
            .map(|s| {
                let range = if s.first == s.last { format!("{}", s.first + 1) } else { format!("{}-{}", s.first + 1, s.last + 1) };
                let file = if s.file == self.file0() { String::new() } else { format!("{}: ", s.file) };
                format!("{range} · {file}{}", clip(&s.title, TITLE_CHARS))
            })
            .collect();
        if self.sections.len() > TOC_LINES {
            let rest = &self.sections[TOC_LINES..];
            lines.push(format!("… {} more sections in parts {}-{}", rest.len(), rest[0].first + 1, self.parts.len()));
        }
        lines.join("\n")
    }

    /// 照需求挑段（回索引，照原本順序）：核心段先放（不超過額度一半才放），再依分數高到低裝到額度
    pub fn pick(&self, query: &str, budget: usize) -> Vec<usize> {
        self.pick_by(None, query, budget)
    }

    /// 給模型看的一段：標題＋內文（算意思座標用，embed.rs）
    pub fn doc_text(&self, i: usize) -> String {
        format!("{}\n{}", self.parts[i].title, self.parts[i].text.trim())
    }

    /// 同 pick；sem＝意思座標排好的分數（embed.rs 算的，高到低）。沒有就照關鍵字（ranked）
    pub fn pick_by(&self, sem: Option<&[(f64, usize)]>, query: &str, budget: usize) -> Vec<usize> {
        let mut chosen = vec![];
        let mut used = 0;
        let core = self.sections.iter().find(|s| {
            s.file == self.file0() && self.parts[s.first..=s.last].iter().map(|p| p.text.trim().chars().count()).sum::<usize>() >= CORE_CHARS
        });
        if let Some(s) = core {
            if self.parts[s.first].tokens <= budget / 2 {
                chosen.push(s.first);
                used += self.parts[s.first].tokens;
            }
        }
        // 關鍵字：分數不到第一名三成的多半只是撞到幾個常見字，不挑，免得把額度花在不相干的段。
        // 意思座標每段都有分數（相似度），照排名裝到額度為止
        let (ranked, ratio) = match sem {
            Some(r) => (r.iter().copied().filter(|(_, i)| *i < self.parts.len()).collect::<Vec<_>>(), 0.0),
            None => (self.ranked(query), MIN_RATIO),
        };
        let floor = ranked.first().map(|(s, _)| s * ratio).unwrap_or(0.0);
        for (s, i) in ranked {
            if s >= floor && !chosen.contains(&i) && used + self.parts[i].tokens <= budget {
                chosen.push(i);
                used += self.parts[i].tokens;
            }
        }
        chosen.sort_unstable();
        chosen
    }

    /// 每一段對這個請求的分數（高到低，沒撞到任何詞的不列）。
    /// 詞出現在越少段，越能分辨該挑哪段（IDF），所以「光的」這種到處都有的組合不會壓過「逆光」；標題撞到的加重
    pub fn ranked(&self, query: &str) -> Vec<(f64, usize)> {
        let ts = terms(query);
        if ts.is_empty() {
            return vec![];
        }
        let bodies: Vec<String> = self.parts.iter().map(|p| fold_zh(&p.text.to_lowercase())).collect();
        let titles: Vec<String> = self.parts.iter().map(|p| fold_zh(&p.title.to_lowercase())).collect();
        let n = self.parts.len() as f64;
        // counts[段][詞]：詞在這段內文出現幾次；sec_total[節][詞]：整節加起來（一節被切成好幾段時，每段只拿到一部分證據）
        let counts: Vec<Vec<usize>> = bodies.iter().map(|b| ts.iter().map(|t| b.matches(t.as_str()).count()).collect()).collect();
        let mut sec_total = vec![vec![0usize; ts.len()]; self.sections.len()];
        for (p, c) in self.parts.iter().zip(&counts) {
            for (j, k) in c.iter().enumerate() {
                sec_total[p.section][j] += k;
            }
        }
        let idf: Vec<f64> = (0..ts.len())
            .map(|j| {
                let df = (0..self.parts.len()).filter(|&i| counts[i][j] > 0 || titles[i].contains(ts[j].as_str())).count() as f64;
                (1.0 + (n - df + 0.5) / (df + 0.5)).ln().min(IDF_CAP)
            })
            .collect();
        let mut out: Vec<(f64, usize)> = self
            .parts
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let mut s = 0.0;
                for (j, w) in idf.iter().enumerate() {
                    let own = counts[i][j].min(3) as f64;
                    let rest = (sec_total[p.section][j] - counts[i][j]).min(3) as f64;
                    let title = if titles[i].contains(ts[j].as_str()) { TITLE_BONUS } else { 0.0 };
                    s += w * (own + SECTION_SHARE * rest + title);
                }
                (s / (1.0 + p.tokens as f64 / 1000.0), i)
            })
            .filter(|(s, _)| *s > 0.0)
            .collect();
        out.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        out
    }

    fn header(&self) -> String {
        let desc = if self.desc.trim().is_empty() { String::new() } else { format!(": {}", clip(&self.desc, 200)) };
        format!("EXTRA SKILL \"{}\" (the user attached it to this model{desc}).", self.name)
    }

    fn parts_text(&self, idx: &[usize]) -> String {
        idx.iter().map(|&i| format!("[part {} · {}]\n{}", i + 1, clip(&self.parts[i].title, TITLE_CHARS), self.parts[i].text.trim())).collect::<Vec<_>>().join("\n\n")
    }

    /// AI 改寫、只寫詞：照需求挑的段（它們不能叫工具，所以不給目錄）。挑不到就空字串
    pub fn pick_block(&self, query: &str, budget: usize) -> String {
        self.pick_block_by(None, query, budget)
    }

    pub fn pick_block_by(&self, sem: Option<&[(f64, usize)]>, query: &str, budget: usize) -> String {
        let idx = self.pick_by(sem, query, budget);
        if idx.is_empty() {
            return String::new();
        }
        format!("{} {FORMAT_NOTE}\n\n{}", self.header(), self.parts_text(&idx))
    }

    /// 助手：挑好的段＋目錄（別的段用 guide 工具讀）
    pub fn agent_block(&self, guide: &str, query: &str) -> String {
        self.agent_block_by(guide, query, None)
    }

    pub fn agent_block_by(&self, guide: &str, query: &str, sem: Option<&[(f64, usize)]>) -> String {
        let idx = self.pick_by(sem, query, PICK_AGENT);
        let mut s = format!("{} {FORMAT_NOTE}", self.header());
        if !idx.is_empty() {
            s += "\nParts picked for this request:\n\n";
            s += &self.parts_text(&idx);
        }
        s += &format!(
            "\n\nContents of the skill ({} parts). Read another part with {{\"action\":\"call\",\"tool\":\"guide\",\"args\":{{\"name\":\"{guide}\",\"part\":N}}}}:\n{}",
            self.parts.len(),
            self.toc()
        );
        s
    }

    /// guide 工具讀第 n 段（從 1 起）
    pub fn part_reply(&self, n: usize) -> Result<String, String> {
        let p = n.checked_sub(1).and_then(|i| self.parts.get(i)).ok_or_else(|| format!("外掛寫法「{}」只有第 1～{} 段", self.name, self.parts.len()))?;
        let sec = &self.sections[p.section];
        let of = if sec.first == sec.last { String::new() } else { format!(" ({}/{})", n - sec.first, sec.last - sec.first + 1) };
        Ok(format!("Extra skill \"{}\", part {n} of {} · {}{of}. {FORMAT_NOTE}\n\n{}", self.name, self.parts.len(), clip(&p.title, TITLE_CHARS), p.text.trim()))
    }

    pub fn summary(&self) -> Value {
        json!({"id": self.id, "name": self.name, "desc": self.desc, "source": self.source, "files": self.files,
               "parts": self.parts.len(), "sections": self.sections.len(), "tokens": self.total_tokens()})
    }
}

// ---------------------------------------------------------------- 從 GitHub 裝
#[derive(Debug, PartialEq, Eq)]
pub struct GhRef {
    pub owner: String,
    pub repo: String,
    pub branch: Option<String>,
    /// 庫裡的資料夾（"" ＝ 根目錄）
    pub path: String,
}

/// github.com/<owner>/<repo>[/tree|blob/<分支>/<路徑>]；blob 指到檔案就取它的資料夾
pub fn parse_github(url: &str) -> Option<GhRef> {
    let u = url.trim();
    let rest = u.strip_prefix("https://").or_else(|| u.strip_prefix("http://")).unwrap_or(u);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let rest = rest.strip_prefix("github.com/")?;
    let rest = rest.split(['?', '#']).next()?;
    let seg: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    let owner = seg.first().copied().filter(|s| ok(s))?;
    let repo = seg.get(1).map(|s| s.trim_end_matches(".git")).filter(|s| ok(s))?;
    let (branch, mut path) = match seg.get(2).copied() {
        Some("tree" | "blob") => (seg.get(3).map(|s| s.to_string()), seg.get(4..).map(|s| s.join("/")).unwrap_or_default()),
        _ => (None, String::new()),
    };
    if seg.get(2) == Some(&"blob") && path.rsplit('/').next().is_some_and(|f| f.contains('.')) {
        path = path.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
    }
    Some(GhRef { owner: owner.into(), repo: repo.into(), branch, path })
}

/// 庫裡要裝哪一份 SKILL.md（回它的資料夾，"" ＝ 根目錄）。點開頭的資料夾（.agents 之類工具自帶的）不算
pub fn choose_skill(paths: &[String], want: &str) -> Result<String, String> {
    let want = want.trim_matches('/');
    let mut dirs: Vec<String> = paths
        .iter()
        .filter(|p| {
            let segs: Vec<&str> = p.split('/').collect();
            segs.last().is_some_and(|f| f.eq_ignore_ascii_case(SKILL_MD)) && !segs.iter().any(|s| s.starts_with('.'))
        })
        .map(|p| p.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default())
        .filter(|d| want.is_empty() || d == want || d.starts_with(&format!("{want}/")))
        .collect();
    dirs.sort_by_key(|d| (if d.is_empty() { 0 } else { d.matches('/').count() + 1 }, d.clone()));
    if dirs.iter().any(|d| d == want) {
        return Ok(want.to_string());
    }
    match dirs.len() {
        0 if want.is_empty() => Err("這個庫裡找不到 SKILL.md，它可能不是 skill".into()),
        0 => Err(format!("「{want}」底下找不到 SKILL.md")),
        1 => Ok(dirs.remove(0)),
        _ => Err(format!(
            "這個庫裡有好幾份 skill，請貼其中一份的網址（…/tree/分支/資料夾）：{}",
            dirs.iter().take(8).map(String::as_str).collect::<Vec<_>>().join("、")
        )),
    }
}

/// 要下載的檔（庫裡的路徑, 大小）：那個資料夾底下的 .md／.txt（README、CHANGELOG 不要），加上資料夾第一層的授權檔
pub fn wanted_files(tree: &[(String, u64)], dir: &str) -> Result<Vec<(String, u64)>, String> {
    let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
    let mut out = vec![];
    for (p, size) in tree {
        let Some(rel) = p.strip_prefix(&prefix) else { continue };
        if rel.split('/').any(|s| s.starts_with('.') || s == "..") {
            continue;
        }
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let license = !rel.contains('/') && license_file(name);
        if license || (text_file(name) && !skip_file(name)) {
            out.push((p.clone(), *size));
        }
    }
    let total: u64 = out.iter().map(|(_, s)| s).sum();
    if out.len() > MAX_FILES || total > MAX_BYTES {
        return Err(format!("檔案太多（{} 個、{:.1} MB），這不像提示詞 skill，沒有下載", out.len(), total as f64 / 1048576.0));
    }
    Ok(out)
}

pub fn license_file(name: &str) -> bool {
    let low = name.to_lowercase();
    let stem = low.split('.').next().unwrap_or("");
    stem.starts_with("licen") || stem == "copying"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("anima_skills_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const MOSSY: &str = "---\nname: moss-test\ndescription: >-\n  Compiles prompts for MOSS-Image\n  across three modes.\nlicense: CC-BY-NC-ND-4.0\n---\n\n# MOSS 提示词 SKILL\n\n## 0. 任务定义与默认参数\n\n你的任务：把用户的需求写成一对提示词。这一节说明任务、模型和默认参数，至少要有两百个字才会被当成核心规则，所以这里多写一些说明：分辨率、步数、CFG 都在这里讲清楚，之后每一节都按这里的定义执行，不能跳步，也不能自创本文件没有列出的手法。再补几句凑够长度：写实、二次元、跨次元三种模式都从这一节开始判断，用户没说的时候默认写实。这里还要说明输出前的自检：逐条检查年龄、肢体、文字是否正确，确认没有违反禁用清单。\n\n## 3. 确定图片媒介与场景光影\n\n写实照片要写出逆光时人物正面的暗部，防止正面发白。\n\n```text\n## 这不是标题\n```\n\n## 7. 多人同框时按位置分区\n\n双人同框要写清楚左右位置和发色差异，防止画成双胞胎。\n";

    #[test]
    fn front_matter_folds_description() {
        let (n, d, body) = front_matter(MOSSY);
        assert_eq!((n.as_str(), d.as_str()), ("moss-test", "Compiles prompts for MOSS-Image across three modes."));
        assert!(body.trim_start().starts_with("# MOSS"));
        let (n, d, body) = front_matter("# 沒有 frontmatter\n內容");
        assert!(n.is_empty() && d.is_empty() && body.starts_with("# 沒有"));
    }

    #[test]
    fn sections_skip_headings_inside_code() {
        let (_, _, body) = front_matter(MOSSY);
        let s = sections_of(body);
        let titles: Vec<&str> = s.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(titles, ["MOSS 提示词 SKILL", "0. 任务定义与默认参数", "3. 确定图片媒介与场景光影", "7. 多人同框时按位置分区"]);
        assert!(s[2].1.contains("## 这不是标题"));
    }

    #[test]
    fn long_sections_split_under_the_budget() {
        let para = "写实照片要写出逆光时人物正面的暗部。".repeat(30);
        let text = format!("## 长\n\n{para}\n\n{para}\n\n```\n{para}\n```\n\n{}\n", "一".repeat(500));
        let parts = pack(&text, 300);
        assert!(parts.len() > 3, "{}", parts.len());
        assert!(parts.iter().all(|p| tokens(p) <= 300), "{:?}", parts.iter().map(|p| tokens(p)).collect::<Vec<_>>());
        assert_eq!(parts.concat(), text);
        assert_eq!(tokens("abcd中文"), 3);
    }

    #[test]
    fn load_reads_skill_folder_and_builds_contents() {
        let d = tmp("load").join("moss-test");
        std::fs::create_dir_all(d.join("references")).unwrap();
        std::fs::write(d.join("SKILL.md"), MOSSY).unwrap();
        std::fs::write(d.join("README.md"), "## 給人看的\n不要收").unwrap();
        std::fs::write(d.join("LICENSE"), "CC BY-NC-ND").unwrap();
        std::fs::write(d.join("references").join("comic.md"), "# 漫画\n\n## 气泡归属\n\n气泡尾巴要指向说话的人。\n").unwrap();
        std::fs::create_dir_all(d.parent().unwrap().join(".installing")).unwrap();
        let sk = load(&d).unwrap();
        assert_eq!((sk.name.as_str(), sk.files), ("moss-test", 2));
        assert!(!sk.parts.iter().any(|p| p.text.contains("不要收")));
        let toc = sk.toc();
        assert!(toc.contains("2 · 0. 任务定义与默认参数") && toc.contains("references/comic.md: 气泡归属"), "{toc}");
        assert_eq!(list(d.parent().unwrap()).len(), 1);
        assert!(find(d.parent().unwrap(), "../moss-test").is_none());
        assert!(sk.part_reply(99).is_err());
        assert!(sk.part_reply(4).unwrap().contains("双人同框"));
        let _ = std::fs::remove_dir_all(d.parent().unwrap());
    }

    #[test]
    fn traditional_request_picks_simplified_parts_and_keeps_core() {
        let d = tmp("pick").join("moss-test");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("SKILL.md"), MOSSY).unwrap();
        let sk = load(&d).unwrap();
        let picked: Vec<&str> = sk.pick("幫我畫兩個女生同框，一個金髮一個黑髮", PICK_WRITE).iter().map(|&i| sk.parts[i].title.as_str()).collect();
        assert_eq!(picked, ["0. 任务定义与默认参数", "7. 多人同框时按位置分区"]);
        let block = sk.pick_block("逆光的寫實照片", PICK_WRITE);
        assert!(block.starts_with("EXTRA SKILL \"moss-test\"") && block.contains("正面发白") && block.contains("Keep the reply format"));
        assert!(!block.contains("双胞胎"));
        // 額度太小放不下核心段：只放挑中的
        assert_eq!(sk.pick("逆光", 100).len(), 1);
        let agent = sk.agent_block("moss", "逆光");
        assert!(agent.contains(r#""args":{"name":"moss","part":N}"#) && agent.contains("Contents of the skill"));
        let _ = std::fs::remove_dir_all(d.parent().unwrap());
    }

    #[test]
    fn github_urls_and_skill_choice() {
        let g = parse_github("https://github.com/Moeblack/moss-image-prompt-skill").unwrap();
        assert_eq!((g.owner.as_str(), g.repo.as_str(), g.branch.clone(), g.path.as_str()), ("Moeblack", "moss-image-prompt-skill", None, ""));
        let g = parse_github("github.com/iamyoki/qwen-image-2.1-skill/tree/main/skills/qwen-image-2-1-prompter/").unwrap();
        assert_eq!((g.branch.as_deref(), g.path.as_str()), (Some("main"), "skills/qwen-image-2-1-prompter"));
        let g = parse_github("https://github.com/OpenMOSS/MOSS-Image/blob/main/skills/moss-image-prompt-enhancer/SKILL.md").unwrap();
        assert_eq!(g.path, "skills/moss-image-prompt-enhancer");
        assert!(parse_github("https://gitlab.com/a/b").is_none());
        let paths: Vec<String> = [".agents/skills/skill-creator/SKILL.md", "skills/qwen/SKILL.md", "skills/qwen/references/a.md"].map(String::from).to_vec();
        assert_eq!(choose_skill(&paths, "").unwrap(), "skills/qwen");
        let two: Vec<String> = ["a/SKILL.md", "b/SKILL.md"].map(String::from).to_vec();
        assert!(choose_skill(&two, "").unwrap_err().contains("a、b"));
        assert_eq!(choose_skill(&two, "b").unwrap(), "b");
        let root: Vec<String> = ["SKILL.md", "examples/SKILL.md"].map(String::from).to_vec();
        assert_eq!(choose_skill(&root, "").unwrap(), "");
        let tree: Vec<(String, u64)> = [("SKILL.md", 100), ("README.md", 10), ("LICENSE", 5), ("references/x.md", 20), ("examples/a.png", 9000), (".github/y.md", 1)]
            .map(|(p, s)| (p.to_string(), s))
            .to_vec();
        let files: Vec<String> = wanted_files(&tree, "").unwrap().into_iter().map(|(p, _)| p).collect();
        assert_eq!(files, ["SKILL.md", "LICENSE", "references/x.md"]);
        assert!(wanted_files(&[("SKILL.md".into(), MAX_BYTES + 1)], "").is_err());
        assert_eq!(safe_id("moss image/prompt"), "moss-image-prompt");
    }

    #[test]
    fn bindings_come_from_settings() {
        let s: Map<String, Value> = serde_json::from_value(json!({"skill_bind": {"moss": "moss-test", "anima": ""}})).unwrap();
        assert_eq!(bound_id(&s, "moss").as_deref(), Some("moss-test"));
        assert!(bound_id(&s, "anima").is_none() && bound_id(&s, "qwen").is_none());
    }

    #[test]
    fn terms_break_on_filler_characters() {
        let t = terms("幫我畫逆光的寫實人像照片");
        assert!(t.contains(&"逆光".to_string()) && t.contains(&"写实".to_string()) && t.contains(&"人像".to_string()), "{t:?}");
        // 「光的」「的写」跨過虛字的組合不能出現，不然會撞到內文裡不相干的句子
        assert!(!t.contains(&"光的".to_string()) && !t.contains(&"的写".to_string()), "{t:?}");
        assert!(terms("幫我畫一個").is_empty());
    }

    #[test]
    fn a_section_split_in_parts_keeps_its_evidence() {
        // 二次元那節一段就放得下（比較短），逆光出現一次；光影那節被切成兩段，每段也只有一次。
        // 只看單段的話短的二次元那段贏，把同一節其他段的證據也算進來，光影那節才是該挑的
        let short = format!("{}逆光{}", "景".repeat(300), "景".repeat(300));
        let para = format!("{}逆光{}", "景".repeat(400), "景".repeat(400));
        let skill = format!("---\nname: split\n---\n\n## 二次元\n\n{short}\n\n## 光影\n\n{para}\n\n{para}\n");
        let d = tmp("split").join("split");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("SKILL.md"), skill).unwrap();
        let sk = load(&d).unwrap();
        assert_eq!(sk.sections.len(), 2);
        assert!(sk.parts.len() >= 3, "{}", sk.parts.len());
        let first = sk.ranked("逆光")[0].1;
        assert_eq!(sk.parts[first].title, "光影");
        let _ = std::fs::remove_dir_all(d.parent().unwrap());
    }
}
