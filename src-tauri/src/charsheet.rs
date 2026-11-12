//! 角色設定圖工坊（design\角色設定圖工坊_設計.md）：
//! 一張全身定稿圖 → Qwen 2.1 多圖參考（<image1>＝定稿）展開成「全身轉身圖」「臉部特寫表」。
//! 這裡只有純函式：兩種版面的提示詞和尺寸、文字起點的定稿補字、反推 tag 只留外觀類。
//! 生圖本身走既有 /api/generate（txt2img／multiref／sheet），存檔走資產庫 API。
//! 2026-10-24 實測（D:\AI\小克工作區\測試區\charsheet\run.py，官方 Turbo 8 步）：兩個測試角色、兩個種子，結果見設計第 8 節。

/// 全身轉身圖的輸出尺寸（橫的，五個全身一排）
pub const TURN_W: u32 = 1664;
pub const TURN_H: u32 = 928;
/// 臉部特寫表的輸出尺寸（三欄兩列）
pub const FACE_W: u32 = 1344;
pub const FACE_H: u32 = 1344;

const TURN: &str = "A professional character turnaround reference sheet of {who}. Five full-body views of the same character stand side by side \
in one row on a plain light gray background, from left to right: front view, three-quarter front view, side view in profile, \
three-quarter back view, and back view. Every view shows the whole body from head to feet at exactly the same height and scale, \
feet on the same ground line, standing in a relaxed A-pose with the arms slightly away from the body. The face, hairstyle, \
body proportions, outfit, colors and accessories match <image1> exactly in every view. Even flat studio lighting, no text.";
const FACE: &str = "A character expression sheet of {who}, head and shoulders only: six portraits arranged in a grid of three columns and two rows \
on a plain light gray background. Top row, all with a calm neutral expression: front view, three-quarter view, side profile view. \
Bottom row, all front view: a happy smile, an angry frown, a shy blush. The face, eye color, hairstyle, hair color and accessories \
match <image1> exactly in every portrait, every head the same size with the same lighting, no text.";
/// 正面特寫（2026-10-27 角色卡）：單人、正臉、肩膀以上、平光素背景，鎖臉用的固定參考圖
pub const PORTRAIT_W: u32 = 1024;
pub const PORTRAIT_H: u32 = 1024;
const PORTRAIT: &str = "A front-view head-and-shoulders portrait of {who}, one person only: looking straight at the camera with a calm neutral expression, \
shoulders square to the camera, the whole head and hair inside the frame, even soft studio lighting, plain light gray background. \
The face, eye color, hairstyle, hair color, skin tone, makeup and every mark or accessory match <image1> exactly. No text.";
/// 生成頁是 Anima（動漫）時加的畫風字；其他家族照定稿的畫風，不加
const ANIME: &str = " Anime illustration, clean line art, cel shading.";

/// kind＝"turnaround"｜"face"，desc＝角色外觀（可空白），family＝生成頁的模型家族（anima 才加動漫畫風字）。
/// 回傳（提示詞, 寬, 高）；kind 不認得回 None
pub fn prompt(kind: &str, desc: &str, family: &str) -> Option<(String, u32, u32)> {
    let (tpl, w, h) = match kind {
        "turnaround" => (TURN, TURN_W, TURN_H),
        "face" => (FACE, FACE_W, FACE_H),
        "portrait" => (PORTRAIT, PORTRAIT_W, PORTRAIT_H),
        _ => return None,
    };
    let desc = desc.trim().trim_end_matches('.');
    let who = if desc.is_empty() { "the character from <image1>".to_string() } else { format!("the character from <image1> ({desc})") };
    let mut p = tpl.replace("{who}", &who);
    if family == "anima" {
        p.push_str(ANIME);
    }
    Some((p, w, h))
}

/// 文字起點畫定稿時接在使用者文字後面的補字：全身、正面、A 字站姿、素色淺灰背景
pub fn pose_suffix(family: &str) -> &'static str {
    if family == "anima" {
        ", full body, standing, front view, arms at sides, simple background, grey background"
    } else {
        ". Full body, standing straight facing the camera in a relaxed A-pose, plain light gray background."
    }
}

/// 不是外觀的 tag：人數、姿勢、視角、構圖、表情、視線（背景另外用「結尾是 background」判斷）
const DROP: &[&str] = &[
    "1girl", "1boy", "2girls", "2boys", "multiple girls", "multiple boys", "solo", "solo focus", "full body", "upper body", "lower body",
    "cowboy shot", "portrait", "standing", "sitting", "arms at sides", "arms behind back", "hand on own hip", "hands on own hips",
    "contrapposto", "looking at viewer", "looking to the side", "looking away", "closed mouth", "open mouth", "smile", "light smile",
    "expressionless", "blush", "parted lips", "front view", "from side", "from behind", "straight-on", "turnaround",
    "multiple views", "reference sheet", "character sheet", "simple background", "shadow", "gradient", "border", "white border",
];

/// 反推回來的 tag 只留外觀（頭髮、眼睛、膚色、身材、衣服、配件、角色名）；姿勢、表情、背景、構圖、人數丟掉
pub fn filter_tags(tags: &[String]) -> Vec<String> {
    tags.iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty() && !DROP.contains(t) && !t.ends_with(" background"))
        .map(str::to_owned)
        .fold(vec![], |mut v, t| {
            if !v.contains(&t) {
                v.push(t);
            }
            v
        })
}

/// 讀圖模型填角色設定表的系統提示（欄位見 charspec::FIELDS）
pub const SYSTEM_SPEC: &str = r#"You fill in a fixed spec sheet for ONE character from reference images of that character. Return JSON only, no prose:
{"fields": {"sex": F, "age": F, "hair_style": F, "hair_color": F, "face": F, "eyes": F, "skin": F, "body": F, "marks": F, "outfit": F}}
where every F is {"zh": "<short Traditional Chinese phrase>", "en": "<short English phrase>", "tags": ["<Danbooru tag>", ...]}.

Rules:
- Only objective, visible, lasting features. Never pose, expression, background, lighting or camera.
- Every phrase must stand alone in a prompt. en examples: "female", "looks in her mid 20s", "long straight hair reaching the waist", "orange hair", "oval face", "amber almond-shaped eyes", "fair skin", "slender build". zh examples: "女性", "外觀二十五歲上下", "及腰長直髮", "橘色頭髮", "鵝蛋臉", "琥珀色杏眼", "白皙膚色", "纖細體型".
- age: the apparent age.
- marks: permanent features that identify this character (mole, scar, tattoo, freckles, heterochromia, an accessory always worn). Separate several with "; " in en and "；" in zh. Empty strings when there are none.
- outfit: the clothes in the images (kept apart because it can change).
- tags: real Danbooru tags, lowercase with spaces (e.g. "orange hair", "mole under eye"), 0 to 6 per field.
- Use the notes (name, sex, existing description and tags) when they agree with the images; when they disagree, the images win."#;

/// 模型回的設定表 → 乾淨的 fields（每一格 {zh, en, tags}）；讀不出 JSON 回 None
pub fn spec_fields(reply: &str) -> Option<serde_json::Value> {
    let m = serde_json::Value::Object(crate::director::json_in(reply)?);
    let fields = if m["fields"].is_object() { m["fields"].clone() } else { m };
    Some(crate::charspec::clean(&serde_json::json!({"fields": fields}))["fields"].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_kinds() {
        let (p, w, h) = prompt("turnaround", "a girl with orange hair", "anima").unwrap();
        assert!(p.contains("<image1>") && p.contains("a girl with orange hair") && p.contains("Anime illustration"));
        assert_eq!((w, h), (TURN_W, TURN_H));
        let (p, w, h) = prompt("face", "", "qwen21").unwrap();
        assert!(p.contains("<image1>") && !p.contains("Anime illustration"));
        assert_eq!((w, h), (FACE_W, FACE_H));
        assert!(prompt("x", "", "anima").is_none());
        assert!([w, h, TURN_W, TURN_H, PORTRAIT_W, PORTRAIT_H].iter().all(|v| v.is_multiple_of(32)));
        let (p, w, h) = prompt("portrait", "an adult woman with orange hair", "anima").unwrap();
        assert!(p.contains("<image1>") && p.contains("head-and-shoulders") && p.contains("an adult woman with orange hair") && p.ends_with("cel shading."));
        assert_eq!((w, h), (PORTRAIT_W, PORTRAIT_H));
    }

    #[test]
    fn spec_reply_is_cleaned() {
        let reply = "好的\n{\"fields\": {\"hair_color\": {\"zh\": \"橘髮\", \"en\": \"orange hair\", \"tags\": [\"Orange_Hair\"]}, \"junk\": 1}}";
        let f = spec_fields(reply).unwrap();
        assert_eq!(f["hair_color"]["tags"], serde_json::json!(["orange hair"]));
        assert_eq!(f.as_object().unwrap().len(), crate::charspec::FIELDS.len());
        assert!(spec_fields("沒有 JSON").is_none());
        // 模型直接回欄位（沒包 fields）也收
        assert_eq!(spec_fields("{\"sex\": {\"en\": \"female\"}}").unwrap()["sex"]["en"], "female");
    }

    #[test]
    fn suffix_by_family() {
        assert!(pose_suffix("anima").contains("full body, standing"));
        assert!(pose_suffix("other").contains("A-pose") && !pose_suffix("other").contains("grey background"));
    }

    #[test]
    fn tags_filtered() {
        let t = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            filter_tags(&t(&["1girl", "orange hair", "full body", "blue sky background", "hair ornament", "standing", "smile"])),
            t(&["orange hair", "hair ornament"])
        );
        assert!(filter_tags(&t(&["solo", "simple background", "looking at viewer"])).is_empty());
    }
}
