# -*- coding: utf-8 -*-
"""掃出使用次數最高的 800 個有中文對照的 Danbooru tag，供人工校正台灣用語與錯翻。

用法（專案根目錄）：
    C:\\Users\\User\\AppData\\Local\\Python\\bin\\python -I -X utf8 tools\\zh_fix_scan.py

輸出 tools\\zh_fix_report.md，一行一個 tag：
    tag | 現在的中文 | 建議（空白＝不用改）

資料來源與 src-tauri\\src\\tags.rs 的 TagIndex::load 相同：
    resources\\tags\\danbooru.csv   tag,category,count,aliases
    resources\\tags\\zh_map.json    [["中文", "english tag"], ...]，同一個 tag 以第一筆為準
"""

import csv
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TAGS_DIR = ROOT / "resources" / "tags"
OUT = ROOT / "tools" / "zh_fix_report.md"
TOP_N = 800

# 與 tags.rs 的 SKIP 相同：年齡類 tag 不列入自動完成
SKIP = re.compile(
    r"(^|_)(loli|lolicon|lolidom|shota|shotacon|child|children|toddler|toddlercon|"
    r"underage|aged_down|female_child|male_child)($|_)"
)

# 對岸用語 → 台灣說法。只放整個詞，避免把「橙色」以外的字亂換。
TERM_RULES = [
    ("視頻", "影片"),
    ("屏幕", "螢幕"),
    ("鼠標", "滑鼠"),
    ("文胸", "胸罩"),
    ("打底褲", "內搭褲"),
    ("連衣裙", "洋裝"),
    ("帽衫", "帽T"),
    ("衛衣", "大學T"),
    ("劉海", "瀏海"),
    ("質量", "品質"),
    ("信息", "資訊"),
    ("軟件", "軟體"),
    ("網絡", "網路"),
    ("橙髮", "橘髮"),
    ("橙發", "橘髮"),
    ("橙眼", "橘眼"),
    ("橙色", "橘色"),
    ("橙紅", "橘紅"),
]

# tag 專用的修正（意思錯、簡轉繁轉錯、對岸說法）。鍵是小寫底線。
OVERRIDES = {
    "school_uniform": "制服",
    "closed_mouth": "閉著嘴",
    "grey_hair": "灰髮",
    "short_sleeves": "短袖",
    "sidelocks": "耳前側髮",
    "cowboy_shot": "牛仔鏡頭",
    "pantyhose": "褲襪",
    "nude": "裸體",
    "open_clothes": "衣服敞開",
    "censored": "打碼的",
    "collared_shirt": "有領襯衫",
    "japanese_clothes": "日式服裝",
    "medium_hair": "中長髮",
    "parted_lips": "微張的嘴唇",
    "puffy_sleeves": "泡泡袖",
    "fang": "虎牙",
    "stomach": "肚子",
    "speech_bubble": "對話框",
    "sex": "性愛",
    "white_dress": "白色洋裝",
    "on_back": "仰躺",
    "blue_archive": "蔚藍檔案",
    "fate_(series)": "Fate（系列）",
    "fate/grand_order": "Fate/Grand Order（FGO）",
    "orange_hair": "橘髮",
    "star_(symbol)": "星形",
    "black_dress": "黑色洋裝",
    "blurry": "模糊的",
    "from_side": "從側面看",
    "white_thighhighs": "白色大腿襪",
    "arm_up": "抬起一隻手臂",
    "clothes_lift": "掀起衣服",
    "leotard": "緊身連身衣",
    "black_pantyhose": "黑色褲襪",
    "bracelet": "手環",
    "arms_up": "舉起雙手",
    "two_side_up": "兩側綁髮",
    "covered_nipples": "遮住乳頭",
    "clothing_cutout": "服裝鏤空",
    "wrist_cuffs": "腕套",
    "bar_censor": "條狀打碼",
    "torn_clothes": "破損的衣服",
    "short_shorts": "熱褲",
    "buttons": "鈕扣",
    "ascot": "領巾",
    "gradient_hair": "漸層頭髮",
    "gradient_background": "漸層背景",
    "one-piece_swimsuit": "連身泳衣",
    "tattoo": "刺青",
    "uncensored": "無碼",
    "hoodie": "連帽T",
    "profile": "側臉",
    "floating_hair": "飄浮的頭髮",
    "double_bun": "雙丸子頭",
    "dress_shirt": "襯衫",
    "bodysuit": "緊身連身衣",
    "dutch_angle": "傾斜鏡頭",
    "bell": "鈴鐺",
    "open_shirt": "敞開的襯衫",
    "hair_intakes": "髮際線",
    "watermark": "浮水印",
    "one_side_up": "單側綁髮",
    "facial_mark": "臉部印記",
    "side-tie_bikini_bottom": "側綁帶比基尼泳褲",
    "black_socks": "黑色襪子",
    "sunglasses": "墨鏡",
    "bob_cut": "鮑伯頭",
    "no_shoes": "沒穿鞋",
    "short_dress": "短洋裝",
    "fire_emblem": "聖火降魔錄",
    "drill_hair": "鑽頭捲髮",
    "thigh_boots": "過膝長靴",
    "flying_sweatdrops": "飛出的汗珠",
    "traditional_media": "傳統媒材",
    "arms_behind_back": "雙手背後",
    "stuffed_toy": "絨毛玩偶",
    "crossed_arms": "雙手交叉抱胸",
    "cleavage_cutout": "乳溝鏤空",
    "garter_straps": "吊襪帶",
    "animated": "動圖",
    "obi": "和服腰帶",
    "portrait": "肖像",
    "shirt_lift": "掀起上衣",
    "oral": "口交",
    "final_fantasy": "Final Fantasy（太空戰士）",
    "tank_top": "背心（細肩帶）",
    "scrunchie": "大腸髮圈",
    "crossed_legs": "翹二郎腿",
    "colored_inner_hair": "內層染髮",
    "partial_commentary": "部分註解",
    "backpack": "背包",
    "mob_cap": "軟布帽",
    "blazer": "西裝外套",
    "yaoi": "BL（耽美）",
    "spiked_hair": "尖刺髮",
    "single_hair_bun": "單丸子頭",
    "black_leotard": "黑色緊身連身衣",
    "juliet_sleeves": "茱麗葉袖",
    "light_blush": "淡淡臉紅",
    "slit_pupils": "豎瞳",
    "underwear_only": "只穿內衣褲",
    "hat_ribbon": "帽子緞帶",
    "grey_shirt": "灰色襯衫",
    "sisters": "姊妹",
    "crossover": "跨界合作",
    "breasts_out": "露出乳房",
    "polka_dot": "圓點",
    "fishnets": "網襪",
    "bandaid": "OK繃",
    "pink_dress": "粉色洋裝",
    "skin_fang": "露出的虎牙",
    "tan": "曬黑",
    "hat_bow": "帽子蝴蝶結",
    "letterboxed": "寬螢幕格式",
    "otoko_no_ko": "偽娘（男の娘）",
    "hair_bobbles": "髮圈",
    "cherry_blossoms": "櫻花",
    "armband": "臂章",
    "revealing_clothes": "暴露衣著",
    "surprised": "驚訝",
    "remilia_scarlet": "蕾米莉亞·斯卡蕾特",
    "flandre_scarlet": "芙蘭朵露·斯卡蕾特",
    "meme": "迷因",
    "anal": "肛交",
    "low_ponytail": "低馬尾",
    "danganronpa_(series)": "槍彈辯駁（系列）",
    "strapless_dress": "無肩帶洋裝",
    "bike_shorts": "單車褲",
    "pelvic_curtain": "腰間布簾",
    "holding_phone": "拿著手機",
    "white_pantyhose": "白色褲襪",
    "large_pectorals": "大胸肌",
    "waist_apron": "半身圍裙",
    "gundam": "鋼彈",
}


def load_counts():
    counts = {}
    with (TAGS_DIR / "danbooru.csv").open(encoding="utf-8", newline="") as f:
        for row in csv.reader(f):
            if len(row) < 3:
                continue
            tag = row[0]
            aliases = row[3].split(",") if len(row) > 3 and row[3] else []
            if SKIP.search(tag) or any(SKIP.search(a) for a in aliases):
                continue
            try:
                counts[tag] = int(row[2] or 0)
            except ValueError:
                counts[tag] = 0
    return counts


def load_zh():
    pairs = json.loads((TAGS_DIR / "zh_map.json").read_text(encoding="utf-8"))
    zh = {}
    for pair in pairs:
        if not isinstance(pair, list) or len(pair) < 2:
            continue
        cur, en = pair[0], pair[1]
        if not isinstance(cur, str) or not isinstance(en, str):
            continue
        key = en.replace(" ", "_").lower()
        zh.setdefault(key, cur)
    return zh


def suggest(tag, zh):
    if tag in OVERRIDES:
        return OVERRIDES[tag]
    if "hair" in tag:
        zh = zh.replace("發", "髮")
    out = zh
    for src, dst in TERM_RULES:
        out = out.replace(src, dst)
    return out if out != zh else ""


def main():
    counts = load_counts()
    zh = load_zh()
    rows = [(tag, zh[tag], counts.get(tag, 0)) for tag in zh]
    rows.sort(key=lambda r: (-r[2], r[0]))
    rows = rows[:TOP_N]
    lines = [
        "# Danbooru tag 中文對照校正（前 800 名）",
        "",
        "格式：`tag | 現在的中文 | 建議（空白＝不用改）`",
        "",
    ]
    for tag, cur, _count in rows:
        lines.append(f"{tag} | {cur} | {suggest(tag, cur)}")
    OUT.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"{len(rows)} rows -> {OUT}")


if __name__ == "__main__":
    main()
