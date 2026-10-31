# 繪匣 PaintDeck

> 舊名「Anima 生圖站」，2026-10-24 改名開源。

在自己的 Windows 電腦上畫圖的桌面程式：用 [ComfyUI](https://github.com/comfyanonymous/ComfyUI) 當引擎，把動漫（Anima）、寫實（Qwen-Image 2.1）、排版寫字（MOSS-Image）、影片（內建 LTX-2.3，也能換成自己的 ComfyUI 影片工作流）這幾套模型包成一個中文介面。圖都在本機畫、存在本機，不經過別人的伺服器。

> **English:** A Windows desktop front-end for local image/video generation on top of ComfyUI (Rust + Tauri 2, plain-JS web UI). The UI is Traditional Chinese with English and Simplified Chinese switches. Requires an NVIDIA GPU (AMD and Intel Arc GPUs: experimental, untested on real hardware; machines without a usable GPU can still install it and run ComfyUI on the CPU). Licensed under GPL-3.0.

## 功能

| 功能 | 說明 |
|---|---|
| 生圖 | 文生圖、圖生圖、局部重繪、擴圖、多圖參考、放大、四視圖、去背；LLLite／ControlNet 構圖控制、高解析修復、版面框（指定東西放哪裡） |
| AI 改寫 | 一句中文想法改寫成完整提示詞（本機 llama.cpp 跑 Qwen 系列模型），也能看圖寫描述 |
| 反推 | 從圖片反推 Danbooru tag（WD EVA02，CPU 執行），附自動清理和中文對照 |
| 生圖助手 | 用中文說要什麼，助手自己寫提示詞、畫圖、看圖檢查、再修改；可接雲端顯卡（RunPod 或自己的主機） |
| 讓圖動起來 | 圖生影片：內建 LTX-2.3（有聲音）；也能在設定匯入自己的 ComfyUI 影片工作流（Wan、HunyuanVideo…），生圖站自動填提示詞、首格圖、種子、長度 |
| 導演台 | 長劇情自動分段 → 每段第一格 → 圖生影片 → 接成一部短片；角色設定卡（多視圖設定圖）讓角色長相一致 |
| 影片台與拍攝單 | 接雲端顯卡跑 MiniMax H3 影片：素材板＋分鏡卡、首尾幀、長片接續；拍攝單把劇情拆成一格一格的鏡頭 |
| 資產庫與角色設定圖工坊 | 角色、場景、道具集中管理；從一張圖或一段文字做出全身轉身圖和臉部特寫表，存成可重複使用的角色 |
| 漫畫台 | 選版型、寫每格內容和台詞，MOSS-Image 一次畫出整頁分格漫畫 |
| 影音工作台 | 內建剪輯台（[FableCut](resources/extras/fablecut/LICENSE)）和拆幀工具（幀數沒有固定上限，只看硬碟空間） |
| 給 AI 操作 | 內建 MCP 伺服器：Claude、Codex 等 AI 可以直接叫它生圖、生影片、剪片 |
| 圖庫與提示詞庫 | 每張圖都記得怎麼畫的；好用的設定、片段存成提示詞庫 |
| LoRA 與 Civitai | 搜尋、下載 LoRA，觸發詞和建議強度自動帶入 |
| 安裝精靈 | 第一次開啟時自動下載 ComfyUI、模型、llama.cpp、ffmpeg；已經有 ComfyUI 也能直接沿用；能掃描整台電腦，別的 ComfyUI、A1111／Forge 裡已經有的模型原地使用、不重新下載 |
| 模型與工作流 | 設定頁管理外部模型資料夾（寫成 ComfyUI 的 extra model paths，不搬檔）；模型檔拖進來看權重自動歸類，或貼 Hugging Face／Civitai 網址下載；匯入自己的 ComfyUI 文生圖工作流，在生成頁當成一個模型選 |
| 設定 | 匯出／匯入、恢復原廠（搬到「待刪除」，可以救回） |

完整說明在程式內的使用手冊（`ui/manual.html`，程式裡按「說明」打開）。

## 需要什麼

| 項目 | 需要 |
|---|---|
| 系統 | Windows 10／11（64 位元）。只支援 Windows：用到 DPAPI、Windows 程序管理，安裝精靈下載的是 Windows 版 ComfyUI 和 llama.cpp |
| 顯示卡 | NVIDIA，顯存 8GB 以上（開發時用 RTX 3070 8GB 實測） |
| A 卡（AMD） | **實驗性支援**：RX 5000 以後的獨顯、680M／780M／860M 以後的內顯，最新 AMD 驅動。精靈會改下載官方 AMD 版 ComfyUI 和 llama.cpp Vulkan 版，官方版沒附的架構再換成 AMD 的 ROCm 10 PyTorch；也認得 ZLUDA 版、DirectML 版 ComfyUI。還沒有 A 卡實測過 |
| Intel 顯示卡 | **實驗性支援**：Arc 獨顯、Core Ultra 的 Arc 內顯（改下載官方 Intel 版 ComfyUI）；UHD、Iris Xe 不支援。還沒實測過 |
| 沒有能用的顯示卡 | 照樣能裝，ComfyUI 用 CPU 跑（很慢）；生圖助手可以接雲端顯卡 |
| 記憶體 | 16GB 起跳，做影片建議 32GB |
| 硬碟 | 基本版約 20GB；完整版約 90GB |

## 安裝（一般使用者）

到 [Releases](https://github.com/sressky12-ux/PaintDeck/releases) 下載安裝檔 `PaintDeck_<版本>_x64-setup.exe` 雙擊安裝。安裝檔沒有付費簽章，Windows 可能跳出「Windows 已保護您的電腦」：按「其他資訊」→「仍要執行」。第一次打開會進安裝精靈，照畫面選資料放哪裡、要下載哪些模型就好。

完整版有幾個模型放在 Civitai，要先在「設定」填 Civitai API key 才下載得到（手冊第 01 章有申請步驟）。

## 從 Anima 生圖站升級

1. 先從系統匣右鍵「結束」，關掉舊版。
2. 到 Windows 設定解除安裝「Anima 生圖站」，**不要勾「刪除應用程式資料」**。
3. 安裝「繪匣 PaintDeck」。按「瀏覽」選資料夾後，安裝程式會再加一層 `繪匣 PaintDeck`。
4. 第一次開啟會沿用原本的設定、資料位置與圖庫；有用 MCP 的，請到「設定 → 給 AI 操作」重新複製設定。

改名後 Windows 會把新舊版當成兩套程式，兩套共用設定與連接埠，不能同時開。若裝在同一個資料夾，兩筆解除安裝紀錄可能指向同一個 `uninstall.exe`；安裝新版後再解除安裝舊版，可能把新版一起移除。勾刪除應用程式資料會清掉程式記住的資料位置，必須重跑精靈指回原資料夾；圖庫檔案仍在原資料資料夾。解除安裝時如果跳出「要不要連模型、ComfyUI 和所有資料一起刪掉？」，升級時一定要按「只移除程式，資料留著」。

## 解除安裝

到 Windows 設定解除安裝「繪匣 PaintDeck」，按下解除安裝後會跳出確認視窗，列出每個資料夾跟大小：

- **全部刪除**：永久刪除（不進資源回收筒）ComfyUI 整個資料夾（模型、LoRA、工作流、自訂節點、AI 改寫模型）、繪匣的資料、安裝暫存和程式設定，以及刪完變空的外層資料夾（例如 `AnimaStudio`）。基本版模型大約 42GB，選這個才會真的空出空間。
  - 生成的圖片（`ComfyUI\output`）預設留著；勾「連生成的圖片一起刪」才會刪。
  - junction 只拆連結，指到的檔案不刪；自己加的外部模型資料夾不動。
  - 安裝時選的是自己原本的 ComfyUI，也會整個刪掉。
- **只移除程式，資料留著**（預設）：只移除程式，模型和資料都留在原位，之後重裝可以直接沿用。

已經用舊版移除、模型還留在硬碟上的話：模型和 ComfyUI 預設放在空間最大那顆硬碟的 `X:\AnimaStudio\`，確認裡面是 `anima_data` 和 `ComfyUI_windows_portable` 後整個刪掉就好。

現有 `26.10.24` 與更早版本都沒有 App 內更新功能；第一個支援更新的版本仍須手動安裝一次。之後公開版可在「設定 → 程式更新」檢查新版，確認後才下載安裝；私人版維持手動更新，請使用私人安裝檔。第一個支援本體更新的公開版是 26.1024.1。

## 從原始碼編譯

需要：

- [Rust](https://rustup.rs/)（stable）
- Tauri CLI：`cargo install tauri-cli --version "^2" --locked`
- WebView2（Windows 10／11 大多內建）
- Node.js 只有跑前端語法檢查時要；Python 只有跑 `tools\` 裡的維護腳本時要

```powershell
# 1. 下載不放進版控的兩個執行檔（onnxruntime.dll、7zr.exe），會比對 SHA-256
powershell -ExecutionPolicy Bypass -File tools\fetch_vendor.ps1

# 2. 測試（不需要模型或 ComfyUI）
cd src-tauri
cargo test
cargo clippy --all-targets -- -D warnings

# 3. 打包成安裝檔（輸出在 src-tauri\target\release\bundle\nsis\）
cargo tauri build
```

### 開發時只跑網頁伺服器

`anima-server` 不開視窗、只跑內建網頁伺服器，改前端時重新整理瀏覽器就看得到：

```powershell
cd src-tauri
$env:ANIMA_ROOT = "$env:TEMP\anima_dev\root"   # 資料根目錄（資料庫、設定、待刪除）
$env:ANIMA_WEB  = "..\ui"                      # 網頁檔
$env:ANIMA_RES  = "..\resources"               # 程式附帶的資料
$env:COMFY_ROOT = "D:\ComfyUI_windows_portable\ComfyUI"  # 選填，預設是資料根目錄旁邊的 ComfyUI_windows_portable\ComfyUI
$env:COMFY_URL  = "http://127.0.0.1:8188"      # 選填
$env:ANIMA_PORT = "8191"                       # 選填，預設 8191（正式版用 8190）
cargo run --bin anima-server
```

然後用瀏覽器開 `http://127.0.0.1:8191`。伺服器只聽 127.0.0.1，也只接受本機頁面的請求。

## 專案結構

| 位置 | 內容 |
|---|---|
| `src-tauri/src/main.rs` | 桌面版：視窗、系統匣、第一次啟動的安裝精靈 |
| `src-tauri/src/server/` | 內建網頁伺服器（axum）：所有 `/api/*` 路由，`mod.rs` 的 `router()` 是總表 |
| `src-tauri/src/graphs.rs` | 把網頁送來的參數組成 ComfyUI 節點圖（各模式、各模型家族） |
| `src-tauri/src/comfy.rs` | 跟 ComfyUI 溝通：送任務、收進度與預覽、斷線重連 |
| `src-tauri/src/pe.rs` | AI 改寫：管理 llama-server、組請求、解析與修補模型輸出 |
| `src-tauri/src/agent.rs`、`director.rs`、`comic.rs`、`studio.rs` | 生圖助手、導演台、漫畫台、影音工作台 |
| `src-tauri/src/setup.rs` | 安裝精靈：下載、解壓、檢查 |
| `src-tauri/src/model_dirs.rs`、`image_wf.rs` | 外部模型資料夾與整台電腦的模型掃描；匯入的 ComfyUI 生圖工作流 |
| `src-tauri/src/db.rs` | SQLite（圖庫、任務、提示詞庫、LoRA 說明） |
| `src-tauri/tests/` | 整合測試：`api_routes.rs` 真的開伺服器打 API；`golden_*.rs` 跟舊版 Python 實作的輸出逐項比對 |
| `ui/` | 網頁介面：原生 ES module，不用打包工具；`js/main.js` 是進入點，`i18n/en.json` 是英文字典 |
| `resources/` | 程式附帶的資料：姿勢彙整、Danbooru tag、提示詞範本、ComfyUI 節點與工作流程、模型下載清單 |
| `tools/` | 維護腳本：英文字典（`i18n_extract.py`）、下載附帶執行檔（`fetch_vendor.ps1`）、姿勢圖渲染（`pose3d/`） |

介面文字改了以後，跑 `python -I tools/i18n_extract.py extract` 找出還沒翻的句子，翻好存成 `tools/i18n/done_NN.json`，再跑 `build` 產生 `ui/i18n/en.json`。

## 內容與模型

- 這是一套本機工具，能畫什麼由你下載的模型決定，其中包括成人內容：完整版清單有成人向 LoRA，H3 影片台有「成人」分組的範本，只給 18 歲以上的成年人使用。請遵守你所在地的法律，以及每個模型、LoRA 各自的授權條款（下載頁面上寫的那份）。
- 生圖助手不會自己挑「調整年齡」類的 LoRA（`agent.rs` 的 `agent_may_pick`）。
- 模型不包含在這個專案裡，安裝精靈從 Hugging Face、Civitai 等原始出處下載；下載清單在 `resources/share/models_*.json`。

## 授權

GPL-3.0，見 [LICENSE](LICENSE)。專案裡附帶的第三方程式碼、字型和資料各有授權，列在 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
