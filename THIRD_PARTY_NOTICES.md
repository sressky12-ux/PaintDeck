# 第三方授權說明（Third-party notices）

繪匣 PaintDeck本身以 GPL-3.0 授權（見 [LICENSE](LICENSE)）。下面這些是專案裡附帶、或安裝包裡附帶的第三方程式碼、字型和資料，各自保留原本的授權。

## 原始碼裡附帶的

| 位置 | 內容 | 授權 |
|---|---|---|
| `resources/extras/comfy_moss/` | [ComfyUI](https://github.com/comfyanonymous/ComfyUI) 官方 commit `e087d8f`（MOSS-Image 支援）的 9 個檔，給 ComfyUI 0.38.0 補用（`lora.py` 拿掉了 0.38 沒有的一段，見資料夾裡的 `說明.txt`）。安裝精靈只會把它們放進使用者的 ComfyUI 資料夾 | GPL-3.0（Comfy Org） |
| `resources/extras/h3_infinite.tar.gz` | [Herrgotts H3 Infinite Continuation Suite](https://github.com/HerrgottMargott/Herrgotts-H3-Infinite-Continuation-Suite) commit `4b1edd6`（v1.4）的執行檔案（完整 Python 原始碼＋LICENSE，見同資料夾的 `h3_infinite.txt`）。H3 長片第一次開拍時用 ssh 裝到雲端 ComfyUI | GPL-3.0（HerrgottMargott） |
| `resources/extras/fablecut/` | [FableCut](https://www.npmjs.com/package/fablecut) 瀏覽器剪輯台，加上中文化與配色（`anima-i18n.js`、`anima-style.css`） | MIT（Copyright (c) 2026 ronak-create），全文見該資料夾的 `LICENSE` |
| `resources/extras/fablecut/library/fonts/` | FableCut 附的 Google Fonts 字型 | SIL Open Font License 1.1，清單見該資料夾的 `LICENSES.md` |
| `ui/vendor/three/` | [three.js](https://threejs.org/)（`three.module.min.js`、`MarchingCubes.js`、`RoomEnvironment.js`） | MIT（Copyright 2010-2024 Three.js Authors），全文見 `ui/vendor/three/LICENSE` |
| `ui/vendor/opencc-t2cn.js` | [opencc-js](https://github.com/nk2028/opencc-js) 的繁轉簡（台灣詞→大陸詞）字典與轉換程式 | MIT，全文見 `ui/vendor/LICENSE-opencc-js.txt`；字典資料來自 [OpenCC](https://github.com/BYVoid/OpenCC)（Apache-2.0） |
| `resources/tags/t2s_chars.json` | 繁→簡逐字對照（`tools/make_t2s_chars.mjs` 用上面的 opencc-js 產生，給簡體輸入找 tag、姿勢用） | 字典資料來自 [OpenCC](https://github.com/BYVoid/OpenCC)（Apache-2.0） |
| `resources/tags/weilin/` | [WeiLin-Comfyui-Tools-Prompt](https://github.com/weilin9999/WeiLin-Comfyui-Tools-Prompt) 的分類詞庫（`library.json`，tags 2025-03-31＋NSFW 2025-04-01）和 danbooru 中文翻譯（`zh_extra.json`，2025-04-01，只收 `zh_map.json` 沒有的 tag）；內容照搬，用 OpenCC 轉台灣繁體（`tools/weilin_tags.mjs` 產生） | MIT（Copyright (c) 2025 WeiLin），全文見 `resources/tags/weilin/LICENSE.txt` |
| `resources/tags/danbooru_groups/` | Danbooru wiki [tag groups](https://danbooru.donmai.us/wiki_pages/tag_groups) 的分類（大類、小類、段落與 tag 清單，2026-11-17 抓），使用次數取自上面的 `danbooru.csv`；中文翻譯來自 `zh_map.json`、WeiLin 的 `zh_extra.json` 與 `tools/danbooru_groups_zh.json`（本機模型直翻＋人工校）。`tools/danbooru_taggroups.mjs` 產生 | 資料來自 Danbooru |
| `ui/vendor/opencc-cn2t.js` | [opencc-js](https://github.com/nk2028/opencc-js) 的簡轉繁（大陸詞→台灣詞）字典與轉換程式，給隨機產生器把外掛的簡體文字顯示成台灣繁體 | MIT，全文見 `ui/vendor/LICENSE-opencc-js.txt`；字典資料來自 [OpenCC](https://github.com/BYVoid/OpenCC)（Apache-2.0） |
| （不隨附）詞庫頁的「隨機產生器」 | [Comfyui-Anima-Wardrobe-V6](https://github.com/lingdongbudong/Comfyui-Anima-Wardrobe-V6) commit `bb45472`（v6.26.0），作者灵冻不冻，底層腳本作者飞蓬。繪匣**不附它的檔案**：使用者按「安裝隨機產生器」時才從 GitHub clone 到自己的 ComfyUI `custom_nodes`，繪匣只把 `/api/wardrobe/x/*` 轉給它的 `/anima-wardrobe-v6/*` | GPL-3.0 |
| `ui/fonts/` | Barlow Condensed、IBM Plex Mono、VT323（Google Fonts 的 latin 子集） | SIL Open Font License 1.1 |
| `resources/tags/danbooru.csv` | Danbooru tag 清單（tag、分類、數量、別名），取自 [a1111-sd-webui-tagcomplete](https://github.com/DominikDoom/a1111-sd-webui-tagcomplete) | 該專案以 MIT 授權；tag 本身來自 Danbooru |
| `resources/tag_rules.json` | 從 Danbooru 公開 API 整理的 tag 隱含關係（`tools/tag_clean_proto/build_tag_rules.py` 產生） | 資料來自 Danbooru |

## 安裝包裡附帶的（不在版控裡，用 `tools/fetch_vendor.ps1` 下載）

| 檔案 | 內容 | 授權 |
|---|---|---|
| `onnxruntime.dll` | [ONNX Runtime](https://onnxruntime.ai) 1.30.0 CPU 版（取自 PyPI 的 `onnxruntime` wheel） | MIT（Copyright (c) Microsoft Corporation） |
| `7zr.exe` | [7-Zip](https://www.7-zip.org) 26.04 主控台版 | 7zr.exe 標示 Public domain；7-Zip 其他部分為 GNU LGPL，見 https://www.7-zip.org/license.txt |

## 執行時才下載的（不包含在專案或安裝包裡）

安裝精靈依使用者的選擇從原始出處下載下面這些，各依其授權：

- ComfyUI 官方 Windows 免安裝版（GPL-3.0）
- llama.cpp（MIT）
- Node.js（MIT）
- ffmpeg（gyan.dev 的 essentials build，GPL）
- WD EVA02 Large Tagger v3（SmilingWolf，Hugging Face）
- 各個生圖、影片、語言模型與 LoRA：清單在 `resources/share/models_*.json`，授權以各模型在 Hugging Face／Civitai 頁面上寫的為準
