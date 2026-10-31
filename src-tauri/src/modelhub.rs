//! 下載中心（2026-10-29，設計見 design\下載中心_設計.md）：C 站、魔塔（ModelScope）、RunningHub 三個來源的搜尋結果整理成同一種格式。
//! 這裡只有純邏輯（組請求、整理回應、家族對照），網路在 server\api_hub.rs。
//!
//! 魔塔、RunningHub 的搜尋是網站自己用的 API（沒有文件，2026-10-29 實測）：欄位隨時可能改，所以每個來源各寫各的，
//! 壞了只影響那一個來源；回應樣本寫在單元測試裡，改壞了測試會先叫。
//!
//! 統一的一筆（item）：
//! {source, id, kind, name, author, family, family_label, usable, base, cover, cover_nsfw, downloads, likes, size, nsfw, tags,
//!  triggers, url, description, versions: [{id, name, base, family, usable, published, triggers, files: [{name, size, sha256, url, primary}]}]}
//! 魔塔的 versions 是空的：檔案清單要另外查（/api/hub/files），C 站的檔案搜尋時就有。
use crate::families;
use serde_json::{json, Value};

/// 每個來源有哪些種類（lora、model＝主模型、vae、workflow）
pub fn kinds_of(source: &str) -> &'static [&'static str] {
    match source {
        "civitai" => &["lora", "model", "vae", "workflow"],
        "modelscope" => &["lora", "model", "vae"],
        "runninghub" => &["workflow"],
        _ => &[],
    }
}

/// 站內的家族，和它在各來源的叫法。usable＝生圖站內建流程畫得了（其他家族只能靠匯入的工作流）
pub struct Family {
    pub id: &'static str,
    pub label: &'static str,
    /// C 站的 baseModel（第一個也拿來當 LoRA 資訊裡的 base_model，families::from_base_model 認得）
    pub civitai: &'static [&'static str],
    /// 魔塔的子底模（sub_vision_foundation，網頁版的底模篩選用的就是它，可以一次給好幾個）；None＝魔塔沒有這個底模，改用關鍵字找
    pub modelscope: Option<&'static [&'static str]>,
    pub usable: bool,
}

/// 2026-10-29 照 C 站 /api/v1/enums 查的名稱（MOSS Image Base 是舊名稱，現在 C 站只剩 MOSS Image，留著搜得到舊的）。
/// 魔塔 2026-10-30 改照網頁版的底模清單（GET /api/v1/muse/predict/unauth/defaultTemplateV2 → IMAGE／VIDEO 的 supportSDVersionList 的 sub_value）：
/// 以前用大底模（vision_foundation）篩，Qwen-Image-2.1 的正式寫法是「大底模 QWEN_IMAGE_20_B＋子底模 QWEN_IMAGE_2_1」，漏掉四成（1968／3309）
pub const FAMILIES: [Family; 9] = [
    Family { id: "anima", label: "Anima（動漫）", civitai: &["Anima"], modelscope: None, usable: true },
    Family { id: "qwen21", label: "Qwen 2.1（編輯）", civitai: &["Qwen 2.1"], modelscope: Some(&["QWEN_IMAGE_2_1", "QWEN_IMAGE_2_1_TURBO"]), usable: true },
    Family { id: "moss", label: "MOSS-Image（文生圖繪圖）", civitai: &["MOSS Image", "MOSS Image Base"], modelscope: Some(&["MOSS_IMAGE_SFT"]), usable: true },
    Family { id: "sdxl", label: "SDXL（含 Pony、Illustrious、NoobAI）", civitai: &["SDXL 1.0", "Pony", "Illustrious", "NoobAI"], modelscope: Some(&["SD_XL"]), usable: false },
    Family { id: "sd15", label: "SD 1.5", civitai: &["SD 1.5"], modelscope: Some(&["SD_1_5"]), usable: false },
    Family { id: "flux", label: "Flux", civitai: &["Flux.1 D", "Flux.1 S", "Flux.1 Krea", "Flux.2 D"], modelscope: Some(&["FLUX1_DEV", "FLUX1_KONTEXT", "FLUX2_DEV", "FLUX2_KLEIN_4B", "FLUX2_KLEIN_9B", "FLUX2_KLEIN_BASE_4B", "FLUX2_KLEIN_BASE_9B"]), usable: false },
    Family { id: "krea2", label: "Krea 2", civitai: &["Krea 2", "Krea 2 Turbo"], modelscope: Some(&["KREA_2_TURBO"]), usable: false },
    Family { id: "ltx", label: "LTX-2.3（影片）", civitai: &["LTXV 2.3", "LTXV2", "LTXV"], modelscope: Some(&["LTX_VIDEO_2_3"]), usable: false },
    Family { id: "h3", label: "MiniMax H3（影片）", civitai: &["MiniMax H3"], modelscope: Some(&["MINIMAX_H3"]), usable: false },
];

pub fn family(id: &str) -> Option<&'static Family> {
    FAMILIES.iter().find(|f| f.id == id)
}

/// C 站的 baseModel → 家族代號；表上沒有的再問 families::from_base_model（Anima、MOSS、Qwen 2.1 的變體寫法）
pub fn family_of_civitai(base: &str) -> Option<&'static str> {
    let b = base.trim();
    FAMILIES
        .iter()
        .find(|f| f.civitai.iter().any(|c| c.eq_ignore_ascii_case(b)))
        .map(|f| f.id)
        .or_else(|| families::from_base_model(b))
}

/// 魔塔的大底模（vision_foundation）→ 家族：子底模沒填（舊的模型）或不認得才用。
/// QWEN_IMAGE_20_B 底下有 Qwen-Image、2512、Edit、2.1 好幾代，光看大底模分不出來，不算
const MS_VF_FAMILY: [(&str, &str); 9] = [
    ("QWEN_IMAGE_2_1", "qwen21"),
    ("MOSS_IMAGE_SFT", "moss"),
    ("SD_XL", "sdxl"),
    ("SD_1_5", "sd15"),
    ("FLUX_1", "flux"),
    ("FLUX_2", "flux"),
    ("KREA_2", "krea2"),
    ("LTX_VIDEO", "ltx"),
    ("MINIMAX_H3", "h3"),
];

/// 魔塔的一筆 → 家族：先看子底模（網頁版篩的就是它），子底模沒填或不認得再看大底模
pub fn family_of_modelscope(sub: &str, vf: &str) -> Option<&'static str> {
    let (sub, vf) = (sub.trim(), vf.trim());
    FAMILIES
        .iter()
        .find(|f| f.modelscope.is_some_and(|s| s.contains(&sub)))
        .map(|f| f.id)
        .or_else(|| MS_VF_FAMILY.iter().find(|(k, _)| *k == vf).map(|(_, f)| *f))
}

fn usable(fam: Option<&str>) -> bool {
    fam.and_then(family).is_some_and(|f| f.usable)
}

fn label(fam: Option<&str>) -> Value {
    fam.and_then(family).map(|f| json!(f.label)).unwrap_or(Value::Null)
}

/// LoRA 存在 loras 底下哪個子資料夾：跟 C 站下載同一套（civitai::base_subdir），用家族在 C 站的第一個叫法
pub fn lora_subdir(fam: Option<&str>, base_raw: &str) -> String {
    match fam.and_then(family) {
        Some(f) => crate::civitai::base_subdir(f.civitai[0]),
        None => crate::civitai::base_subdir(if base_raw.is_empty() { "unknown" } else { base_raw }),
    }
}

/// LoRA 資訊（lora_meta）的 base_model：寫 C 站的叫法，families::lora_family 才認得出家族
pub fn meta_base(fam: Option<&str>) -> Option<&'static str> {
    fam.and_then(family).map(|f| f.civitai[0])
}

// ---------------------------------------------------------------- C 站
/// 種類 → C 站的 types（可以帶好幾個）
pub fn civitai_types(kind: &str) -> &'static [&'static str] {
    match kind {
        "lora" => &["LORA", "LoCon", "DoRA"],
        "model" => &["Checkpoint"],
        "vae" => &["VAE"],
        "workflow" => &["Workflows"],
        _ => &[],
    }
}

/// C 站 /api/v1/models 的查詢參數。family 空的＝不限底模；sort：downloads｜rating｜newest
pub fn civitai_query(kind: &str, fam: &str, q: &str, sort: &str, nsfw: bool, cursor: &str) -> Vec<(&'static str, String)> {
    let sort = match sort {
        "rating" => "Highest Rated",
        "newest" => "Newest",
        _ => "Most Downloaded",
    };
    let mut out: Vec<(&str, String)> = vec![("limit", "24".into()), ("sort", sort.into()), ("nsfw", nsfw.to_string())];
    for t in civitai_types(kind) {
        out.push(("types", (*t).into()));
    }
    // 工作流不分底模（C 站的工作流版本也有 baseModel，但常常亂填）
    if kind != "workflow" {
        for b in family(fam).map(|f| f.civitai).unwrap_or_default() {
            out.push(("baseModels", (*b).into()));
        }
    }
    if !q.trim().is_empty() {
        out.push(("query", q.trim().into()));
    }
    if !cursor.is_empty() {
        out.push(("cursor", cursor.into()));
    }
    out
}

fn civitai_file(f: &Value) -> Value {
    let size = f["sizeKB"].as_f64().map(|kb| (kb * 1024.0) as u64);
    json!({
        "name": f["name"], "size": size, "sha256": f["hashes"]["SHA256"].as_str().map(str::to_lowercase),
        "url": f["downloadUrl"], "primary": crate::py::truthy(f.get("primary")), "type": f["type"],
    })
}

/// C 站搜尋結果的一個模型 → 統一格式。fam 有給就只留那個家族的版本；一個版本都不剩就是 None
pub fn civitai_item(m: &Value, kind: &str, fam: &str) -> Option<Value> {
    let want = family(fam).map(|f| f.civitai);
    let versions: Vec<Value> = m["modelVersions"]
        .as_array()?
        .iter()
        .filter(|v| {
            let b = v["baseModel"].as_str().unwrap_or("");
            kind == "workflow" || want.is_none_or(|w| w.iter().any(|x| x.eq_ignore_ascii_case(b)))
        })
        .map(|v| {
            let base = v["baseModel"].as_str().unwrap_or("");
            let f = family_of_civitai(base);
            json!({
                "id": v["id"], "name": v["name"], "base": base, "family": f, "usable": usable(f),
                "published": v["publishedAt"].as_str().map(|s| s.chars().take(10).collect::<String>()),
                "triggers": v.get("trainedWords").cloned().unwrap_or(json!([])),
                "files": v["files"].as_array().into_iter().flatten().map(civitai_file).collect::<Vec<_>>(),
            })
        })
        .collect();
    let first = versions.first()?.clone();
    let raw_first = m["modelVersions"].as_array()?.iter().find(|v| v["id"] == first["id"])?;
    let cover = raw_first["images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i.get("type").and_then(Value::as_str).unwrap_or("image") == "image")
        .min_by_key(|i| i["nsfwLevel"].as_i64().unwrap_or(1));
    let images: Vec<Value> = raw_first["images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i.get("type").and_then(Value::as_str).unwrap_or("image") == "image")
        .filter_map(|i| i["url"].as_str().map(|u| json!({"url": crate::civitai::img_small(u, 450), "nsfw": i["nsfwLevel"].as_i64().unwrap_or(1)})))
        .take(6)
        .collect();
    let size = first["files"].as_array().and_then(|fs| fs.iter().find(|f| f["primary"] == true).or(fs.first())).map(|f| f["size"].clone());
    let fam_id = first["family"].as_str();
    Some(json!({
        "source": "civitai", "id": m["id"].to_string(), "kind": kind, "name": m["name"], "author": m["creator"]["username"],
        "family": first["family"], "family_label": label(fam_id), "usable": first["usable"], "base": first["base"],
        "cover": cover.and_then(|i| i["url"].as_str()).map(|u| crate::civitai::img_small(u, 450)),
        "cover_nsfw": cover.map(|i| i["nsfwLevel"].clone()).unwrap_or(json!(1)), "images": images,
        "downloads": m["stats"]["downloadCount"].as_u64().unwrap_or(0), "likes": m["stats"]["thumbsUpCount"].as_u64().unwrap_or(0),
        "size": size.unwrap_or(Value::Null), "nsfw": m["nsfw"].as_bool().unwrap_or(false),
        "tags": m["tags"].as_array().map(|t| t.iter().take(8).cloned().collect::<Vec<_>>()).unwrap_or_default(),
        "triggers": first["triggers"], "url": format!("https://civitai.com/models/{}", m["id"]),
        "description": Value::Null, "versions": versions,
    }))
}

// ---------------------------------------------------------------- 魔塔
pub const MS_API: &str = "https://modelscope.cn/api/v1";

/// 魔塔的排序：popular（熱門，預設）、newest（最新）、recommend（推薦）、stars（收藏多）。
/// 2026-10-30 實測翻到底：Default、GmtModified 一筆不漏；AigcRecommend 掉 9%、StarsCount 掉 18%（同分的每頁亂排，會重複、跳過，網頁版也一樣），
/// 所以預設用 Default
fn ms_sort(sort: &str) -> &'static str {
    match sort {
        "recommend" => "AigcRecommend",
        "newest" => "GmtModified",
        "stars" | "rating" => "StarsCount",
        _ => "Default",
    }
}

/// 魔塔分類標籤的台灣用字（網頁版是簡體；2026-10-30 照 /api/v1/models/aigc/tags 抄的，不在表上的照魔塔給的名字）
const MS_TAG_ZH: [(&str, &str); 31] = [
    ("photography", "寫實攝影"),
    ("illustration-design", "插畫設計"),
    ("e-commerce-design", "電商設計"),
    ("dimension", "二次元"),
    ("3d", "3D 立體"),
    ("hand-drawn-style", "手繪風"),
    ("flat-abstraction", "扁平抽象"),
    ("other-styles", "其他風格"),
    ("architectural-design", "建築設計"),
    ("classic-painting-style", "經典繪畫風"),
    ("cg-fantasy", "CG 幻想"),
    ("logo", "Logo 圖示"),
    ("commodity", "商品"),
    ("toy-figurines", "玩具手辦"),
    ("scenery", "風景"),
    ("animal", "動物"),
    ("artware", "工藝品"),
    ("construction", "建築物"),
    ("man", "男生"),
    ("food", "食物"),
    ("woman", "女生"),
    ("automobile-traffic", "汽車交通"),
    ("sci-fi-mecha", "科幻機甲"),
    ("clothing", "服飾"),
    ("plant", "植物"),
    ("character-enhancement", "人物加強"),
    ("art-style-strong", "畫風加強"),
    ("other-functions", "其他功能"),
    ("picture-control", "畫面控制"),
    ("main-strong", "主體加強"),
    ("character-strong", "角色加強"),
];

/// 標籤分組（音樂的標籤生圖用不到，不列）
const MS_TAG_GROUPS: [(&str, &str); 3] = [("designStyle", "風格"), ("contentElement", "內容"), ("modelFunction", "功能")];

/// /api/v1/models/aigc/tags 的回應 → [{tag, label, group}]：照分組、再照魔塔的排序（IsTop 大的在前）
pub fn ms_tags(v: &Value) -> Result<Vec<Value>, String> {
    if let Some(e) = ms_fail(v) {
        return Err(e);
    }
    let list = v["Data"]["data"].as_array().ok_or("魔塔的分類標籤看不懂（沒有 Data.data）")?;
    let mut out = vec![];
    for (ty, group) in MS_TAG_GROUPS {
        let mut g: Vec<&Value> = list.iter().filter(|t| t["TagType"] == ty && t["IsDeleted"].as_i64().unwrap_or(0) == 0).collect();
        g.sort_by_key(|t| std::cmp::Reverse(t["IsTop"].as_i64().unwrap_or(0)));
        for t in g {
            let Some(tag) = t["Tag"].as_str().filter(|x| ms_tag_ok(x)) else { continue };
            let label = MS_TAG_ZH.iter().find(|(k, _)| *k == tag).map(|(_, v)| (*v).to_string()).or_else(|| t["ChineseName"].as_str().map(str::to_string)).unwrap_or_else(|| tag.to_string());
            out.push(json!({"tag": tag, "label": label, "group": group}));
        }
    }
    Ok(out)
}

/// 魔塔的分類標籤代號（網頁版上面那排「写实摄影、二次元…」，清單在 /api/v1/models/aigc/tags）：小寫英數和 -
pub fn ms_tag_ok(tag: &str) -> bool {
    !tag.is_empty() && tag.len() <= 60 && tag.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// PUT /api/v1/dolphin/models 的請求內容，跟網頁版組的一樣：底模送 sub_vision_foundation contains（大底模固定 all）；
/// Anima 魔塔沒有這個底模，改成在關鍵字前面加「anima」（魔塔的關鍵字是整個詞比對、好幾個詞要全中）。
/// tag＝分類標籤（aigc_official_tags contains），空的或不像代號就不篩；size＝一頁幾筆（魔塔最多 100）
pub fn ms_body(kind: &str, fam: &str, q: &str, page: u32, size: u32, sort: &str, tag: &str) -> Value {
    let aigc = match kind {
        "model" => "Checkpoint",
        "vae" => "VAE",
        _ => "LoRA",
    };
    let f = family(fam);
    let name = match f {
        Some(f) if f.modelscope.is_none() => format!("{} {}", f.id, q.trim()).trim().to_string(),
        _ => q.trim().to_string(),
    };
    let sc = |cat: &str, v: &str| json!({"category": cat, "DateType": "string", "predicate": "equal", "StringValue": v});
    let mut crit: Vec<Value> = vec![];
    if let Some(subs) = f.and_then(|f| f.modelscope) {
        crit.push(json!({"category": "sub_vision_foundation", "predicate": "contains", "values": subs}));
    }
    if ms_tag_ok(tag) {
        crit.push(json!({"category": "aigc_official_tags", "predicate": "contains", "values": [tag]}));
    }
    json!({
        "PageSize": size.clamp(1, 100), "PageNumber": page.max(1), "SortBy": ms_sort(sort), "Target": "", "IsAigc": true, "ImgUrl": "",
        "Name": name, "SingleCriterion": [sc("aigc_type", aigc), sc("vision_foundation", "all")], "Criterion": crit, "IsStar": false,
    })
}

/// 魔塔圖片加縮圖參數（跟網頁版一樣寬 500；動圖不轉 webp），只動 resources.modelscope.cn 的
fn ms_thumb(u: &str) -> String {
    if !u.starts_with("https://resources.modelscope.cn/") || u.contains('?') {
        return u.to_string();
    }
    let fmt = if u.to_lowercase().ends_with(".gif") { "" } else { "/format,webp" };
    format!("{u}?x-oss-process=image/resize,w_500{fmt}/quality,Q_60")
}

/// 觸發詞：最外層 TriggerWords（陣列），沒有就看 MuseInfo 每個版本的 triggerWords（是一段 JSON 字串，例如 "[\"xxx\"]"）
fn ms_triggers(m: &Value) -> Vec<String> {
    let clean = |a: &[Value]| -> Vec<String> { a.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect() };
    let top = m["TriggerWords"].as_array().map(|a| clean(a)).unwrap_or_default();
    if !top.is_empty() {
        return top;
    }
    let mut out: Vec<String> = vec![];
    for v in m["MuseInfo"]["versions"].as_array().into_iter().flatten() {
        let tw = &v["modelVersion"]["triggerWords"];
        let list = match tw {
            Value::String(s) => serde_json::from_str::<Vec<Value>>(s).map(|a| clean(&a)).unwrap_or_else(|_| s.split(',').map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect()),
            Value::Array(a) => clean(a),
            _ => vec![],
        };
        for t in list {
            if !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}

/// 魔塔回的不是成功：拿它的 Message（中文）
fn ms_fail(v: &Value) -> Option<String> {
    (v["Success"] == false || v["Code"].as_i64().is_some_and(|c| c != 200 && c != 0))
        .then(|| format!("魔塔回應錯誤：{}", v["Message"].as_str().unwrap_or("（沒有說明）")))
}

/// 搜尋回應 → (items, 總筆數)
pub fn ms_items(v: &Value, kind: &str, fam: &str) -> Result<(Vec<Value>, u64), String> {
    if let Some(e) = ms_fail(v) {
        return Err(e);
    }
    let d = &v["Data"]["Model"];
    let list = d["Models"].as_array().ok_or("魔塔回應看不懂（沒有 Data.Model.Models），可能是網站改版：先到魔塔網頁版找")?;
    // 沒有底模標籤、靠關鍵字找的家族（Anima）：關鍵字也會撈到別的底模（例如 Krea 2 的「anima 風」LoRA），只留真的是這個家族的
    let by_keyword = family(fam).is_some_and(|f| f.modelscope.is_none());
    let items = list.iter().map(|m| ms_item(m, kind, by_keyword)).filter(|it| !by_keyword || it["family"] == fam).collect();
    Ok((items, d["TotalCount"].as_u64().unwrap_or(0)))
}

/// 名字明寫了別的底模（krea2-anima、qwen-2511-anima、ZIB-anima、Klein_…）。短的字要整個詞才算，不然 ponytail、swan 會被當成 Pony、Wan
fn names_other_family(lname: &str) -> bool {
    ["z-image", "z_image", "zimage"].iter().any(|k| lname.contains(k))
        || lname.split(|c: char| !c.is_alphanumeric()).any(|t| {
            ["krea", "qwen", "flux", "sdxl", "illustrious", "noob", "klein"].iter().any(|k| t.contains(k))
                || ["pony", "moss", "wan", "ltx", "zib"].iter().any(|k| t.strip_prefix(k).is_some_and(|r| r.is_empty() || r.starts_with(|c: char| c.is_ascii_digit())))
        })
}

/// by_keyword＝這次是在找靠關鍵字的家族（Anima）：說明寫到 anima 的也算（魔塔的關鍵字會比對說明）
fn ms_item(m: &Value, kind: &str, by_keyword: bool) -> Value {
    let (owner, name) = (m["Path"].as_str().unwrap_or(""), m["Name"].as_str().unwrap_or(""));
    let shown = m["ChineseName"].as_str().filter(|s| !s.trim().is_empty()).unwrap_or(name);
    let vf = m["VisionFoundation"].as_str().unwrap_or("");
    let muse = &m["MuseInfo"];
    // 子底模：最外層的 SubVisionFoundation（網頁版篩的就是它），沒有再看 MuseInfo
    let sub = m["SubVisionFoundation"].as_str().filter(|s| !s.trim().is_empty()).or_else(|| muse["stableDiffusionSubVersion"].as_str()).unwrap_or("");
    // 底模的顯示名稱（網頁版卡片上那個「MOSS-Image-SFT」「Krea-2-Turbo」），沒有就用代號
    let base = [&muse["stableDiffusionSubVersionDesc"], &muse["stableDiffusionVersionDesc"]].iter().find_map(|v| v.as_str().filter(|s| !s.trim().is_empty())).unwrap_or(vf);
    // Anima：魔塔沒有這個底模可選，作者只好亂標（2026-10-30 實際看到：SD 1.5、SDXL、Z-Image、FLUX.2 都有；bskff 的說明直接寫「这选不了Anima基础模型」），
    // 所以標籤不能信。名字寫 anima（找 Anima 時說明寫到也算）就當 Anima（猜的，family_guess 標出來），名字明寫別的底模的才不算
    let lname = format!("{} {}", shown.to_lowercase(), name.to_lowercase());
    let hit = lname.contains("anima") || (by_keyword && m["Description"].as_str().is_some_and(|d| d.to_lowercase().contains("anima")));
    let anima_guess = hit && !names_other_family(&lname);
    let f = if anima_guess { Some("anima") } else { family_of_modelscope(sub, vf) };
    // 封面：網頁版用的是 MuseInfo 每個版本的 coverImages（最外層的 CoverImages 幾乎都是空的）
    let images: Vec<Value> = muse["versions"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|v| v["coverImages"].as_array().into_iter().flatten())
        .chain(m["CoverImages"].as_array().into_iter().flatten())
        .filter_map(|c| c.as_str().or_else(|| c["url"].as_str()).or_else(|| c["Url"].as_str()))
        .map(|u| json!({"url": ms_thumb(u), "nsfw": 1}))
        .take(6)
        .collect();
    let likes = muse["model"]["favoriteCount"].as_u64().or(m["Stars"].as_u64()).unwrap_or(0);
    json!({
        "source": "modelscope", "id": format!("{owner}/{name}"), "kind": kind, "name": shown, "author": owner,
        "family": f, "family_label": label(f), "usable": usable(f), "family_guess": anima_guess && !vf.is_empty() && vf != "all",
        "base": base, "cover": images.first().map(|i| i["url"].clone()), "images": images, "cover_nsfw": 1,
        "downloads": m["Downloads"].as_u64().unwrap_or(0), "likes": likes, "gens": muse["genImageCount"].as_u64().unwrap_or(0),
        "size": m["StorageSize"].as_u64(), "nsfw": false,
        "tags": m["Tags"].as_array().map(|t| t.iter().take(8).cloned().collect::<Vec<_>>()).unwrap_or_default(),
        "triggers": ms_triggers(m), "url": format!("https://modelscope.cn/models/{owner}/{name}"),
        "description": m["Description"].as_str().filter(|s| !s.trim().is_empty()), "license": m["License"], "versions": [],
    })
}

/// 魔塔的模型代號（作者/名稱）：只收英數、- _ .，擋掉 ..（會拼進網址路徑）
pub fn ms_id_ok(id: &str) -> bool {
    let mut parts = id.split('/');
    let ok = |s: &str| !s.is_empty() && s != "." && s != ".." && s.len() <= 120 && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if ok(a) && ok(b))
}

/// 網址路徑的一段：英數和 -_.~ 以外的都 %XX（中文檔名、空白）
fn pct(seg: &str) -> String {
    seg.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// 下載網址：https://modelscope.cn/models/作者/名稱/resolve/master/檔案路徑（公開模型免金鑰，2026-10-29 實測 200）
pub fn ms_download_url(id: &str, path: &str) -> String {
    let p: Vec<String> = path.split('/').filter(|s| !s.is_empty()).map(pct).collect();
    format!("https://modelscope.cn/models/{id}/resolve/master/{}", p.join("/"))
}

/// 檔案清單回應（GET /api/v1/models/作者/名稱/repo/files?Revision=master&Recursive=true）→ 模型檔
pub fn ms_files(v: &Value, id: &str) -> Result<Vec<Value>, String> {
    if let Some(e) = ms_fail(v) {
        return Err(e);
    }
    let files = v["Data"]["Files"].as_array().ok_or("魔塔回應看不懂（沒有 Data.Files）")?;
    let mut out: Vec<Value> = files
        .iter()
        .filter(|f| f["Type"] == "blob")
        .filter_map(|f| {
            let path = f["Path"].as_str()?;
            crate::model_dirs::is_model_file(std::path::Path::new(path)).then(|| {
                json!({"name": path.rsplit('/').next().unwrap_or(path), "path": path, "size": f["Size"].as_u64(),
                       "sha256": f["Sha256"].as_str().filter(|s| !s.is_empty()).map(str::to_lowercase),
                       "url": ms_download_url(id, path), "primary": false})
            })
        })
        .collect();
    // 最大的那個當主要檔（LoRA 庫常常附好幾個步數的存檔，最大的通常是主模型）
    if let Some(i) = (0..out.len()).max_by_key(|&i| out[i]["size"].as_u64().unwrap_or(0)) {
        out[i]["primary"] = json!(true);
    }
    Ok(out)
}

// ---------------------------------------------------------------- RunningHub
/// POST https://www.runninghub.cn/api/sku/list（網站自己用的，不用登入）
pub fn rh_body(q: &str, page: u32) -> Value {
    json!({"categoryType": "WORKFLOW", "tagIds": [], "search": q.trim(), "categoryTagIds": [], "owners": [], "isCollected": false,
           "region": "", "isWhitelist": false, "pageNum": page.max(1), "pageSize": 24})
}

pub fn rh_items(v: &Value) -> Result<(Vec<Value>, u64), String> {
    if v["code"].as_i64() != Some(0) {
        return Err(format!("RunningHub 回應錯誤：{}", v["msg"].as_str().unwrap_or("（沒有說明）")));
    }
    let page = &v["data"]["page"];
    let list = page["records"].as_array().ok_or("RunningHub 回應看不懂（沒有 data.page.records），可能是網站改版：先到 RunningHub 網頁版找")?;
    let items = list
        .iter()
        .filter_map(|r| {
            let id = r["id"].as_str().map(str::to_string).or_else(|| r["id"].as_u64().map(|n| n.to_string()))?;
            id.chars().all(|c| c.is_ascii_digit()).then(|| {
                json!({
                    "source": "runninghub", "id": id, "kind": "workflow", "name": r["name"], "author": Value::Null,
                    "family": Value::Null, "family_label": Value::Null, "usable": true, "base": r["categoryName"],
                    "cover": r["backgroundUrl"], "cover_nsfw": 1, "downloads": r["runCount"].as_str().and_then(|s| s.parse::<u64>().ok()).or(r["runCount"].as_u64()).unwrap_or(0),
                    "likes": 0, "size": Value::Null, "nsfw": false, "tags": [], "triggers": [],
                    "url": format!("https://www.runninghub.cn/post/{id}"),
                    "description": r["description"].as_str().filter(|s| !s.trim().is_empty()), "versions": [],
                })
            })
        })
        .collect();
    Ok((items, page["total"].as_u64().unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_map_both_ways() {
        assert_eq!(family_of_civitai("Qwen 2.1"), Some("qwen21"));
        assert_eq!(family_of_civitai("MOSS Image Base"), Some("moss"));
        assert_eq!(family_of_civitai("Illustrious"), Some("sdxl"));
        assert_eq!(family_of_civitai("Qwen"), None); // 第一代 Qwen-Image，架構不同
        // 魔塔：子底模優先（Qwen-Image-2.1 正式寫法是大底模 QWEN_IMAGE_20_B＋子底模 QWEN_IMAGE_2_1），沒填才看大底模
        assert_eq!(family_of_modelscope("QWEN_IMAGE_2_1", "QWEN_IMAGE_20_B"), Some("qwen21"));
        assert_eq!(family_of_modelscope("QWEN_IMAGE_2_1_TURBO", "QWEN_IMAGE_2_1_TURBO"), Some("qwen21"));
        assert_eq!(family_of_modelscope("QWEN_IMAGE_2512", "QWEN_IMAGE_20_B"), None);
        assert_eq!(family_of_modelscope("", "QWEN_IMAGE_2_1"), Some("qwen21"));
        assert_eq!(family_of_modelscope("FLUX2_KLEIN_9B", "FLUX_2"), Some("flux"));
        assert_eq!(family_of_modelscope("", "LTX_VIDEO"), Some("ltx"));
        assert_eq!(family_of_modelscope("", ""), None);
        assert_eq!(lora_subdir(Some("qwen21"), ""), "qwen21");
        assert_eq!(lora_subdir(Some("anima"), ""), "");
        assert!(lora_subdir(None, "Wan Video").starts_with("other"));
        assert_eq!(meta_base(Some("moss")), Some("MOSS Image"));
        // 下載下來的 LoRA 資訊寫的 base_model 要讓 families 認得
        for f in FAMILIES.iter().filter(|f| f.usable) {
            assert_eq!(families::from_base_model(f.civitai[0]), Some(f.id), "{}", f.id);
        }
    }

    #[test]
    fn civitai_query_and_item() {
        let q = civitai_query("lora", "moss", " cat ", "newest", true, "abc");
        assert!(q.contains(&("types", "LORA".into())) && q.contains(&("types", "DoRA".into())));
        assert!(q.contains(&("baseModels", "MOSS Image".into())) && q.contains(&("baseModels", "MOSS Image Base".into())));
        assert!(q.contains(&("sort", "Newest".into())) && q.contains(&("query", "cat".into())) && q.contains(&("cursor", "abc".into())));
        assert!(!civitai_query("workflow", "anima", "", "", false, "").iter().any(|(k, _)| *k == "baseModels"));
        let m = json!({"id": 77, "name": "Cute", "nsfw": false, "creator": {"username": "u"}, "tags": ["a"], "stats": {"downloadCount": 9, "thumbsUpCount": 3},
            "modelVersions": [
                {"id": 1, "name": "sdxl v1", "baseModel": "SDXL 1.0", "files": [], "images": []},
                {"id": 2, "name": "v2", "baseModel": "Anima", "publishedAt": "2026-10-01T00:00:00Z", "trainedWords": ["cute"],
                 "files": [{"name": "cute.safetensors", "sizeKB": 1024.0, "primary": true, "downloadUrl": "https://civitai.com/api/download/models/2", "hashes": {"SHA256": "ABCD"}}],
                 "images": [{"url": "https://image.civitai.com/x/original=true/a.jpeg", "nsfwLevel": 4}, {"url": "https://image.civitai.com/x/width=800/b.jpeg", "nsfwLevel": 1}, {"url": "v.mp4", "type": "video"}]}
            ]});
        let it = civitai_item(&m, "lora", "anima").unwrap();
        assert_eq!((it["id"].as_str(), it["family"].as_str(), it["usable"].as_bool()), (Some("77"), Some("anima"), Some(true)));
        assert_eq!(it["versions"].as_array().unwrap().len(), 1);
        assert_eq!(it["versions"][0]["files"][0]["sha256"], "abcd");
        assert_eq!(it["size"], 1048576);
        assert_eq!(it["cover"], "https://image.civitai.com/x/width=450/b.jpeg"); // 尺度最低的那張、縮小
        assert_eq!(it["images"], json!([{"url": "https://image.civitai.com/x/width=450/a.jpeg", "nsfw": 4}, {"url": "https://image.civitai.com/x/width=450/b.jpeg", "nsfw": 1}])); // 影片不算
        // 不限底模：兩個版本都在，第一個是 SDXL（生圖站用不了）
        let all = civitai_item(&m, "lora", "").unwrap();
        assert_eq!((all["versions"].as_array().unwrap().len(), all["usable"].as_bool()), (2, Some(false)));
        assert!(civitai_item(&m, "lora", "qwen21").is_none());
    }

    #[test]
    fn modelscope_body_items_files() {
        let b = ms_body("lora", "anima", "girl", 2, 48, "newest", "");
        assert_eq!((b["Name"].as_str(), b["PageNumber"].as_u64(), b["PageSize"].as_u64(), b["SortBy"].as_str()), (Some("anima girl"), Some(2), Some(48), Some("GmtModified")));
        assert_eq!((b["SingleCriterion"][1]["StringValue"].as_str(), b["Criterion"].as_array().map(Vec::len)), (Some("all"), Some(0)));
        // 跟網頁版一樣：大底模 all、子底模 contains（一次好幾個）
        let b = ms_body("model", "qwen21", "", 0, 24, "", "dimension");
        assert_eq!((b["SingleCriterion"][0]["StringValue"].as_str(), b["SingleCriterion"][1]["StringValue"].as_str()), (Some("Checkpoint"), Some("all")));
        assert_eq!(b["Criterion"], json!([
            {"category": "sub_vision_foundation", "predicate": "contains", "values": ["QWEN_IMAGE_2_1", "QWEN_IMAGE_2_1_TURBO"]},
            {"category": "aigc_official_tags", "predicate": "contains", "values": ["dimension"]}]));
        assert_eq!((b["PageNumber"].as_u64(), ms_body("lora", "", "", 1, 500, "", "")["PageSize"].as_u64()), (Some(1), Some(100)));
        // 2026-10-29 實際回應的欄位（節錄）
        let resp = json!({"Code": 200, "Success": true, "Data": {"Model": {"TotalCount": 1966, "Models": [
            {"Path": "Lybobo", "Name": "Qwen-Image-2.1_C200_V2", "ChineseName": "", "AigcType": "LoRA", "VisionFoundation": "QWEN_IMAGE_2_1",
             "TriggerWords": [""], "CoverImages": [], "Downloads": 8, "StorageSize": 2349224366u64, "License": "Apache License 2.0", "Tags": ["LoRA"], "Stars": 4, "Description": ""}]}}});
        let (items, total) = ms_items(&resp, "lora", "qwen21").unwrap();
        assert_eq!(total, 1966);
        let it = &items[0];
        assert_eq!((it["id"].as_str(), it["name"].as_str(), it["family"].as_str()), (Some("Lybobo/Qwen-Image-2.1_C200_V2"), Some("Qwen-Image-2.1_C200_V2"), Some("qwen21")));
        assert_eq!((it["triggers"].as_array().unwrap().len(), it["cover"].is_null(), it["description"].is_null()), (0, true, true));
        assert!(ms_items(&json!({"Code": 10010103009u64, "Success": false, "Message": "登录失败"}), "lora", "").unwrap_err().contains("登录失败"));
        // 子底模才是準的：大底模寫 QWEN_IMAGE_20_B、子底模 QWEN_IMAGE_2_1 的是 Qwen 2.1；子底模 2512 的不是
        let q = json!({"Code": 200, "Success": true, "Data": {"Model": {"TotalCount": 2, "Models": [
            {"Path": "a", "Name": "qh", "VisionFoundation": "QWEN_IMAGE_20_B", "SubVisionFoundation": "QWEN_IMAGE_2_1"},
            {"Path": "b", "Name": "x2512", "VisionFoundation": "QWEN_IMAGE_20_B", "SubVisionFoundation": "QWEN_IMAGE_2512"}]}}});
        let (items, _) = ms_items(&q, "lora", "").unwrap();
        assert_eq!((items[0]["family"].as_str(), items[0]["usable"].as_bool(), items[1]["family"].as_str()), (Some("qwen21"), Some(true), None));
        // Anima 靠關鍵字找：撈到的 Krea 2「anima 風」LoRA 要拿掉，沒有底模標籤的留著
        let mixed = json!({"Code": 200, "Success": true, "Data": {"Model": {"TotalCount": 2, "Models": [
            {"Path": "a", "Name": "krea2-anima", "VisionFoundation": "KREA_2"},
            {"Path": "b", "Name": "anima-style", "VisionFoundation": ""},
            {"Path": "c", "Name": "qwen-2511-anima", "VisionFoundation": ""}]}}});
        let (items, _) = ms_items(&mixed, "lora", "anima").unwrap();
        assert_eq!((items.len(), items[0]["id"].as_str()), (1, Some("b/anima-style")));
        assert_eq!(ms_items(&mixed, "lora", "").unwrap().0.len(), 3);
        let files = json!({"Code": 200, "Success": true, "Data": {"Files": [
            {"Path": "images", "Type": "tree", "Size": 0, "Sha256": ""},
            {"Path": "README.md", "Type": "blob", "Size": 10, "Sha256": "aa"},
            {"Path": "ckpt/step500.safetensors", "Type": "blob", "Size": 100, "Sha256": "BB"},
            {"Path": "我的 lora.safetensors", "Type": "blob", "Size": 300, "Sha256": "cc"}]}});
        let fs = ms_files(&files, "a/b").unwrap();
        assert_eq!(fs.len(), 2);
        assert_eq!((fs[0]["name"].as_str(), fs[0]["sha256"].as_str(), fs[0]["primary"].as_bool()), (Some("step500.safetensors"), Some("bb"), Some(false)));
        assert_eq!(fs[1]["primary"], true);
        assert_eq!(fs[1]["url"], "https://modelscope.cn/models/a/b/resolve/master/%E6%88%91%E7%9A%84%20lora.safetensors");
        assert!(ms_id_ok("Q1131Q/anima-aesthetic-v1.1") && !ms_id_ok("a/../b") && !ms_id_ok("a/b/c") && !ms_id_ok("../x"));
    }

    /// 2026-10-30 魔塔網頁版實際的回應：封面、觸發詞、生圖次數都在 MuseInfo（最外層的 CoverImages、TriggerWords 常常是空的）
    #[test]
    fn modelscope_muse_info() {
        let m = json!({"Path": "bozoyan", "Name": "MOSS-Image-SFT-8-steps-dmd", "ChineseName": "8步MOSS", "VisionFoundation": "MOSS_IMAGE_SFT",
            "TriggerWords": null, "CoverImages": [], "Downloads": 5, "Stars": 0,
            "MuseInfo": {"genImageCount": 20, "model": {"favoriteCount": 3}, "stableDiffusionSubVersion": "MOSS_IMAGE_SFT", "stableDiffusionSubVersionDesc": "MOSS-Image-SFT",
                "versions": [
                    {"coverImages": [{"url": "https://resources.modelscope.cn/cover-images/a.png"}, {"url": "https://resources.modelscope.cn/cover-images/b.gif"}],
                     "modelVersion": {"triggerWords": "[\"moss style\", \"\"]", "versionName": "v2"}},
                    {"coverImages": [{"url": "https://resources.modelscope.cn/cover-images/c.png"}], "modelVersion": {"triggerWords": "[]"}}]}});
        let (items, _) = ms_items(&json!({"Code": 200, "Success": true, "Data": {"Model": {"TotalCount": 1, "Models": [m]}}}), "lora", "moss").unwrap();
        let it = &items[0];
        assert_eq!(it["cover"], "https://resources.modelscope.cn/cover-images/a.png?x-oss-process=image/resize,w_500/format,webp/quality,Q_60");
        assert_eq!(it["images"][1]["url"], "https://resources.modelscope.cn/cover-images/b.gif?x-oss-process=image/resize,w_500/quality,Q_60"); // 動圖不轉 webp
        assert_eq!(it["images"].as_array().unwrap().len(), 3);
        assert_eq!((it["gens"].as_u64(), it["likes"].as_u64()), (Some(20), Some(3)));
        assert_eq!(it["triggers"], json!(["moss style"]));
        assert_eq!(it["base"], "MOSS-Image-SFT");
        // 魔塔沒有 Anima 可選：作者什麼都標（實際看到的例子），標籤不能信。名字或說明寫 anima → 當 Anima（猜的，標出來）；名字明寫別的底模的不算
        let mislabeled = json!({"Code": 200, "Success": true, "Data": {"Model": {"TotalCount": 9, "Models": [
            {"Path": "a", "Name": "Anima_base_v1_character", "VisionFoundation": "SD_1_5", "MuseInfo": {"stableDiffusionSubVersionDesc": "stable-diffusion-v1-5"}},
            {"Path": "b", "Name": "anima-aesthetic-v1.1", "VisionFoundation": "FLUX_2", "SubVisionFoundation": "FLUX2_DEV"},
            {"Path": "c", "Name": "bskff", "VisionFoundation": "SD_XL", "Description": "这选不了Anima基础模型,这是Anima的lora."},
            {"Path": "d", "Name": "anima_ponytail_swan", "VisionFoundation": "Z_IMAGE_TURBO"},
            {"Path": "x", "Name": "Z-image-Anima", "VisionFoundation": "Z_IMAGE_TURBO"},
            {"Path": "x", "Name": "ZIB-anima", "VisionFoundation": "QWEN_IMAGE_20_B"},
            {"Path": "x", "Name": "animaEdit", "ChineseName": "Klein_动漫", "VisionFoundation": "FLUX_2"},
            {"Path": "x", "Name": "wan2.2-anima", "VisionFoundation": "SD_XL"},
            {"Path": "x", "Name": "plain", "VisionFoundation": "SD_XL"}]}}});
        let (items, _) = ms_items(&mislabeled, "lora", "anima").unwrap();
        assert_eq!(items.iter().map(|i| i["id"].as_str().unwrap()).collect::<Vec<_>>(), ["a/Anima_base_v1_character", "b/anima-aesthetic-v1.1", "c/bskff", "d/anima_ponytail_swan"]);
        assert_eq!((items[0]["family"].as_str(), items[0]["family_guess"].as_bool(), items[0]["base"].as_str()), (Some("anima"), Some(true), Some("stable-diffusion-v1-5")));
        // 不是在找 Anima：名字寫 anima 的照樣認，只有說明提到的不算（照作者標的）
        let (items, _) = ms_items(&mislabeled, "lora", "").unwrap();
        assert_eq!((items[1]["family"].as_str(), items[2]["family"].as_str()), (Some("anima"), Some("sdxl")));
        // 分類標籤（網頁版上面那排「二次元、写实摄影…」）
        let b = ms_body("lora", "", "", 1, 24, "", "dimension");
        assert_eq!(b["Criterion"], json!([{"category": "aigc_official_tags", "predicate": "contains", "values": ["dimension"]}]));
        assert_eq!(ms_body("lora", "", "", 1, 24, "", "bad tag!")["Criterion"], json!([]));
    }

    #[test]
    fn modelscope_tags() {
        let v = json!({"Code": 200, "Success": true, "Data": {"data": [
            {"Tag": "woman", "ChineseName": "女生", "TagType": "contentElement", "IsTop": 5, "IsDeleted": 0},
            {"Tag": "dimension", "ChineseName": "二次元", "TagType": "designStyle", "IsTop": 30, "IsDeleted": 0},
            {"Tag": "photography", "ChineseName": "写实摄影", "TagType": "designStyle", "IsTop": 33, "IsDeleted": 0},
            {"Tag": "music-style-pop", "ChineseName": "流行", "TagType": "musicStyle", "IsTop": 1, "IsDeleted": 0},
            {"Tag": "new-thing", "ChineseName": "新东西", "TagType": "modelFunction", "IsTop": 1, "IsDeleted": 0},
            {"Tag": "gone", "ChineseName": "删掉的", "TagType": "designStyle", "IsTop": 99, "IsDeleted": 1}]}});
        let t = ms_tags(&v).unwrap();
        let got: Vec<(&str, &str, &str)> = t.iter().map(|x| (x["tag"].as_str().unwrap(), x["label"].as_str().unwrap(), x["group"].as_str().unwrap())).collect();
        assert_eq!(got, [("photography", "寫實攝影", "風格"), ("dimension", "二次元", "風格"), ("woman", "女生", "內容"), ("new-thing", "新东西", "功能")]);
    }

    #[test]
    fn runninghub_items() {
        let resp = json!({"code": 0, "msg": "success", "data": {"page": {"total": 5, "records": [
            {"id": "2004520621293486081", "name": "【微电影】分镜 Qwen", "description": "单图提供参考", "runCount": "12", "categoryName": "图生图",
             "backgroundUrl": "https://rh-images.xiaoyaoyou.com/a.jpg"},
            {"id": "../x", "name": "壞的"}]}}});
        let (items, total) = rh_items(&resp).unwrap();
        assert_eq!((items.len(), total), (1, 5));
        assert_eq!((items[0]["downloads"].as_u64(), items[0]["url"].as_str()), (Some(12), Some("https://www.runninghub.cn/post/2004520621293486081")));
        assert!(rh_items(&json!({"code": 500, "msg": "busy"})).unwrap_err().contains("busy"));
        assert_eq!(rh_body("qwen", 0)["pageNum"], 1);
    }
}
