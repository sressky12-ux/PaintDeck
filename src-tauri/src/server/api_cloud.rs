//! 生圖助手的雲端模式：三種連法（data\agent\cloud\cloud.json 的 provider）
//!   runpod：一鍵租 RunPod 的卡（RTX A6000 48GB 起）→ 裝 llama.cpp＋35B（跟本機同一份無審查版）和 ComfyUI（master，含 MOSS）＋MOSS-Image，
//!           用完刪機。（2.0.19 前是 Qwen 2.1 Turbo，隨 Qwen 淘汰換掉）
//!   ssh：   自己在任何一家雲端（Vast.ai、AutoDL、Lambda、TensorDock、Paperspace…或自己的主機）租好 Linux＋NVIDIA 48GB 的機器、
//!           把生圖站的公鑰加進去 → 生圖站用 SSH 連進去跑同一份安裝腳本。開關機、計費在那家的網站（生圖站只能停服務、可選送關機指令）。
//!   url：   已經在跑的 ComfyUI（要有 MOSS 三個檔）＋OpenAI 相容的 LLM 網址（llama-server、vLLM…），直接連，不安裝。
//! runpod／ssh 開 SSH 通道（本機 127.0.0.1:18199 → 雲端 llama-server、18188 → 雲端 ComfyUI）→ 助手的「想」和「畫」都在雲端跑，
//! 圖下載回本機 output\AnimaWeb\cloud\ 進圖庫。閒置太久自動關（runpod 刪機；ssh 要使用者勾「順便關主機」）；
//! 程式重開時看到上次沒關的機器會提醒（不會自己花錢）。
//!
//! 金鑰：設定視窗填的存在 cloud.json（Windows 用 DPAPI 加密成 <名稱>_dpapi）；RunPod 沒填就讀環境變數 RUNPOD_API_KEY。
//! 金鑰只放在請求標頭，不印、不寫 log、不送網頁（網頁只看得到「有沒有填」）。
//! 只動自己開的那一台（pod.json 記的編號），帳號裡其他機器一律不碰。
//! RunPod 開機前網頁一定先顯示每小時價格、使用者按確認才送 {"confirm": true}。
use super::{bad, blocking, ok, ApiResult, Body, Shared};
use crate::director::tx;
use crate::families;
use crate::graphs;
use crate::py::{now, truthy};
use axum::extract::State;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

/// 可以選的顯卡：48GB 以上才放得下 35B 腦袋＋MOSS 生圖（35B 約 17GB、MOSS 三個檔約 13GB）。ids 依序試（第一張租不到就租同等級的下一張）。
/// 價格是開機前顯示用的（RunPod Secure Cloud 2026-10-13 牌價）；開機後一律改用 API 回的 costPerHr
pub struct GpuOpt {
    pub key: &'static str,
    pub ids: &'static [&'static str],
    pub name: &'static str,
    pub price_lo: f64,
    pub price_hi: f64,
    pub note: &'static str,
    /// 顯存（GB）：助手＋H3 同一台要 80GB 以上
    pub vram: u32,
}

pub const GPUS: &[GpuOpt] = &[
    GpuOpt {
        key: "a6000",
        ids: &["NVIDIA RTX A6000", "NVIDIA A40"],
        name: "RTX A6000 48GB",
        price_lo: 0.59,
        price_hi: 0.59,
        note: "最划算；實測一輪約 20 秒。租不到會改租同價的 A40",
        vram: 48,
    },
    GpuOpt {
        key: "l40s",
        ids: &["NVIDIA L40S", "NVIDIA RTX 6000 Ada Generation"],
        name: "L40S 48GB",
        price_lo: 0.99,
        price_hi: 1.09,
        note: "生圖估計快 1.5 倍，腦袋差不多（還沒實測）。租不到會改租 RTX 6000 Ada",
        vram: 48,
    },
    GpuOpt {
        key: "pro6000",
        ids: &["NVIDIA RTX PRO 6000 Blackwell Server Edition", "NVIDIA RTX PRO 6000 Blackwell Workstation Edition"],
        name: "RTX PRO 6000 96GB",
        price_lo: 2.49,
        price_hi: 2.59,
        note: "最快：生圖估計 2～3 倍、腦袋約 2 倍（還沒實測）",
        vram: 96,
    },
];

/// RunPod 的顯卡目錄：公開的 GraphQL，不用金鑰。拿來自動更新價格、有沒有貨，和上面三張以外「其他卡」的選項
const CATALOG: &str = "https://api.runpod.io/graphql";
const CATALOG_Q: &str = "query { gpuTypes { id displayName memoryInGb secureCloud securePrice lowestPrice(input: {gpuCount: 1, secureCloud: true}) { stockStatus } } }";
/// 目錄放多久就重抓（秒）：有沒有貨變得很快；抓失敗隔一分鐘再試
const CATALOG_TTL: f64 = 600.0;
const CATALOG_RETRY: f64 = 60.0;
/// 「其他卡」最少要的顯存：助手（35B 腦袋＋MOSS）、H3 都要 48GB 以上
const MIN_VRAM: u32 = 48;

/// 目錄裡的一張卡（只留 Secure Cloud 租得到的）
#[derive(Clone, Debug, PartialEq)]
pub struct LiveGpu {
    pub id: String,
    pub name: String,
    pub vram: u32,
    pub price: f64,
    /// High／Medium／Low；空字串＝現在租光
    pub stock: String,
}

fn parse_catalog(v: &Value) -> Vec<LiveGpu> {
    let Some(a) = v["data"]["gpuTypes"].as_array() else { return Vec::new() };
    a.iter()
        .filter(|g| g["secureCloud"].as_bool() == Some(true))
        .filter_map(|g| {
            let (id, price) = (tx(&g["id"]), g["securePrice"].as_f64().unwrap_or(0.0));
            (!id.is_empty() && price > 0.0).then(|| LiveGpu {
                name: tx(&g["displayName"]),
                vram: g["memoryInGb"].as_u64().unwrap_or(0) as u32,
                price,
                stock: tx(&g["lowestPrice"]["stockStatus"]),
                id,
            })
        })
        .collect()
}

/// 網頁上能選的一張卡。key：推薦的是 GPUS 的 key，其他卡是 RunPod 的 id
#[derive(Clone, Debug)]
pub struct GpuChoice {
    pub key: String,
    pub ids: Vec<String>,
    pub name: String,
    pub price_lo: f64,
    pub price_hi: f64,
    pub note: String,
    pub vram: u32,
    /// 目錄說的貨量（ids 裡最好的那張）：High／Medium／Low／""＝租光；None＝目錄裡沒這張（或還沒抓到目錄）
    pub stock: Option<String>,
    /// 上面 GPUS 那三張（推薦）
    pub preset: bool,
}

fn stock_rank(s: &str) -> u8 {
    match s {
        "High" => 3,
        "Medium" => 2,
        "Low" => 1,
        _ => 0,
    }
}

/// 推薦三張在前（價格、貨量照目錄更新），其他 48GB 以上的 NVIDIA 卡照價格排在後面（不含切出來的 MIG）
fn gpu_choices(live: &[LiveGpu]) -> Vec<GpuChoice> {
    let mut out: Vec<GpuChoice> = GPUS
        .iter()
        .map(|g| {
            let hits: Vec<&LiveGpu> = live.iter().filter(|l| g.ids.contains(&l.id.as_str())).collect();
            let (lo, hi) = match hits.is_empty() {
                true => (g.price_lo, g.price_hi),
                false => hits.iter().fold((f64::MAX, 0.0_f64), |(lo, hi), l| (lo.min(l.price), hi.max(l.price))),
            };
            GpuChoice {
                key: g.key.into(),
                ids: g.ids.iter().map(|s| s.to_string()).collect(),
                name: g.name.into(),
                price_lo: lo,
                price_hi: hi,
                note: g.note.into(),
                vram: g.vram,
                stock: hits.iter().map(|l| l.stock.clone()).max_by_key(|s| stock_rank(s)),
                preset: true,
            }
        })
        .collect();
    let mut more: Vec<&LiveGpu> = live
        .iter()
        .filter(|l| l.id.starts_with("NVIDIA ") && !l.id.contains(" MIG ") && l.vram >= MIN_VRAM)
        .filter(|l| !GPUS.iter().any(|g| g.ids.contains(&l.id.as_str())))
        .collect();
    more.sort_by(|a, b| a.price.total_cmp(&b.price).then(a.vram.cmp(&b.vram)));
    out.extend(more.into_iter().map(|l| GpuChoice {
        key: l.id.clone(),
        ids: vec![l.id.clone()],
        name: format!("{} {}GB", l.name, l.vram),
        price_lo: l.price,
        price_hi: l.price,
        note: "RunPod 目錄自動抓到的卡，還沒實測；裝不起來會自動關機刪除（開機那幾分鐘照算錢）".into(),
        vram: l.vram,
        stock: Some(l.stock.clone()),
        preset: false,
    }));
    out
}

/// 顯卡目錄的快取（`<雲端資料夾>\gpus.json` 存 RunPod 原始回應，重開程式也有上次的清單）
#[derive(Default)]
struct Catalog {
    at: f64,
    tried: f64,
    busy: bool,
    live: Vec<LiveGpu>,
}

/// 開機前、讀不到實際價格時用的價格（預設那張卡）
pub const PRICE_HINT: f64 = 0.59;
const IMAGE: &str = "runpod/pytorch:1.0.2-cu1281-torch280-ubuntu2404";
const REST: &str = "https://rest.runpod.io/v1";
const POD_NAME: &str = "anima-agent";
const LLM_LOCAL: u16 = 18199;
const COMFY_LOCAL: u16 = 18188;
/// 雲端只裝 MOSS-Image（跟本機生圖站同一組檔案：int8 主模型、w4a8 文字編碼器、FLUX.2 VAE；Comfy-Org/MOSS-Image）
pub const CLOUD_UNET: &str = "moss_image_sft_int8_convrot.safetensors";
const CLOUD_REPO: &str = "Comfy-Org/MOSS-Image";
/// 閒置幾分鐘自動關機（預設）
const IDLE_MIN: i64 = 20;
/// 開機＋安裝最久等多久（超過就刪機，免得卡著一直計費）
const START_LIMIT: Duration = Duration::from_secs(50 * 60);

/// 三種連法
pub const PROVIDERS: [&str; 3] = ["runpod", "ssh", "url"];
/// cloud.json 裡的金鑰欄位（存檔時加密成 <名稱>_dpapi）
const SECRETS: [&str; 3] = ["runpod_key", "comfy_key", "llm_key"];
/// RunPod 機器上的工作資料夾與埠（新的乾淨機器）
const RUNPOD_DIR: &str = "/workspace/agent";
/// 自己租的主機上用的埠：避開 8188、8080 這些雲端範本常已經占用的
const SSH_LLM_PORT: u16 = 38199;
const SSH_COMFY_PORT: u16 = 38188;

/// 雲端要裝哪些東西（cloud.json 的 packs）：
/// agent＝生圖助手（35B 腦袋＋MOSS 生圖）；h3＝H3 影片（10Eros、文字編碼器、VAE、二采放大、10 個特效 embedding）；
/// h3x＝H3 進階玩法（官方 ref2va＋DMD Turbo、換角色／特效／換畫風／動態桌布／銳化 LoRA、Fun ControlNet、SDPose、YZ 工作流的 Turbo LoRA）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packs {
    pub agent: bool,
    pub h3: bool,
    pub h3x: bool,
}

impl Default for Packs {
    fn default() -> Self {
        Packs { agent: true, h3: false, h3x: false }
    }
}

impl Packs {
    /// 設定裡的 {"agent", "h3", "h3x"}；沒設定＝只有助手（舊版行為）。h3x 一定連 h3；什麼都沒勾就當助手
    pub fn from_value(v: Option<&Value>) -> Packs {
        let Some(o) = v.and_then(Value::as_object) else { return Packs::default() };
        let b = |k: &str| o.get(k).is_some_and(|x| truthy(Some(x)));
        let h3x = b("h3x");
        let h3 = b("h3") || h3x;
        Packs { agent: b("agent") || !h3, h3, h3x }
    }
    pub fn to_value(self) -> Value {
        json!({"agent": self.agent, "h3": self.h3, "h3x": self.h3x})
    }
    /// pod.json、雲端 $A/packs 用的寫法：agent,h3,h3x
    pub fn tag(self) -> String {
        [(self.agent, "agent"), (self.h3, "h3"), (self.h3x, "h3x")].iter().filter(|(b, _)| *b).map(|(_, n)| *n).collect::<Vec<_>>().join(",")
    }
    pub fn from_tag(t: &str) -> Packs {
        let has = |n: &str| t.split(',').any(|x| x.trim() == n);
        Packs { agent: has("agent"), h3: has("h3") || has("h3x"), h3x: has("h3x") }
    }
    /// self 有的包 other 都有
    pub fn within(self, other: Packs) -> bool {
        (!self.agent || other.agent) && (!self.h3 || other.h3) && (!self.h3x || other.h3x)
    }
    /// 模型大約多大（開機進度顯示用，GB）
    pub fn download_gb(self) -> f64 {
        32.0 * self.agent as u8 as f64 + 42.0 * self.h3 as u8 as f64 + 32.0 * self.h3x as u8 as f64
    }
    /// 硬碟至少要多少（GB；ComfyUI＋torch 約 20GB）
    pub fn disk_gb(self) -> i64 {
        20 + 40 * self.agent as i64 + 50 * self.h3 as i64 + 40 * self.h3x as i64
    }
    /// RunPod 的容器硬碟（GB）
    pub fn container_gb(self) -> i64 {
        (self.disk_gb() + 40).max(120)
    }
    /// 顯存至少多少（MB）：一個包 48GB 等級；助手的 35B 常駐＋H3 要 96GB 等級
    pub fn vram_mb(self) -> i64 {
        if self.agent && self.h3 { 90000 } else { 44000 }
    }
}

/// H3 影片包下載的檔：(HF 倉庫, 檔案, 放到 ComfyUI/models 的哪個資料夾)
pub const H3_FILES: &[(&str, &str, &str)] = &[
    ("TenStrip/10Eros-Max", "10Eros_Max_h3_TURBO-hybrid_beta5_int8.safetensors", "diffusion_models"),
    ("Comfy-Org/MiniMax-H3", "text_encoders/qwen3vl_32b_minimax_h3_nvfp4_awq.safetensors", "text_encoders"),
    ("Comfy-Org/MiniMax-H3", "vae/minimax_h3_video_vae_int8_convrot.safetensors", "vae"),
    ("Comfy-Org/MiniMax-H3", "vae/minimax_h3_audio_vae_fp32.safetensors", "vae"),
    ("LBH-123-AI/Minimax_h3_latent_Upscaler", "minimax_h3_latent_upscaler_3d_conv_v1/minimax_h3_latent_upscaler_3d_conv_v1_fp16.safetensors", "latent_upscale_models"),
    ("Comfy-Org/MiniMax-H3", "embeddings/minimaxh3_art_is_explosion.safetensors", "embeddings"),
    ("Comfy-Org/MiniMax-H3", "embeddings/minimaxh3_blooming_flowers.safetensors", "embeddings"),
    ("Comfy-Org/MiniMax-H3", "embeddings/minimaxh3_bullet_time.safetensors", "embeddings"),
    ("Comfy-Org/MiniMax-H3", "embeddings/minimaxh3_dark_magic.safetensors", "embeddings"),
    ("Comfy-Org/MiniMax-H3", "embeddings/minimaxh3_fire_breath.safetensors", "embeddings"),
    ("Comfy-Org/MiniMax-H3", "embeddings/minimaxh3_four_seasons.safetensors", "embeddings"),
    ("Comfy-Org/MiniMax-H3", "embeddings/minimaxh3_kiss_camera.safetensors", "embeddings"),
    ("Comfy-Org/MiniMax-H3", "embeddings/minimaxh3_spiral_ascent.safetensors", "embeddings"),
    ("Comfy-Org/MiniMax-H3", "embeddings/minimaxh3_storm_magic.safetensors", "embeddings"),
    ("Comfy-Org/MiniMax-H3", "embeddings/minimaxh3_truman_show.safetensors", "embeddings"),
];
/// H3 進階包（第二批玩法）
pub const H3X_FILES: &[(&str, &str, &str)] = &[
    ("Comfy-Org/MiniMax-H3", "diffusion_models/minimax_h3_ref2va_pruned_int8_convrot.safetensors", "diffusion_models"),
    ("drbaph/MiniMax-H3-Turbo-Lora-ComfyUI", "experimental/minimax_h3_dmd_ref2va_8step_turbo_pruned.safetensors", "loras"),
    ("drbaph/MiniMax-H3-Turbo-Lora-ComfyUI", "minimax_h3_turbo_v4_step600_ema_pruned_comfyui.safetensors", "loras"),
    ("Alissonerdx/Minimax-H3-ComfyUI", "loras/minimax_h3_character_swap_v1.1_ref2va_fro.safetensors", "loras"),
    ("Alissonerdx/Minimax-H3-ComfyUI", "loras/minimax_h3_vfx_edit_v1.0_r128.safetensors", "loras"),
    ("Alissonerdx/Minimax-H3-ComfyUI", "loras/minimax_h3_style_transfer_v1.0_r64.safetensors", "loras"),
    ("Alissonerdx/Minimax-H3-ComfyUI", "loras/minimax_h3_live_wallpaper_v1.0_ref2va_r64.safetensors", "loras"),
    ("Alissonerdx/Minimax-H3-ComfyUI", "loras/minimax_h3_lms_v1.0_r64.safetensors", "loras"),
    ("Comfy-Org/MiniMax-H3", "model_patches/minimax_h3_fun_controlnet_union_pruned_int8_convrot.safetensors", "model_patches"),
    ("Comfy-Org/SDPose", "checkpoints/sdpose_wholebody_fp16.safetensors", "checkpoints"),
    ("Comfy-Org/SDPose", "diffusion_models/rt_detr_v4-x-hgnet_fp16.safetensors", "diffusion_models"),
];
/// 二采的 3D 潛空間放大節點（只用 torch、einops、safetensors，沒有網路呼叫；2026-10-22 審過這個 commit）
const UPSCALER_NODE: &str = "https://github.com/LBH-123-AI/Comfyui_Minimax_h3_latent_Upscaler.git";
const UPSCALER_COMMIT: &str = "40316cf008b2fd8663263270669eb4da23f89d2c";
/// 「SelfLift 兩段取樣」的節點（只用 torch、comfy；沒有網路、subprocess；2026-10-23 審過這個 commit）。裝不起來只是少這個選項，不擋開機
const SELFLIFT_NODE: &str = "https://github.com/rogen84/SelfLiftH3Sampler.git";
const SELFLIFT_COMMIT: &str = "cb772ed2a02266c9b6efefc149dae2d731d78ded";

/// 雲端機器上跑的安裝腳本（@...@ 會被換掉）。步驟寫進 step 檔，程式每 10 秒看一次；裝完接著跑 run.sh
const BOOT: &str = r#"#!/bin/bash
set -e
A=@DIR@
mkdir -p $A && cd $A
step() { echo "$1" > $A/step; }
die() { echo "$1" >&2; step error; exit 1; }
trap 'step error' ERR
step check
for c in curl git python3 tar nvidia-smi; do command -v $c > /dev/null || die "這台主機缺少指令 $c（要 Ubuntu 22.04 以上、有 NVIDIA 驅動的映像檔）"; done
python3 -c "import venv, ensurepip" 2> /dev/null || die "python3 沒有 venv（Ubuntu 要先裝 python3-venv：apt install python3-venv）"
CV=$(nvidia-smi | grep -o "CUDA Version: [0-9.]*" | grep -o "[0-9.]*$" || echo 0)
CVN=$(echo "$CV" | awk -F. '{print $1 * 100 + $2}')
[ "${CVN:-0}" -ge 1208 ] || die "顯卡驅動太舊（CUDA $CV），要 12.8 以上：換一台，或選新一點的映像檔"
VR=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits | head -1 | tr -dc 0-9)
[ "${VR:-0}" -ge @VRAM_MB@ ] || die "顯存不夠：@VRAM_NEED@（這台第一張卡 ${VR:-0} MB）"
FREE=$(df -Pm $A | awk 'NR==2 {print $4}')
USED=$(du -sm $A 2> /dev/null | cut -f1)
[ $(( ${FREE:-0} + ${USED:-0} )) -ge @DISK_MB@ ] || die "硬碟空間不夠：要 @DISK_GB@GB 以上（剩 ${FREE:-0} MB）"
@LLAMA_GET@step venv
python3 -m venv $A/cvenv
$A/cvenv/bin/pip install -q --upgrade pip
$A/cvenv/bin/pip install -q huggingface_hub hf_transfer
export HF_HUB_ENABLE_HF_TRANSFER=1
@AGENT_DL@@FILES_DL@step comfy
if [ ! -f $A/.comfy_ok ]; then
rm -rf $A/ComfyUI
git clone -q --depth 1 https://github.com/comfyanonymous/ComfyUI.git $A/ComfyUI
IDX=cu128
if [ "${CVN:-0}" -ge 1300 ]; then IDX=cu130; fi
$A/cvenv/bin/pip install -q torch torchvision torchaudio --index-url https://download.pytorch.org/whl/$IDX
$A/cvenv/bin/pip install -q -r $A/ComfyUI/requirements.txt
touch $A/.comfy_ok
fi
@NODES@step models
@WAITS@for sub in diffusion_models text_encoders vae; do if [ -d $A/cm/$sub ]; then mkdir -p $A/ComfyUI/models/$sub; ln -sf $A/cm/$sub/* $A/ComfyUI/models/$sub/; fi; done
if [ -f $A/links.txt ]; then while IFS=$'\t' read -r t f; do [ -n "$f" ] || continue; mkdir -p "$A/ComfyUI/models/$t"; ln -sf "$f" "$A/ComfyUI/models/$t/"; done < $A/links.txt; fi
mkdir -p $A/ComfyUI/models/loras
echo "@PACKS@" > $A/packs
step installed
bash $A/run.sh
"#;

/// 助手包：llama.cpp
const LLAMA_GET: &str = r#"step llama
B=@LLAMA@
curl -fsSL -o llama.tgz https://github.com/ggml-org/llama.cpp/releases/download/$B/llama-$B-bin-ubuntu-cuda-12.8-x64.tar.gz
curl -fsSL -o cudart.tgz https://github.com/ggml-org/llama.cpp/releases/download/$B/cudart-llama-$B-bin-ubuntu-cuda-12.8-x64.tar.gz
mkdir -p llama && tar xzf llama.tgz -C llama && tar xzf cudart.tgz -C llama
"#;

/// 助手包：35B 腦袋＋MOSS 三個檔（背景下載）
const AGENT_DL: &str = r#"$A/cvenv/bin/python - > $A/dl_llm.log 2>&1 <<'PY' &
from huggingface_hub import hf_hub_download as d
d("@LLM_REPO@", "@LLM_FILE@", local_dir="@DIR@/models")
d("@LLM_REPO@", "@MMPROJ@", local_dir="@DIR@/models")
PY
P1=$!
$A/cvenv/bin/python - > $A/dl_img.log 2>&1 <<'PY' &
from huggingface_hub import hf_hub_download as d
for f in ["diffusion_models/@UNET@", "text_encoders/@TE@", "vae/@VAE@"]:
    d("@REPO@", f, local_dir="@DIR@/cm")
PY
P2=$!
"#;

/// H3 包：各倉庫的檔下載到 $A/hf/<倉庫>，清單寫進 links.txt（等 ComfyUI 裝好再連進 models）
const FILES_DL: &str = r#"$A/cvenv/bin/python - > $A/dl_h3.log 2>&1 <<'PY' &
from huggingface_hub import hf_hub_download as d
rows = []
for repo, f, sub in @FILES@:
    rows.append(sub + "\t" + d(repo, f, local_dir="@DIR@/hf/" + repo.replace("/", "__")))
open("@DIR@/links.txt", "w").write("\n".join(rows) + "\n")
PY
P3=$!
"#;

/// H3 包：二采放大節點（固定 commit）
const NODES: &str = r#"N=$A/ComfyUI/custom_nodes/Comfyui_Minimax_h3_latent_Upscaler
if [ ! -d $N ]; then git clone -q @UPNODE@ $N || { rm -rf $N; die "下載 H3 放大節點失敗（GitHub 連不上？）：關機再開一次"; }; git -C $N checkout -q @UPCOMMIT@ || { rm -rf $N; die "H3 放大節點切換版本失敗：關機再開一次"; }; fi
N=$A/ComfyUI/custom_nodes/SelfLiftH3Sampler
if [ ! -d $N ]; then { git clone -q @SLNODE@ $N && git -C $N checkout -q @SLCOMMIT@; } || { rm -rf $N; echo "SelfLift 節點沒裝起來（只是少「SelfLift 兩段取樣」）"; }; fi
N=$A/ComfyUI/custom_nodes/@RMDIR@
if [ ! -d $N ]; then { git clone -q @RMNODE@ $N && git -C $N checkout -q @RMCOMMIT@; } || { rm -rf $N; echo "RefMod 節點沒裝起來（只是少「用 RefMod 鎖臉」）"; }; fi
"#;

/// 啟動（或重新啟動）腦袋和 ComfyUI；只聽 127.0.0.1，外面要走 SSH 通道
const RUN: &str = r#"#!/bin/bash
A=@DIR@
cd $A
@STOP@
echo start > $A/step
@LLAMA_RUN@cd $A
setsid nohup $A/cvenv/bin/python $A/ComfyUI/main.py --listen 127.0.0.1 --port @COMFY_PORT@ < /dev/null > $A/comfy.log 2>&1 &
echo $! > $A/comfy.pid
echo ready > $A/step
"#;

/// 助手包：啟動 llama-server
const LLAMA_RUN: &str = r#"LS=$(find $A/llama -name llama-server -type f | head -1)
cd "$(dirname "$LS")"
LD_LIBRARY_PATH=$PWD:$LD_LIBRARY_PATH setsid nohup ./llama-server -m $A/models/@LLM_FILE@ --mmproj $A/models/@MMPROJ@ --host 127.0.0.1 --port @LLM_PORT@ -c 32768 --jinja --no-webui -np 1 --reasoning-format deepseek -ngl 99 -ctk q8_0 -ctv q8_0 -fa on -b 2048 -ub 2048 -t 6 -tb 7 < /dev/null > $A/llm.log 2>&1 &
echo $! > $A/llm.pid
"#;

/// 停掉這個資料夾開的腦袋和 ComfyUI（只認自己的 pid 檔與路徑，別的程式不碰）
const STOP: &str = r#"for f in $A/llm.pid $A/comfy.pid; do if [ -f $f ]; then kill $(cat $f) 2> /dev/null || true; rm -f $f; fi; done
if command -v pkill > /dev/null; then pkill -f "$A/ComfyUI/main.py" || true; pkill -f "llama-server -m $A/models" || true; fi
sleep 2"#;

/// 跟本機同一份 35B（SC117 量化的 llmfan46 heretic APEX I-Compact，檔案大小 17,016,624,544 位元組一致）
const LLM_REPO: &str = "SC117/Qwen3.6-35B-A3B-uncensored-heretic-Native-MTP-Preserved-APEX-GGUF";
const LLM_FILE: &str = "Qwen3.6-35B-A3B-uncensored-heretic-Native-MTP-Preserved-APEX-I-Compact.gguf";
const MMPROJ: &str = "mmproj-Qwen3.6-35B-A3B-uncensored-heretic-Native-MTP-Preserved-APEX-F16.gguf";
const LLAMA: &str = "b11574";

/// 這組包要下載的檔（Python list 寫法，給 FILES_DL）
fn files_py(packs: Packs) -> String {
    let mut rows: Vec<String> = vec![];
    for (on, list) in [(packs.h3, H3_FILES), (packs.h3x, H3X_FILES)] {
        if on {
            rows.extend(list.iter().map(|(r, f, sub)| format!("(\"{r}\", \"{f}\", \"{sub}\")")));
        }
    }
    format!("[{}]", rows.join(", "))
}

fn fill(s: &str, dir: &str, llm_port: u16, comfy_port: u16, packs: Packs) -> String {
    let pick = |on: bool, t: &str| if on { t.to_string() } else { String::new() };
    let mut waits = String::new();
    if packs.agent {
        waits.push_str("wait $P1\nwait $P2\n");
    }
    if packs.h3 {
        waits.push_str("wait $P3\n");
    }
    let need = if packs.agent && packs.h3 { "助手和 H3 一起用要 96GB 等級的卡（RTX PRO 6000），或在雲端設定只勾一個" } else { "要 48GB 等級的卡" };
    s.replace("@LLAMA_GET@", &pick(packs.agent, LLAMA_GET))
        .replace("@AGENT_DL@", &pick(packs.agent, AGENT_DL))
        .replace("@FILES_DL@", &pick(packs.h3, FILES_DL))
        .replace("@NODES@", &pick(packs.h3, NODES))
        .replace("@LLAMA_RUN@", &pick(packs.agent, LLAMA_RUN))
        .replace("@WAITS@", &waits)
        .replace("@FILES@", &files_py(packs))
        .replace("@UPNODE@", UPSCALER_NODE)
        .replace("@UPCOMMIT@", UPSCALER_COMMIT)
        .replace("@SLNODE@", SELFLIFT_NODE)
        .replace("@SLCOMMIT@", SELFLIFT_COMMIT)
        .replace("@RMDIR@", crate::refmod::DIR)
        .replace("@RMNODE@", crate::refmod::REPO)
        .replace("@RMCOMMIT@", crate::refmod::COMMIT)
        .replace("@VRAM_MB@", &packs.vram_mb().to_string())
        .replace("@VRAM_NEED@", need)
        .replace("@DISK_MB@", &(packs.disk_gb() * 1000).to_string())
        .replace("@DISK_GB@", &packs.disk_gb().to_string())
        .replace("@PACKS@", &packs.tag())
        .replace("@STOP@", STOP)
        .replace("@DIR@", dir)
        .replace("@LLAMA@", LLAMA)
        .replace("@LLM_REPO@", LLM_REPO)
        .replace("@LLM_FILE@", LLM_FILE)
        .replace("@MMPROJ@", MMPROJ)
        .replace("@REPO@", CLOUD_REPO)
        .replace("@UNET@", CLOUD_UNET)
        .replace("@TE@", graphs::MOSS_TE)
        .replace("@VAE@", graphs::MOSS_VAE)
        .replace("@LLM_PORT@", &llm_port.to_string())
        .replace("@COMFY_PORT@", &comfy_port.to_string())
}

/// 舊的只有助手包的版本（測試用）
#[cfg(test)]
pub fn boot_script(dir: &str) -> String {
    boot_script_for(dir, Packs::default())
}

#[cfg(test)]
pub fn run_script(dir: &str, llm_port: u16, comfy_port: u16) -> String {
    run_script_for(dir, llm_port, comfy_port, Packs::default())
}

pub fn boot_script_for(dir: &str, packs: Packs) -> String {
    fill(BOOT, dir, 0, 0, packs)
}

pub fn run_script_for(dir: &str, llm_port: u16, comfy_port: u16, packs: Packs) -> String {
    fill(RUN, dir, llm_port, comfy_port, packs)
}

/// 安裝步驟 → 中文
fn step_zh(step: &str) -> &'static str {
    match step {
        "check" => "檢查主機（驅動、顯存、硬碟）",
        "llama" => "下載 llama.cpp",
        "venv" => "準備 Python",
        "comfy" => "安裝 ComfyUI（模型同時在下載）",
        "models" => "等模型下載完",
        "installed" | "start" => "啟動腦袋和 ComfyUI",
        "ready" => "載入模型中",
        "error" => "安裝失敗",
        _ => "開機中",
    }
}

/// 遠端資料夾、主機、帳號只收安全字元（會放進 ssh 參數和 shell 指令；開頭不能是 -，免得被當成 ssh 選項）
fn safe_word(s: &str, extra: &str) -> bool {
    !s.is_empty() && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

/// LLM 網址：去掉結尾的 / 和 /v1（程式自己會接 /v1/chat/completions）
pub fn norm_url(u: &str) -> String {
    let mut u = u.trim().trim_end_matches('/').to_string();
    if u.ends_with("/v1") {
        u.truncate(u.len() - 3);
    }
    u.trim_end_matches('/').to_string()
}

/// 網址模式缺哪個網址：ComfyUI 一定要；腦袋只有勾了助手才要
fn url_missing(comfy: &str, llm: &str, packs: Packs) -> Result<(), &'static str> {
    if comfy.trim().is_empty() {
        return Err("還沒填 ComfyUI 的網址：按「雲端設定」填好再連");
    }
    if packs.agent && llm.trim().is_empty() {
        return Err("勾了生圖助手要填腦袋的網址：按「雲端設定」填好再連（不用助手就把它取消勾選）");
    }
    Ok(())
}

/// /object_info/MiniMaxH3ReferenceToVideo 的回應裡有這個節點
fn h3_node_ok(v: &Value) -> bool {
    v.get("MiniMaxH3ReferenceToVideo").is_some()
}

/// ComfyUI object_info 裡某欄位的選項（舊格式 [[...]]、新格式 ["COMBO", {options}] 都讀）
fn combo(info: &Value, node: &str, field: &str) -> Vec<String> {
    let spec = &info[node]["input"]["required"][field];
    spec[0].as_array().or_else(|| spec[1]["options"].as_array()).into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect()
}

pub struct Endpoints {
    pub llm: String,
    pub comfy: String,
    pub llm_key: Option<String>,
    pub comfy_key: Option<String>,
    pub model: String,
}

impl Endpoints {
    pub fn remote(&self) -> crate::pe::Remote {
        crate::pe::Remote { base: self.llm.clone(), key: self.llm_key.clone(), model: self.model.clone() }
    }
}

#[derive(Clone, Default)]
struct Pod {
    /// runpod（我們租的，關機＝刪機）／ssh（使用者自己的主機，關機＝停服務）
    kind: String,
    id: String,
    price: f64,
    created: f64,
    ip: String,
    port: u16,
    /// 實際的卡（顯示用）
    gpu: String,
    user: String,
    /// 雲端上的工作資料夾（絕對路徑；ssh 第一次連上才決定）
    dir: String,
    /// 使用者自己指定的私鑰（空＝生圖站自己的 id_ed25519）
    key: String,
    llm_port: u16,
    comfy_port: u16,
    /// 這台裝了哪些功能包（Packs::tag）
    packs: String,
}

impl Pod {
    fn to_value(&self) -> Value {
        json!({"kind": self.kind, "id": self.id, "price": self.price, "created": self.created, "ip": self.ip, "port": self.port, "gpu": self.gpu,
               "user": self.user, "dir": self.dir, "key": self.key, "llm_port": self.llm_port, "comfy_port": self.comfy_port, "packs": self.packs})
    }
    /// 舊版 pod.json 只有 RunPod 的欄位：缺的照 RunPod 補
    fn from_value(v: &Value) -> Option<Pod> {
        let id = tx(&v["id"]);
        let s = |k: &str, d: &str| Some(tx(&v[k])).filter(|x| !x.is_empty()).unwrap_or_else(|| d.to_string());
        let port = |k: &str, d: u16| v[k].as_u64().map(|p| p as u16).filter(|p| *p > 0).unwrap_or(d);
        (!id.is_empty()).then(|| Pod {
            kind: s("kind", "runpod"),
            id,
            price: v["price"].as_f64().unwrap_or(PRICE_HINT),
            created: v["created"].as_f64().unwrap_or_else(now),
            ip: tx(&v["ip"]),
            port: v["port"].as_u64().unwrap_or(0) as u16,
            gpu: tx(&v["gpu"]),
            user: s("user", "root"),
            dir: s("dir", RUNPOD_DIR),
            key: tx(&v["key"]),
            llm_port: port("llm_port", 8199),
            comfy_port: port("comfy_port", 8188),
            packs: s("packs", "agent"),
        })
    }
}

/// RunPod 回的機器資料裡的顯卡名稱（欄位名稱不確定，幾個常見的都看）
fn gpu_name_of(r: &Value) -> Option<String> {
    [&r["gpu"]["displayName"], &r["machine"]["gpuDisplayName"], &r["machine"]["gpuTypeId"], &r["gpu"]["id"]]
        .into_iter()
        .map(tx)
        .find(|s| !s.trim().is_empty())
        .map(|s| s.trim_start_matches("NVIDIA ").to_string())
}

struct St {
    /// off／starting／ready／stopping／error／left（上次程式關掉時沒關的機器）
    phase: &'static str,
    msg: String,
    pod: Option<Pod>,
    last_used: f64,
    idle_min: i64,
    /// 本機 LoRA 名稱 → 雲端 loras 資料夾裡的檔名（已經傳上去的）
    loras: HashMap<String, String>,
    /// 下次開機要租的卡（GpuChoice 的 key）
    gpu: String,
    /// 已經傳上雲端 input 的素材（H3 影片）
    inputs: std::collections::HashSet<String>,
}

pub struct Cloud {
    dir: PathBuf,
    st: Mutex<St>,
    /// cloud.json（金鑰已解密，只留在記憶體）
    cfg: Mutex<Map<String, Value>>,
    tunnel: Mutex<Option<std::process::Child>>,
    /// 開機中的工作（按「關機」時中止）
    job: Mutex<Option<CancellationToken>>,
    http: reqwest::Client,
    catalog: Mutex<Catalog>,
}

fn no_window(cmd: &mut tokio::process::Command) {
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Windows 內建的 OpenSSH（ssh.exe／ssh-keygen.exe）
fn ssh_bin(name: &str) -> PathBuf {
    let sys = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let p = sys.join("System32").join("OpenSSH").join(format!("{name}.exe"));
    if p.exists() {
        p
    } else {
        PathBuf::from(name)
    }
}

/// RunPod 金鑰的備用來源：環境變數，或使用者環境變數（setx 設的，不用重開程式）
fn env_key() -> Option<String> {
    if let Some(k) = std::env::var("RUNPOD_API_KEY").ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) {
        return Some(k);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("reg")
            .args(["query", r"HKCU\Environment", "/v", "RUNPOD_API_KEY"])
            .creation_flags(0x0800_0000)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 && parts[0] == "RUNPOD_API_KEY" && parts[1].starts_with("REG_") {
                return Some(parts[2].to_string());
            }
        }
    }
    None
}

#[cfg(windows)]
fn seal(s: &str) -> Result<String, String> {
    crate::settings::protect(s).map_err(|e| format!("加密金鑰失敗：{e}"))
}
#[cfg(not(windows))]
fn seal(s: &str) -> Result<String, String> {
    Ok(s.to_string())
}
#[cfg(windows)]
fn unseal(s: &str) -> Option<String> {
    crate::settings::unprotect(s).ok()
}
#[cfg(not(windows))]
fn unseal(s: &str) -> Option<String> {
    Some(s.to_string())
}

/// cloud.json → 設定（金鑰解密）
fn load_catalog(dir: &Path) -> Catalog {
    let v = std::fs::read(dir.join("gpus.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).unwrap_or(Value::Null);
    Catalog { at: v["at"].as_f64().unwrap_or(0.0), live: parse_catalog(&v["raw"]), ..Default::default() }
}

fn load_cfg(dir: &Path) -> Map<String, Value> {
    let mut m = std::fs::read(dir.join("cloud.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    for k in SECRETS {
        if let Some(blob) = m.remove(&format!("{k}_dpapi")).and_then(|v| v.as_str().map(str::to_owned)) {
            if let Some(plain) = unseal(&blob) {
                m.insert(k.into(), json!(plain));
            }
        }
    }
    m
}

/// 設定 → 磁碟上的樣子（金鑰加密、明文欄位拿掉）
fn sealed(m: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    let mut disk = m.clone();
    for k in SECRETS {
        let v = disk.remove(k).and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
        if !v.is_empty() {
            disk.insert(format!("{k}_dpapi"), json!(seal(&v)?));
        }
    }
    Ok(disk)
}

impl Cloud {
    pub fn preparing(&self) -> bool {
        matches!(self.st().phase, "starting" | "stopping")
    }
    pub fn new(dir: &Path) -> Cloud {
        let pod = std::fs::read(dir.join("pod.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| Pod::from_value(&v));
        let left = pod.as_ref().map(|p| p.kind.clone());
        Cloud {
            dir: dir.to_path_buf(),
            st: Mutex::new(St {
                phase: if left.is_some() { "left" } else { "off" },
                msg: match left.as_deref() {
                    Some("ssh") => "上次連的主機可能還開著：按「接回來」繼續用，或按「停止」停掉上面的服務".into(),
                    Some(_) => "上次的雲端機器可能還開著（還在計費）：按「接回來」繼續用，或按「關機」刪掉".into(),
                    None => String::new(),
                },
                pod,
                last_used: now(),
                idle_min: IDLE_MIN,
                loras: HashMap::new(),
                gpu: GPUS[0].key.to_string(),
                inputs: Default::default(),
            }),
            cfg: Mutex::new(load_cfg(dir)),
            tunnel: Mutex::new(None),
            job: Mutex::new(None),
            http: reqwest::Client::builder()
                .proxy(crate::net::proxy())
                .connect_timeout(Duration::from_secs(15))
                .read_timeout(Duration::from_secs(180))
                .build()
                .expect("建立 HTTP client"),
            catalog: Mutex::new(load_catalog(dir)),
        }
    }

    fn catalog(&self) -> std::sync::MutexGuard<'_, Catalog> {
        self.catalog.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn choices(&self) -> Vec<GpuChoice> {
        gpu_choices(&self.catalog().live)
    }

    fn choice(&self, key: &str) -> Option<GpuChoice> {
        self.choices().into_iter().find(|g| g.key == key)
    }

    /// 顯卡目錄太舊就重抓（force＝不管多舊都抓）；同一時間只抓一次
    pub async fn refresh_gpus(&self, force: bool) -> Result<(), String> {
        {
            let mut c = self.catalog();
            let t = now();
            if c.busy || (!force && (t - c.at < CATALOG_TTL || t - c.tried < CATALOG_RETRY)) {
                return Ok(());
            }
            c.busy = true;
            c.tried = t;
        }
        let r = self.fetch_catalog().await;
        let mut c = self.catalog();
        c.busy = false;
        let raw = r?;
        let live = parse_catalog(&raw);
        if live.is_empty() {
            return Err("RunPod 的顯卡目錄是空的".into());
        }
        c.at = now();
        c.live = live;
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(self.dir.join("gpus.json"), json!({"at": c.at, "raw": raw}).to_string());
        Ok(())
    }

    async fn fetch_catalog(&self) -> Result<Value, String> {
        let resp = self
            .http
            .post(CATALOG)
            .json(&json!({"query": CATALOG_Q}))
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|e| format!("連不到 RunPod：{}", crate::net::explain(&e, CATALOG)))?;
        if !resp.status().is_success() {
            return Err(format!("RunPod 顯卡目錄回應 {}", resp.status().as_u16()));
        }
        resp.json::<Value>().await.map_err(|e| format!("RunPod 顯卡目錄讀不懂：{e}"))
    }

    fn st(&self) -> std::sync::MutexGuard<'_, St> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn cfg(&self) -> Map<String, Value> {
        self.cfg.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 設定裡的一個字串（沒有就空字串）
    fn c(&self, k: &str) -> String {
        self.cfg.lock().unwrap_or_else(|e| e.into_inner()).get(k).map(tx).unwrap_or_default().trim().to_string()
    }

    /// 設定裡勾的功能包（下次開機要裝的）
    pub fn packs(&self) -> Packs {
        Packs::from_value(self.cfg().get("packs"))
    }

    /// 現在這台雲端裝了哪些包：自己開的機器看 pod.json；網址模式照設定（使用者自己負責那台有沒有裝）
    pub fn live_packs(&self) -> Option<Packs> {
        let s = self.st();
        if s.phase != "ready" {
            return None;
        }
        match &s.pod {
            Some(p) => Some(Packs::from_tag(&p.packs)),
            None => Some(Packs::from_value(self.cfg.lock().unwrap_or_else(|e| e.into_inner()).get("packs"))),
        }
    }

    pub fn provider(&self) -> &'static str {
        let p = self.c("provider");
        PROVIDERS.iter().find(|x| **x == p).copied().unwrap_or("runpod")
    }

    fn api_key(&self) -> Option<String> {
        Some(self.c("runpod_key")).filter(|k| !k.is_empty()).or_else(env_key)
    }

    /// 目前的連法缺什麼（網頁顯示用；沒缺回 Ok）
    fn missing(&self) -> Result<(), String> {
        match self.provider() {
            "ssh" if self.c("ssh_host").is_empty() => Err("還沒填主機位址：按「雲端設定」填好再連".into()),
            "url" => url_missing(&self.c("comfy_url"), &self.c("llm_url"), self.packs()).map_err(String::from),
            "runpod" if self.api_key().is_none() => Err("還沒填 RunPod 金鑰：按「雲端設定」貼上（RunPod 網站 Settings → API Keys）".into()),
            _ => Ok(()),
        }
    }

    fn set(&self, app: &Shared, phase: &'static str, msg: impl Into<String>) {
        {
            let mut s = self.st();
            s.phase = phase;
            s.msg = msg.into();
        }
        app.hub.send(json!({"type": "agent_cloud", "status": self.status()}));
    }

    fn save_pod(&self, pod: Option<&Pod>) {
        let f = self.dir.join("pod.json");
        match pod {
            Some(p) => {
                let _ = std::fs::create_dir_all(&self.dir);
                if let Err(e) = crate::director::write_atomic(&f, &p.to_value()) {
                    eprintln!("記錄雲端機器失敗：{e}");
                }
            }
            None => {
                let _ = std::fs::remove_file(&f);
            }
        }
    }

    /// 給網頁看的設定（金鑰只說有沒有填）
    fn cfg_view(&self) -> Value {
        let m = self.cfg();
        let mut v = Map::new();
        for k in ["ssh_host", "ssh_port", "ssh_user", "ssh_dir", "ssh_key", "ssh_price", "ssh_shutdown", "comfy_url", "llm_url", "llm_model"] {
            if let Some(x) = m.get(k) {
                v.insert(k.into(), x.clone());
            }
        }
        let has = |k: &str| m.get(k).and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty());
        v.insert("provider".into(), json!(self.provider()));
        v.insert("packs".into(), self.packs().to_value());
        v.insert("has_runpod_key".into(), json!(has("runpod_key")));
        v.insert("runpod_env".into(), json!(env_key().is_some()));
        v.insert("has_comfy_key".into(), json!(has("comfy_key")));
        v.insert("has_llm_key".into(), json!(has("llm_key")));
        Value::Object(v)
    }

    /// 閒置自動關有沒有作用：RunPod 一定有；自己的主機要勾「順便關主機」；網址模式沒有
    fn idle_applies(&self, pod: Option<&Pod>) -> bool {
        match pod {
            Some(p) if p.kind == "runpod" => true,
            Some(_) => truthy(self.cfg().get("ssh_shutdown")),
            None => false,
        }
    }

    /// 網頁用（不含金鑰、不含 IP）
    pub fn status(&self) -> Value {
        let choices = self.choices();
        let gpus_at = self.catalog().at;
        let s = self.st();
        let opt = choices.iter().find(|g| g.key == s.gpu).unwrap_or(&choices[0]);
        let (price, mins, cost) = match &s.pod {
            Some(p) => {
                let mins = ((now() - p.created) / 60.0).max(0.0);
                (p.price, mins.round(), (p.price * mins / 60.0 * 100.0).round() / 100.0)
            }
            None => (opt.price_hi, 0.0, 0.0),
        };
        // 自己的主機還沒讀到顯卡名稱時不要冒出 RunPod 選項的卡名
        let gpu = match &s.pod {
            Some(p) if !p.gpu.is_empty() => p.gpu.clone(),
            Some(p) if p.kind == "ssh" => "自己的主機".to_string(),
            _ => opt.name.to_string(),
        };
        let gpus: Vec<Value> = choices
            .iter()
            .map(|g| json!({"key": g.key, "name": g.name, "price_lo": g.price_lo, "price_hi": g.price_hi, "note": g.note, "vram": g.vram,
                            "stock": g.stock, "preset": g.preset}))
            .collect();
        let idle_on = self.idle_applies(s.pod.as_ref());
        let idle_left = (s.phase == "ready" && idle_on).then(|| ((s.idle_min as f64 * 60.0 - (now() - s.last_used)) / 60.0).max(0.0).ceil());
        let pubkey = std::fs::read_to_string(self.key_file().with_extension("pub")).ok().map(|k| k.trim().to_string());
        let missing = self.missing().err();
        json!({"phase": s.phase, "msg": s.msg, "pod": s.pod.as_ref().map(|p| p.id.clone()), "kind": s.pod.as_ref().map(|p| p.kind.clone()),
               "gpu": gpu, "gpu_key": opt.key, "gpus": gpus, "gpus_at": gpus_at, "price": price, "minutes": mins, "cost": cost,
               "idle_min": s.idle_min, "idle_left": idle_left, "idle_on": idle_on, "has_key": missing.is_none(), "missing": missing,
               "provider": self.provider(), "cfg": self.cfg_view(), "pubkey": pubkey, "model": CLOUD_UNET,
               "packs": Packs::from_value(self.cfg.lock().unwrap_or_else(|e| e.into_inner()).get("packs")).to_value(),
               "pod_packs": s.pod.as_ref().map(|p| Packs::from_tag(&p.packs).to_value())})
    }

    /// 雲端可以用的時候才有：llama-server 和 ComfyUI 的網址。SSH 通道斷了會自己重開
    pub fn endpoints(&self) -> Option<Endpoints> {
        let pod = {
            let s = self.st();
            if s.phase != "ready" {
                return None;
            }
            s.pod.clone()
        };
        let Some(pod) = pod else {
            // 網址模式：連線時已經檢查過
            if self.provider() != "url" {
                return None;
            }
            let key = |k: &str| Some(self.c(k)).filter(|s| !s.is_empty());
            let model = self.c("llm_model");
            return Some(Endpoints {
                llm: norm_url(&self.c("llm_url")),
                comfy: self.c("comfy_url").trim_end_matches('/').to_string(),
                llm_key: key("llm_key"),
                comfy_key: key("comfy_key"),
                model: if model.is_empty() { "remote".into() } else { model },
            });
        };
        let alive = {
            let mut t = self.tunnel.lock().unwrap_or_else(|e| e.into_inner());
            match t.as_mut() {
                Some(c) => matches!(c.try_wait(), Ok(None)),
                None => false,
            }
        };
        if !alive {
            if let Err(e) = self.open_tunnel(&pod) {
                eprintln!("雲端通道重開失敗：{e}");
                return None;
            }
        }
        // 自己架的一律是 35B-A3B（MoE）
        Some(Endpoints {
            llm: format!("http://127.0.0.1:{LLM_LOCAL}"),
            comfy: format!("http://127.0.0.1:{COMFY_LOCAL}"),
            llm_key: None,
            comfy_key: None,
            model: "remote-a3b".into(),
        })
    }

    pub fn touch(&self) {
        self.st().last_used = now();
    }

    fn key_file(&self) -> PathBuf {
        self.dir.join("id_ed25519")
    }

    fn ssh_args(&self, pod: &Pod) -> Vec<String> {
        let key = if pod.key.is_empty() { self.key_file() } else { PathBuf::from(&pod.key) };
        vec![
            "-i".into(),
            key.to_string_lossy().into(),
            "-o".into(),
            "IdentitiesOnly=yes".into(),
            "-o".into(),
            "StrictHostKeyChecking=no".into(),
            "-o".into(),
            format!("UserKnownHostsFile={}", self.dir.join(format!("known_hosts_{}", pod.id)).to_string_lossy()),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "ConnectTimeout=20".into(),
            "-o".into(),
            "LogLevel=ERROR".into(),
            "-o".into(),
            "ServerAliveInterval=30".into(),
            "-p".into(),
            pod.port.to_string(),
        ]
    }

    /// 在雲端跑一個指令（stdin 可以餵資料，例如上傳檔案）；回傳標準輸出
    async fn ssh(&self, pod: &Pod, cmd: &str, stdin: Option<&Path>, timeout: Duration) -> Result<String, String> {
        let mut c = tokio::process::Command::new(ssh_bin("ssh"));
        c.args(self.ssh_args(pod))
            .arg(format!("{}@{}", pod.user, pod.ip))
            .arg(cmd)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        no_window(&mut c);
        let mut child = c.spawn().map_err(|e| format!("執行 ssh 失敗：{e}"))?;
        if let Some(src) = stdin {
            let mut inp = child.stdin.take().ok_or("ssh stdin")?;
            let mut f = tokio::fs::File::open(src).await.map_err(|e| format!("讀 {} 失敗：{e}", src.display()))?;
            tokio::io::copy(&mut f, &mut inp).await.map_err(|e| format!("上傳失敗：{e}"))?;
            inp.shutdown().await.ok();
            drop(inp);
        }
        let out = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| "連雲端逾時".to_string())?
            .map_err(|e| format!("ssh 失敗：{e}"))?;
        if !out.status.success() {
            return Err(format!("雲端指令失敗：{}", String::from_utf8_lossy(&out.stderr).chars().take(300).collect::<String>()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// 寫一個腳本到雲端的工作資料夾
    async fn upload_text(&self, pod: &Pod, name: &str, text: &str) -> Result<(), String> {
        let f = self.dir.join(name);
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("建立 {} 失敗：{e}", self.dir.display()))?;
        std::fs::write(&f, text.replace("\r\n", "\n")).map_err(|e| format!("寫 {name} 失敗：{e}"))?;
        self.ssh(pod, &format!("mkdir -p '{d}' && cat > '{d}/{name}'", d = pod.dir), Some(&f), Duration::from_secs(60)).await?;
        Ok(())
    }

    fn open_tunnel(&self, pod: &Pod) -> Result<(), String> {
        let mut t = self.tunnel.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut old) = t.take() {
            let _ = old.kill();
            let _ = old.wait();
        }
        let mut c = std::process::Command::new(ssh_bin("ssh"));
        c.args(self.ssh_args(pod))
            .args([
                "-N",
                "-o",
                "ExitOnForwardFailure=yes",
                "-L",
                &format!("127.0.0.1:{LLM_LOCAL}:127.0.0.1:{}", pod.llm_port),
                "-L",
                &format!("127.0.0.1:{COMFY_LOCAL}:127.0.0.1:{}", pod.comfy_port),
            ])
            .arg(format!("{}@{}", pod.user, pod.ip))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000);
        }
        *t = Some(c.spawn().map_err(|e| format!("開 SSH 通道失敗：{e}"))?);
        Ok(())
    }

    fn close_tunnel(&self) {
        if let Some(mut c) = self.tunnel.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    fn tunnel_dead(&self) -> bool {
        self.tunnel.lock().unwrap_or_else(|e| e.into_inner()).as_mut().is_none_or(|c| !matches!(c.try_wait(), Ok(None)))
    }

    /// 通道那頭的腦袋和 ComfyUI 是不是都好了
    async fn healthy(&self) -> (bool, bool) {
        let agent = self.st().pod.as_ref().is_none_or(|p| Packs::from_tag(&p.packs).agent);
        let get = |u: String| self.http.get(u).timeout(Duration::from_secs(5)).send();
        let llm = !agent || get(format!("http://127.0.0.1:{LLM_LOCAL}/health")).await.is_ok_and(|r| r.status().is_success());
        let comfy = get(format!("http://127.0.0.1:{COMFY_LOCAL}/system_stats")).await.is_ok_and(|r| r.status().is_success());
        (llm, comfy)
    }

    async fn api(&self, method: reqwest::Method, path: &str, body: Option<&Value>) -> Result<Value, String> {
        let key = self.api_key().ok_or("找不到 RunPod 金鑰：按「雲端設定」貼上（RunPod 網站 Settings → API Keys）")?;
        let mut r = self.http.request(method, format!("{REST}{path}")).bearer_auth(key);
        if let Some(b) = body {
            r = r.json(b);
        }
        let resp = r.send().await.map_err(|e| format!("連不到 RunPod：{}", crate::net::explain(&e, REST)))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("RunPod 回應 {}：{}", status.as_u16(), text.chars().take(300).collect::<String>()));
        }
        Ok(if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::Null) })
    }

    /// 網址模式的 HTTP 請求（有 token 就帶上）
    fn req(&self, method: reqwest::Method, url: String, key: Option<&str>) -> reqwest::RequestBuilder {
        let r = self.http.request(method, url);
        match key {
            Some(k) if !k.is_empty() => r.bearer_auth(k),
            _ => r,
        }
    }

    /// 沒有 SSH 金鑰就用 Windows 內建的 ssh-keygen 做一把，權限收緊到只有自己（OpenSSH 會拒絕別人讀得到的私鑰）
    async fn ensure_key(&self) -> Result<String, String> {
        let k = self.key_file();
        if !k.exists() {
            std::fs::create_dir_all(&self.dir).map_err(|e| format!("建立 {} 失敗：{e}", self.dir.display()))?;
            let mut c = tokio::process::Command::new(ssh_bin("ssh-keygen"));
            c.args(["-q", "-t", "ed25519", "-N", "", "-C", "anima-agent", "-f"]).arg(&k).stdin(Stdio::null());
            no_window(&mut c);
            let st = c.status().await.map_err(|e| format!("執行 ssh-keygen 失敗：{e}（Windows 設定 → 選用功能 → OpenSSH 用戶端）"))?;
            if !st.success() {
                return Err("ssh-keygen 失敗".into());
            }
        }
        #[cfg(windows)]
        if let Ok(user) = std::env::var("USERNAME") {
            let mut c = tokio::process::Command::new("icacls");
            c.arg(&k).args(["/inheritance:r", "/grant:r", &format!("{user}:F")]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            no_window(&mut c);
            let _ = c.status().await;
        }
        let pubk = std::fs::read_to_string(k.with_extension("pub")).map_err(|e| format!("讀公鑰失敗：{e}"))?;
        Ok(pubk.trim().to_string())
    }

    /// 設定裡的自己主機 → Pod（還沒決定資料夾）
    fn ssh_pod(&self) -> Pod {
        let m = self.cfg();
        let host = self.c("ssh_host");
        let port = m.get("ssh_port").and_then(Value::as_u64).filter(|p| (1..=65535).contains(p)).unwrap_or(22) as u16;
        let user = Some(self.c("ssh_user")).filter(|u| !u.is_empty()).unwrap_or_else(|| "root".into());
        let id: String = format!("ssh-{host}-{port}").chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
        Pod {
            kind: "ssh".into(),
            id,
            price: m.get("ssh_price").and_then(Value::as_f64).unwrap_or(0.0),
            created: now(),
            ip: host,
            port,
            gpu: String::new(),
            user,
            dir: self.c("ssh_dir"),
            key: self.c("ssh_key"),
            llm_port: SSH_LLM_PORT,
            comfy_port: SSH_COMFY_PORT,
            packs: self.packs().tag(),
        }
    }

    /// 自己的主機：決定工作資料夾（設定沒填就用 /workspace/anima_agent，不能寫就用家目錄）、讀顯卡名稱
    async fn ssh_probe(&self, pod: &mut Pod) -> Result<String, String> {
        let want = pod.dir.clone();
        let cmd = format!(
            "D='{want}'; case \"$D\" in '') if [ -d /workspace ] && [ -w /workspace ]; then D=/workspace/anima_agent; else D=$HOME/anima_agent; fi;; '~/'*) D=$HOME/${{D#'~/'}};; esac; \
             mkdir -p \"$D\" && echo \"$D\"; nvidia-smi --query-gpu=name,memory.total --format=csv,noheader 2>/dev/null | head -1; \
             df -Pm \"$D\" | awk 'NR==2 {{print $4}}'"
        );
        let out = self.ssh(pod, &cmd, None, Duration::from_secs(40)).await?;
        let mut lines = out.lines().map(str::trim);
        let dir = lines.next().unwrap_or("").to_string();
        if !dir.starts_with('/') || !safe_word(&dir, "/._-~") {
            return Err(format!("主機上的資料夾不能用：{dir}"));
        }
        pod.dir = dir;
        let gpu = lines.next().unwrap_or("").to_string();
        let free: f64 = lines.next().and_then(|l| l.parse().ok()).unwrap_or(0.0);
        pod.gpu = gpu.split(',').next().unwrap_or("").trim().trim_start_matches("NVIDIA ").to_string();
        Ok(format!(
            "{}；硬碟剩 {:.0} GB；資料夾 {}",
            if gpu.is_empty() { "找不到 NVIDIA 顯卡（nvidia-smi 不能用）".to_string() } else { gpu.replace(", ", "、") },
            free / 1024.0,
            pod.dir
        ))
    }

    /// 網址模式：照勾的功能包檢查（助手：MOSS 三個檔＋腦袋；H3：有 H3 節點）
    async fn check_url(&self) -> Result<String, String> {
        let packs = self.packs();
        url_missing(&self.c("comfy_url"), &self.c("llm_url"), packs)?;
        let comfy = self.c("comfy_url").trim_end_matches('/').to_string();
        let llm = norm_url(&self.c("llm_url"));
        let urls = if packs.agent { vec![&comfy, &llm] } else { vec![&comfy] };
        for u in urls {
            if !(u.starts_with("http://") || u.starts_with("https://")) {
                return Err(format!("網址要用 http:// 或 https:// 開頭：{u}"));
            }
        }
        let (ck, lk) = (self.c("comfy_key"), self.c("llm_key"));
        let get = |u: String, k: &str| self.req(reqwest::Method::GET, u, Some(k)).timeout(Duration::from_secs(20)).send();
        let r = get(format!("{comfy}/system_stats"), &ck).await.map_err(|e| format!("連不到 ComfyUI（{comfy}）：{e}"))?;
        if !r.status().is_success() {
            return Err(format!("ComfyUI 回應 {}（網址或 token 不對？）", r.status().as_u16()));
        }
        let stats: Value = r.json().await.unwrap_or(Value::Null);
        let gpu = tx(&stats["devices"][0]["name"]);
        let gpu = if gpu.is_empty() { "讀不到顯卡名稱".to_string() } else { gpu };
        if packs.h3 {
            let v: Value = get(format!("{comfy}/object_info/MiniMaxH3ReferenceToVideo"), &ck)
                .await
                .map_err(|e| format!("讀 ComfyUI 節點清單失敗：{e}"))?
                .json()
                .await
                .unwrap_or(Value::Null);
            if !h3_node_ok(&v) {
                return Err("遠端 ComfyUI 沒有 MiniMax H3 節點：更新 ComfyUI 到有 H3 的版本（不用 H3 影片台就在雲端設定取消勾選）".into());
            }
        }
        if !packs.agent {
            return Ok(format!("ComfyUI 好了（{gpu}）"));
        }
        let mut info = Map::new();
        for node in ["UNETLoader", "CLIPLoader", "VAELoader"] {
            let v: Value = get(format!("{comfy}/object_info/{node}"), &ck).await.map_err(|e| format!("讀 ComfyUI 模型清單失敗：{e}"))?.json().await.unwrap_or(Value::Null);
            if let Some(x) = v.get(node) {
                info.insert(node.into(), x.clone());
            }
        }
        let info = Value::Object(info);
        if !combo(&info, "CLIPLoader", "type").iter().any(|t| t == "moss_image") {
            return Err("遠端 ComfyUI 太舊，還沒有 MOSS-Image 支援：更新到最新版（2026-10 之後的 master）".into());
        }
        let miss: Vec<&str> = [("UNETLoader", "unet_name", CLOUD_UNET), ("CLIPLoader", "clip_name", graphs::MOSS_TE), ("VAELoader", "vae_name", graphs::MOSS_VAE)]
            .into_iter()
            .filter(|t| !combo(&info, t.0, t.1).iter().any(|x| x == t.2))
            .map(|t| t.2)
            .collect();
        if !miss.is_empty() {
            return Err(format!(
                "遠端 ComfyUI 缺少 MOSS-Image 的檔案：{}（Hugging Face 的 {CLOUD_REPO}，分別放進 models 的 diffusion_models、text_encoders、vae）",
                miss.join("、")
            ));
        }
        let r = get(format!("{llm}/v1/models"), &lk).await.map_err(|e| format!("連不到腦袋（{llm}）：{e}"))?;
        if !r.status().is_success() {
            return Err(format!("腦袋回應 {}（網址或金鑰不對？要 OpenAI 相容的網址，例如 llama-server、vLLM）", r.status().as_u16()));
        }
        let models: Value = r.json().await.unwrap_or(Value::Null);
        let first = tx(&models["data"][0]["id"]);
        Ok(format!("ComfyUI 好了（{gpu}）；腦袋好了（{}）", if first.is_empty() { "模型名稱讀不到".into() } else { first }))
    }
}

// ================================================================ 開機
/// 這些 step 代表 boot.sh 已經跑完（ComfyUI、模型都裝好了）
const INSTALLED: &[&str] = &["installed", "start", "ready"];

/// 讀「裝到哪、裝了哪些包」的指令：每一段前面加記號（step 檔本身就有換行，不能靠第幾行）
fn install_probe(d: &str) -> String {
    format!("printf 'S:'; cat '{d}/step' 2>/dev/null; echo; printf 'P:'; cat '{d}/packs' 2>/dev/null; echo")
}

/// install_probe 的輸出 → (step, 已經裝的包)。舊版裝的機器沒有 packs 檔：當成只有助手（舊版只會裝它）
fn parse_install(out: &str) -> (String, Packs) {
    let (mut step, mut have) = (String::new(), None);
    for l in out.lines() {
        if let Some(x) = l.strip_prefix("S:") {
            step = x.trim().to_string();
        } else if let Some(x) = l.strip_prefix("P:").map(str::trim).filter(|x| !x.is_empty()) {
            have = Some(Packs::from_tag(x));
        }
    }
    (step, have.unwrap_or_default())
}

async fn start_run(app: &Shared, token: &CancellationToken) -> Result<(), String> {
    let cloud = &app.agent.cloud;
    let existing = cloud.st().pod.clone();
    if existing.is_none() && cloud.provider() == "url" {
        cloud.set(app, "starting", "連線中…");
        let msg = cloud.check_url().await?;
        {
            let mut s = cloud.st();
            s.loras.clear();
            s.inputs.clear();
        }
        cloud.touch();
        cloud.set(app, "ready", msg);
        return Ok(());
    }
    let mut pod = match existing {
        Some(p) => p,
        None if cloud.provider() == "ssh" => {
            cloud.set(app, "starting", "準備 SSH 金鑰…");
            cloud.ensure_key().await?;
            let p = cloud.ssh_pod();
            cloud.save_pod(Some(&p));
            cloud.st().pod = Some(p.clone());
            p
        }
        None => {
            cloud.set(app, "starting", "準備 SSH 金鑰…");
            let pubk = cloud.ensure_key().await?;
            let key = cloud.st().gpu.clone();
            let opt = cloud.choice(&key).ok_or_else(|| format!("顯卡清單裡找不到選的那張卡（{key}）：按顯卡清單旁的 ↻ 再選一次"))?;
            cloud.set(app, "starting", format!("租 {}…", opt.name));
            let body = json!({
                "name": POD_NAME, "imageName": IMAGE, "gpuTypeIds": opt.ids, "gpuCount": 1, "cloudType": "SECURE",
                "containerDiskInGb": cloud.packs().container_gb(), "volumeInGb": 0, "ports": ["22/tcp"], "env": {"PUBLIC_KEY": pubk},
            });
            // 卡被租光時 RunPod 回 500（no instances currently available／does not have the resources）：講人話，請使用者等一下或換卡
            let r = cloud.api(reqwest::Method::POST, "/pods", Some(&body)).await.map_err(|e| {
                if e.contains("no instances") || e.contains("does not have the resources") {
                    format!("RunPod 現在租不到 {}（被租光了，沒有開出機器、不會計費）。等幾分鐘再按一次，或換一張卡", opt.name)
                } else {
                    e
                }
            })?;
            let id = tx(&r["id"]);
            if id.is_empty() {
                return Err(format!("RunPod 沒有回機器編號：{}", r.to_string().chars().take(200).collect::<String>()));
            }
            let p = Pod {
                kind: "runpod".into(),
                id,
                price: r["costPerHr"].as_f64().unwrap_or(opt.price_hi),
                created: now(),
                gpu: gpu_name_of(&r).unwrap_or_else(|| opt.name.to_string()),
                user: "root".into(),
                dir: RUNPOD_DIR.into(),
                llm_port: 8199,
                comfy_port: 8188,
                packs: cloud.packs().tag(),
                ..Default::default()
            };
            cloud.save_pod(Some(&p));
            cloud.st().pod = Some(p.clone());
            p
        }
    };
    // RunPod：等它拿到 IP 和 SSH port
    let t0 = std::time::Instant::now();
    while pod.kind == "runpod" && (pod.ip.is_empty() || pod.port == 0) {
        if token.is_cancelled() {
            return Err("已取消".into());
        }
        if t0.elapsed() > Duration::from_secs(15 * 60) {
            return Err("機器 15 分鐘還沒開好".into());
        }
        cloud.set(app, "starting", "等機器開機…");
        let r = cloud.api(reqwest::Method::GET, &format!("/pods/{}", pod.id), None).await?;
        if let Some(p) = r["costPerHr"].as_f64() {
            pod.price = p;
        }
        if let Some(g) = gpu_name_of(&r) {
            pod.gpu = g;
        }
        let ip = tx(&r["publicIp"]);
        let port = r["portMappings"]["22"].as_u64().unwrap_or(0) as u16;
        if !ip.is_empty() && port > 0 {
            pod.ip = ip;
            pod.port = port;
            cloud.save_pod(Some(&pod));
            cloud.st().pod = Some(pod.clone());
            break;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    // 等 SSH 能連（自己的主機本來就開著：連不上多半是位址、帳號或公鑰沒加，不用等那麼久）
    let max_tries = if pod.kind == "ssh" { 3 } else { 30 };
    let mut tries = 0;
    loop {
        if token.is_cancelled() {
            return Err("已取消".into());
        }
        match cloud.ssh(&pod, "echo ok", None, Duration::from_secs(40)).await {
            Ok(_) => break,
            Err(e) if tries >= max_tries => {
                return Err(if pod.kind == "ssh" {
                    format!("SSH 連不上：{e}。檢查主機位址、埠、帳號，以及生圖站的公鑰有沒有加進那台主機（雲端設定裡可以複製）")
                } else {
                    format!("SSH 一直連不上：{e}")
                })
            }
            Err(_) => {}
        }
        tries += 1;
        cloud.set(app, "starting", "等 SSH 連得上…");
        tokio::time::sleep(Duration::from_secs(6)).await;
    }
    if pod.kind == "ssh" && !pod.dir.starts_with('/') {
        cloud.set(app, "starting", "檢查主機…");
        cloud.ssh_probe(&mut pod).await?;
        cloud.save_pod(Some(&pod));
        cloud.st().pod = Some(pod.clone());
    }
    let d = pod.dir.clone();
    // 安裝（已經裝過就跳過：接回來的機器、裝過的主機）
    let (step, have) = parse_install(&cloud.ssh(&pod, &install_probe(&d), None, Duration::from_secs(40)).await?);
    let packs = Packs::from_tag(&pod.packs);
    let done_before = INSTALLED.contains(&step.as_str());
    // 裝過、而且這次要的包都裝了才跳過安裝（多勾了包就再跑一次 boot：已經下載的檔會跳過、ComfyUI 不重裝）
    let installed = done_before && packs.within(have);
    cloud.upload_text(&pod, "run.sh", &run_script_for(&d, pod.llm_port, pod.comfy_port, packs)).await?;
    if !installed {
        cloud.upload_text(&pod, "boot.sh", &boot_script_for(&d, packs)).await?;
        // 裝過的機器（舊版裝的沒有 .comfy_ok）：ComfyUI 是好的，補裝功能包時不要整個刪掉重灌
        let keep = if done_before { "touch .comfy_ok && " } else { "" };
        cloud
            .ssh(&pod, &format!("cd '{d}' && {keep}rm -f step && (setsid nohup bash '{d}/boot.sh' < /dev/null > boot.log 2>&1 &) && echo started"), None, Duration::from_secs(60))
            .await?;
    } else {
        // 裝過了：服務還活著就直接用，不然重新啟動
        cloud.open_tunnel(&pod)?;
        tokio::time::sleep(Duration::from_secs(3)).await;
        if cloud.healthy().await != (true, true) {
            cloud.set(app, "starting", "啟動腦袋和 ComfyUI…");
            cloud
                .ssh(&pod, &format!("cd '{d}' && (setsid nohup bash '{d}/run.sh' < /dev/null > run.log 2>&1 &) && echo started"), None, Duration::from_secs(60))
                .await?;
        }
    }
    let t1 = std::time::Instant::now();
    loop {
        if token.is_cancelled() {
            return Err("已取消".into());
        }
        if t1.elapsed() > START_LIMIT {
            return Err("安裝超過 50 分鐘還沒好".into());
        }
        let out = cloud
            .ssh(
                &pod,
                &format!("cat '{d}/step' 2>/dev/null; du -smL '{d}/models' '{d}/cm' '{d}/hf' 2>/dev/null | awk '{{s+=$1}} END {{print s+0}}'; tail -c 400 '{d}/boot.log' 2>/dev/null"),
                None,
                Duration::from_secs(40),
            )
            .await
            .unwrap_or_default();
        let mut lines = out.lines();
        let step = lines.next().unwrap_or("").trim().to_string();
        let mb: f64 = lines.next().and_then(|l| l.trim().parse().ok()).unwrap_or(0.0);
        if step == "error" {
            let tail: String = lines.collect::<Vec<_>>().join("\n");
            return Err(format!("雲端安裝失敗：{}", tail.chars().rev().take(300).collect::<String>().chars().rev().collect::<String>()));
        }
        if step == "ready" {
            break;
        }
        cloud.set(app, "starting", format!("{}…（模型已下載 {:.1} / 約 {:.0} GB）", step_zh(&step), mb / 1024.0, packs.download_gb()));
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
    // 通道＋等兩邊載入完成
    if cloud.tunnel_dead() {
        cloud.open_tunnel(&pod)?;
    }
    let t2 = std::time::Instant::now();
    loop {
        if token.is_cancelled() {
            return Err("已取消".into());
        }
        let (llm, comfy) = cloud.healthy().await;
        if llm && comfy {
            break;
        }
        if t2.elapsed() > Duration::from_secs(10 * 60) {
            return Err(format!("雲端的{}一直沒有回應", if llm { " ComfyUI" } else { "腦袋" }));
        }
        cloud.set(app, "starting", format!("載入模型中…（腦袋{}、ComfyUI{}）", if llm { "好了" } else { "載入中" }, if comfy { "好了" } else { "啟動中" }));
        // 通道掉了就重開
        if cloud.tunnel_dead() {
            cloud.open_tunnel(&pod)?;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    let idle = {
        let mut s = cloud.st();
        s.loras.clear();
        s.inputs.clear();
        s.idle_min
    };
    cloud.touch();
    let price = if pod.price > 0.0 { format!("每小時 {:.2} 美元；", pod.price) } else { String::new() };
    let tail = if cloud.idle_applies(Some(&pod)) {
        format!("閒置 {idle} 分鐘自動關機")
    } else {
        "主機的開關機和計費在雲端服務的網站".to_string()
    };
    let gpu = if pod.gpu.is_empty() { "雲端".to_string() } else { pod.gpu.clone() };
    cloud.set(app, "ready", format!("雲端好了（{gpu}，{price}{tail}）"));
    Ok(())
}

/// 關掉：RunPod 刪機（連硬碟一起，不再計費）；自己的主機停服務（可選送關機指令）；網址模式只是斷線
async fn terminate(app: &Shared) -> Result<(), String> {
    let cloud = &app.agent.cloud;
    cloud.close_tunnel();
    let pod = cloud.st().pod.clone();
    let mut done = "已斷線".to_string();
    if let Some(p) = pod {
        if p.kind == "ssh" {
            cloud.set(app, "stopping", "停止雲端的腦袋和 ComfyUI…");
            done = "已停止雲端的腦袋和 ComfyUI 並斷線。主機本身還開著：要停止計費，請到雲端服務的網站關機".into();
            if p.dir.starts_with('/') {
                // [b]：pkill 比對的是整行指令，這樣才不會連這個 ssh 指令自己（字面上有 boot.sh）一起砍掉
                let stop = format!("A='{}'\n{STOP}\npkill -f \"$A/[b]oot.sh\" || true", p.dir);
                if let Err(e) = cloud.ssh(&p, &stop, None, Duration::from_secs(40)).await {
                    done = format!("已斷線，但停止雲端服務失敗（{e}）。主機本身還開著：要停止計費，請到雲端服務的網站關機");
                }
            }
            if truthy(cloud.cfg().get("ssh_shutdown")) {
                // 主機關機時連線會被切斷，回傳失敗是正常的
                let _ = cloud.ssh(&p, "sudo -n shutdown -h now 2>/dev/null || shutdown -h now", None, Duration::from_secs(20)).await;
                done = "已停止服務並送出關機指令。有些雲端關機後還會收硬碟費，請到網站確認".into();
            }
        } else {
            cloud.set(app, "stopping", "關機中…");
            match cloud.api(reqwest::Method::DELETE, &format!("/pods/{}", p.id), None).await {
                Ok(_) => {}
                // 已經不在了（自己在網站上刪掉）也算關好
                Err(e) if e.contains(" 404") => {}
                Err(e) => {
                    cloud.set(app, "error", format!("關機失敗：{e}。請到 RunPod 網站確認機器 {} 已刪除", p.id));
                    return Err(e);
                }
            }
            let _ = std::fs::remove_file(cloud.dir.join(format!("known_hosts_{}", p.id)));
            done = "已關機刪除，不再計費".into();
        }
    }
    cloud.save_pod(None);
    {
        let mut s = cloud.st();
        s.pod = None;
        s.loras.clear();
        s.inputs.clear();
    }
    cloud.set(app, "off", done);
    Ok(())
}

pub async fn status(State(app): State<Shared>) -> ApiResult {
    // RunPod 模式：顯卡目錄太舊就在背景重抓（價格、有沒有貨、新卡），這次先回手上的
    if app.agent.cloud.provider() == "runpod" {
        let a = app.clone();
        tokio::spawn(async move {
            let _ = a.agent.cloud.refresh_gpus(false).await;
        });
    }
    ok(app.agent.cloud.status())
}

/// 馬上重抓 RunPod 顯卡目錄（網頁的 ↻）：回 {ok, gpus_at}，清單照舊看 /api/agent/cloud
pub async fn gpus(State(app): State<Shared>) -> ApiResult {
    app.agent.cloud.refresh_gpus(true).await.map_err(bad)?;
    ok(json!({"ok": true, "gpus_at": app.agent.cloud.catalog().at, "count": app.agent.cloud.choices().len()}))
}

/// 存雲端設定：{provider, runpod_key?, ssh_host, ssh_port, ssh_user, ssh_dir, ssh_key, ssh_price, ssh_shutdown,
///              comfy_url, comfy_key?, llm_url, llm_key?, llm_model}。金鑰欄位沒帶＝不動，空字串＝清掉
pub async fn config(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let cloud = &app.agent.cloud;
    {
        let s = cloud.st();
        if s.pod.is_some() || matches!(s.phase, "starting" | "ready" | "stopping") {
            return Err(bad("雲端開著的時候不能改設定：先按「關機」或「停止／斷線」"));
        }
    }
    let mut m = cloud.cfg();
    let s = |k: &str| b.get(k).map(tx).unwrap_or_default().trim().to_string();
    if let Some(p) = b.get("provider").and_then(Value::as_str) {
        if !PROVIDERS.contains(&p) {
            return Err(bad("沒有這種連法"));
        }
        m.insert("provider".into(), json!(p));
    }
    if b.get("ssh_host").is_some() {
        let host = s("ssh_host");
        if !host.is_empty() && !safe_word(&host, ".-_:") {
            return Err(bad("主機位址只能有英數字、點、減號（例如 123.45.67.89 或 ssh.example.com；不要加 ssh 或帳號）"));
        }
        m.insert("ssh_host".into(), json!(host));
    }
    if b.get("ssh_user").is_some() {
        let user = s("ssh_user");
        if !user.is_empty() && !safe_word(&user, "._-") {
            return Err(bad("帳號只能有英數字、點、底線、減號"));
        }
        m.insert("ssh_user".into(), json!(user));
    }
    if let Some(v) = b.get("ssh_port") {
        let port = crate::py::int(v).filter(|p| (1..=65535).contains(p)).ok_or_else(|| bad("埠要是 1～65535"))?;
        m.insert("ssh_port".into(), json!(port));
    }
    if b.get("ssh_dir").is_some() {
        let dir = s("ssh_dir");
        if !dir.is_empty() && (!(dir.starts_with('/') || dir.starts_with("~/")) || !safe_word(&dir, "/._-~")) {
            return Err(bad("資料夾要寫完整路徑（/ 或 ~/ 開頭），只能有英數字、/、點、底線、減號；不填就自動決定"));
        }
        m.insert("ssh_dir".into(), json!(dir));
    }
    if b.get("ssh_key").is_some() {
        let key = s("ssh_key").trim_matches('"').to_string();
        if !key.is_empty() && !Path::new(&key).is_file() {
            return Err(bad(format!("找不到私鑰檔：{key}")));
        }
        m.insert("ssh_key".into(), json!(key));
    }
    if let Some(v) = b.get("ssh_price") {
        let p = crate::py::float(v).unwrap_or(0.0);
        if !(0.0..=100.0).contains(&p) {
            return Err(bad("每小時價格要在 0～100 美元之間（不知道就填 0）"));
        }
        m.insert("ssh_price".into(), json!(p));
    }
    if let Some(v) = b.get("ssh_shutdown") {
        m.insert("ssh_shutdown".into(), json!(truthy(Some(v))));
    }
    for k in ["comfy_url", "llm_url"] {
        if b.get(k).is_some() {
            let u = s(k).trim_end_matches('/').to_string();
            if !u.is_empty() && !(u.starts_with("http://") || u.starts_with("https://")) {
                return Err(bad("網址要用 http:// 或 https:// 開頭"));
            }
            m.insert(k.into(), json!(if k == "llm_url" { norm_url(&u) } else { u }));
        }
    }
    if let Some(v) = b.get("packs") {
        m.insert("packs".into(), Packs::from_value(Some(v)).to_value());
    }
    if b.get("llm_model").is_some() {
        m.insert("llm_model".into(), json!(s("llm_model").chars().take(200).collect::<String>()));
    }
    for k in SECRETS {
        if b.get(k).is_some() {
            let v = s(k);
            if v.chars().any(char::is_whitespace) {
                return Err(bad("金鑰裡不能有空白：重新複製一次"));
            }
            if v.is_empty() {
                m.remove(k);
            } else {
                m.insert(k.into(), json!(v));
            }
        }
    }
    let disk = sealed(&m).map_err(bad)?;
    std::fs::create_dir_all(&cloud.dir).map_err(|e| bad(format!("建立 {} 失敗：{e}", cloud.dir.display())))?;
    crate::director::write_atomic(&cloud.dir.join("cloud.json"), &Value::Object(disk)).map_err(|e| bad(format!("存雲端設定失敗：{e}")))?;
    *cloud.cfg.lock().unwrap_or_else(|e| e.into_inner()) = m;
    if cloud.provider() == "ssh" {
        // 公鑰要先有，網頁才能讓使用者複製去貼
        cloud.ensure_key().await.map_err(bad)?;
    }
    let st = cloud.status();
    app.hub.send(json!({"type": "agent_cloud", "status": st}));
    ok(st)
}

/// 測試目前的設定（不開機、不花錢）：RunPod 看金鑰能不能用；自己的主機連進去看顯卡和硬碟；網址模式看兩邊有沒有回應
pub async fn test(State(app): State<Shared>) -> ApiResult {
    let cloud = &app.agent.cloud;
    cloud.missing().map_err(bad)?;
    let msg = match cloud.provider() {
        "url" => cloud.check_url().await.map_err(bad)?,
        "ssh" => {
            cloud.ensure_key().await.map_err(bad)?;
            let mut pod = cloud.ssh_pod();
            cloud
                .ssh(&pod, "echo ok", None, Duration::from_secs(30))
                .await
                .map_err(|e| bad(format!("SSH 連不上：{e}。檢查主機位址、埠、帳號，以及生圖站的公鑰有沒有加進那台主機")))?;
            let info = cloud.ssh_probe(&mut pod).await.map_err(bad)?;
            format!("連上了：{info}")
        }
        _ => {
            cloud.api(reqwest::Method::GET, "/pods", None).await.map_err(bad)?;
            "RunPod 金鑰可以用".to_string()
        }
    };
    ok(json!({"ok": true, "msg": msg}))
}

/// {confirm: true, gpu?: GpuChoice 的 key}：RunPod 要網頁已經讓使用者看過那張卡的每小時價格並按了確認
pub async fn start(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let cloud = &app.agent.cloud;
    let renting = cloud.st().pod.is_none() && cloud.provider() == "runpod";
    let key = match b.get("gpu").and_then(Value::as_str) {
        Some(k) => k.to_string(),
        None => cloud.st().gpu.clone(),
    };
    let opt = cloud.choice(&key).ok_or_else(|| bad("沒有這張卡的選項：按顯卡清單旁的 ↻ 再選一次"))?;
    let packs = cloud.packs();
    if renting && packs.agent && packs.h3 && opt.vram < 80 {
        return Err(bad(format!("{} 放不下助手＋H3：同時要用請選 RTX PRO 6000 96GB，或在雲端設定只勾一個", opt.name)));
    }
    if renting && !truthy(b.get("confirm")) {
        return Err(bad(format!("開機會開始計費（{} 每小時約 {:.2} 美元），請在畫面上確認", opt.name, opt.price_hi)));
    }
    // 接回上次的機器：RunPod 的要金鑰，自己的主機不用看設定
    let kind = cloud.st().pod.as_ref().map(|p| p.kind.clone());
    match kind.as_deref() {
        Some("ssh") => {}
        Some(_) if cloud.api_key().is_none() => return Err(bad("找不到 RunPod 金鑰：按「雲端設定」貼上")),
        Some(_) => {}
        None => cloud.missing().map_err(bad)?,
    }
    let token = {
        {
            let mut s = cloud.st();
            if matches!(s.phase, "starting" | "ready" | "stopping") {
                return Err(bad("雲端已經在開或開好了"));
            }
            // 上次留下的機器（接回來）就用它原本那張卡
            if s.pod.is_none() {
                s.gpu = opt.key.clone();
            }
        }
        let mut j = cloud.job.lock().unwrap_or_else(|e| e.into_inner());
        let t = CancellationToken::new();
        *j = Some(t.clone());
        t
    };
    let a = app.clone();
    tokio::spawn(async move {
        let r = start_run(&a, &token).await;
        *a.agent.cloud.job.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if let Err(e) = r {
            let kind = a.agent.cloud.st().pod.as_ref().map(|p| p.kind.clone());
            // RunPod 開不起來（或開到一半按了關機）就刪機，免得一直計費。按關機時那邊也會刪，重複刪會拿到 404，當成已經刪好
            // 自己的主機：只斷線（裝到一半的檔案留著，下次接著裝）
            let tail = match kind.as_deref() {
                Some("runpod") => {
                    if terminate(&a).await.is_ok() {
                        "（機器已刪除，不會再計費）"
                    } else {
                        "（刪機也失敗了：請到 RunPod 網站確認）"
                    }
                }
                Some(_) => {
                    let c = &a.agent.cloud;
                    c.close_tunnel();
                    c.save_pod(None);
                    c.st().pod = None;
                    "（主機還開著，計費照那家雲端的規定）"
                }
                None => "",
            };
            if !token.is_cancelled() {
                eprintln!("雲端開機失敗：{e}");
                let what = if kind.is_some() || a.agent.cloud.provider() == "runpod" { "開機失敗" } else { "連線失敗" };
                a.agent.cloud.set(&a, "error", format!("{what}：{e}{tail}"));
            }
        }
    });
    ok(json!({"ok": true}))
}

/// 關機：中止開機中的工作、關通道、刪機（自己的主機是停服務，網址模式是斷線）
pub async fn stop(State(app): State<Shared>) -> ApiResult {
    if let Some(t) = app.agent.cloud.job.lock().unwrap_or_else(|e| e.into_inner()).take() {
        t.cancel();
    }
    terminate(&app).await.map_err(bad)?;
    ok(app.agent.cloud.status())
}

/// {minutes}：閒置幾分鐘自動關機（5～240）
pub async fn idle(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let m = b.get("minutes").and_then(crate::py::int).ok_or_else(|| bad("minutes 要是整數"))?.clamp(5, 240);
    app.agent.cloud.st().idle_min = m;
    app.agent.cloud.touch();
    ok(app.agent.cloud.status())
}

/// 背景：閒置太久自動關（助手、H3 影片台都沒在跑、超過設定的分鐘數；只有 RunPod 和勾了「順便關主機」的自己主機）
pub fn spawn_idle_watch(app: &Shared) {
    let a = app.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let (ready, last, idle, pod) = {
                let s = a.agent.cloud.st();
                (s.phase == "ready", s.last_used, s.idle_min, s.pod.clone())
            };
            if a.agent.any_running() || a.h3.any_running() {
                a.agent.cloud.touch();
                continue;
            }
            if ready && a.agent.cloud.idle_applies(pod.as_ref()) && now() - last > idle as f64 * 60.0 {
                eprintln!("雲端閒置 {idle} 分鐘，自動關機");
                if terminate(&a).await.is_ok() {
                    let what = if pod.is_some_and(|p| p.kind == "ssh") { "已停止服務並送出關機指令" } else { "已自動關機刪除" };
                    a.agent.cloud.set(&a, "off", format!("閒置 {idle} 分鐘，{what}"));
                }
            }
        }
    });
}

// ================================================================ 雲端畫圖
/// LoRA 傳到雲端（第一次用才傳），回傳雲端的檔名（攤平、只留安全字元）
async fn ensure_lora(app: &Shared, pod: &Pod, name: &str) -> Result<String, String> {
    if let Some(r) = app.agent.cloud.st().loras.get(name).cloned() {
        return Ok(r);
    }
    let local = families::resolve(&app.paths.lora_roots(), name).ok_or_else(|| format!("找不到 LoRA 檔 {name}"))?;
    let remote: String = name
        .replace(['\\', '/'], "__")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' })
        .collect();
    app.agent.cloud.set(app, "ready", format!("上傳 LoRA {}…", super::basename(name)));
    let d = format!("{}/ComfyUI/models/loras", pod.dir);
    app.agent
        .cloud
        .ssh(pod, &format!("cat > '{d}/{remote}.part' && mv '{d}/{remote}.part' '{d}/{remote}'"), Some(&local), Duration::from_secs(30 * 60))
        .await?;
    app.agent.cloud.st().loras.insert(name.to_string(), remote.clone());
    app.agent.cloud.set(app, "ready", "雲端好了");
    Ok(remote)
}

/// 「用 RefMod 鎖臉」：角色的 RefMod 檔傳到雲端 models/refmods/<名字>.safetensors。
/// 每次都傳（一個一兩 MB），角色重做過也不會用到舊檔；網址模式沒辦法傳
pub async fn upload_refmods(app: &Shared, mods: &[(String, PathBuf)], on_status: &(dyn Fn(String) + Send + Sync)) -> Result<(), String> {
    if mods.is_empty() {
        return Ok(());
    }
    if remote_options(app, "MiniMaxH3RefModApply", "curve_direction").await.is_empty() {
        return Err("雲端沒有 RefMod 節點（開機時沒裝起來）：雲端關機再開一次，或把拍攝單的「用 RefMod 鎖臉」取消勾選".into());
    }
    let pod = app.agent.cloud.st().pod.clone().ok_or("網址模式的雲端不能上傳 RefMod：把拍攝單的「用 RefMod 鎖臉」取消勾選")?;
    for (name, local) in mods {
        if !crate::refmod::valid_name(name) {
            return Err(format!("RefMod 名字不對：{name}"));
        }
        let (sub, file) = name.rsplit_once('/').unwrap_or(("", name.as_str()));
        let d = format!("{}/ComfyUI/models/refmods/{sub}", pod.dir);
        on_status(format!("上傳 RefMod {file}…"));
        app.agent
            .cloud
            .ssh(&pod, &format!("mkdir -p '{d}' && cat > '{d}/{file}.part' && mv '{d}/{file}.part' '{d}/{file}.safetensors'"), Some(local), Duration::from_secs(10 * 60))
            .await?;
    }
    Ok(())
}

/// 雲端 ComfyUI 某個節點欄位的選項（例如 UNETLoader 的 unet_name）。雲端沒開、讀不到就是空清單
pub async fn remote_options(app: &Shared, node: &str, field: &str) -> Vec<String> {
    let cloud = &app.agent.cloud;
    let Some(ep) = cloud.endpoints() else {
        return vec![];
    };
    let r = cloud
        .req(reqwest::Method::GET, format!("{}/object_info/{node}", ep.comfy), ep.comfy_key.as_deref())
        .timeout(Duration::from_secs(20))
        .send()
        .await;
    let v: Value = match r {
        Ok(r) => r.json().await.unwrap_or(Value::Null),
        Err(_) => Value::Null,
    };
    combo(&v, node, field)
}

/// 雲端 ComfyUI 有沒有這個節點（INT 欄位的節點沒有選項清單，不能用 remote_options 判斷）
pub async fn remote_has_node(app: &Shared, node: &str) -> bool {
    let cloud = &app.agent.cloud;
    let Some(ep) = cloud.endpoints() else {
        return false;
    };
    let r = cloud
        .req(reqwest::Method::GET, format!("{}/object_info/{node}", ep.comfy), ep.comfy_key.as_deref())
        .timeout(Duration::from_secs(20))
        .send()
        .await;
    match r {
        Ok(r) => r.json::<Value>().await.is_ok_and(|v| v.get(node).is_some()),
        Err(_) => false,
    }
}

/// 網址模式：LoRA 沒辦法上傳，要遠端已經有（照原名、斜線方向、只看檔名依序找）
async fn remote_lora(app: &Shared, ep: &Endpoints, name: &str) -> Result<String, String> {
    if let Some(r) = app.agent.cloud.st().loras.get(name).cloned() {
        return Ok(r);
    }
    let v: Value = app
        .agent
        .cloud
        .req(reqwest::Method::GET, format!("{}/object_info/LoraLoaderModelOnly", ep.comfy), ep.comfy_key.as_deref())
        .send()
        .await
        .map_err(|e| format!("讀遠端 LoRA 清單失敗：{e}"))?
        .json()
        .await
        .unwrap_or(Value::Null);
    let list = combo(&v, "LoraLoaderModelOnly", "lora_name");
    let base = super::basename(name);
    let found = [name.to_string(), name.replace('\\', "/")]
        .into_iter()
        .find(|c| list.contains(c))
        .or_else(|| list.iter().find(|x| super::basename(x) == base).cloned())
        .ok_or_else(|| format!("遠端 ComfyUI 沒有 LoRA {base}：網址模式不能自動上傳，請把它放進遠端的 models\\loras"))?;
    app.agent.cloud.st().loras.insert(name.to_string(), found.clone());
    Ok(found)
}

/// 雲端生圖：模型換成雲端有的 MOSS-Image、LoRA 先上傳；圖下載回 output\AnimaWeb\cloud\ 並登記進圖庫（job＝cloud-<編號>）
pub async fn render(app: &Shared, mut q: Map<String, Value>, count: i64, token: &CancellationToken) -> Result<(Vec<i64>, Vec<String>), String> {
    let ep = app.agent.cloud.endpoints().ok_or("雲端斷線了：到「雲端」那裡看狀態")?;
    let pod = app.agent.cloud.st().pod.clone();
    if families::family_of(&app.paths.unet_roots(), &tx(&q["model"])) != families::MOSS {
        return Err("雲端目前只裝 MOSS-Image：生成頁請換成 MOSS 模型再按「用生成頁目前的設定」".into());
    }
    q.insert("model".into(), json!(CLOUD_UNET));
    q.insert("turbo_lora".into(), json!(false));
    q.insert("family".into(), json!(families::MOSS));
    q.insert("cloud".into(), json!(true));
    let mut map: HashMap<String, String> = HashMap::new();
    for l in q.get("loras").and_then(Value::as_array).cloned().unwrap_or_default() {
        let n = tx(&l["name"]);
        if !n.is_empty() && !map.contains_key(&n) {
            let r = match &pod {
                Some(p) => ensure_lora(app, p, &n).await?,
                None => remote_lora(app, &ep, &n).await?,
            };
            map.insert(n.clone(), r);
        }
    }
    let ck = ep.comfy_key.as_deref();
    let cloud = &app.agent.cloud;
    let p = Value::Object(q);
    let mut jobs = vec![];
    let mut errors = vec![];
    for i in 0..count {
        let (pp, input) = (p.clone(), app.paths.input.clone());
        let (mut nodes, mut info) = blocking(move || graphs::build(&pp, &input, i)).await.map_err(|e| e.0)?;
        for n in nodes.values_mut() {
            if n["class_type"] == "LoraLoaderModelOnly" {
                if let Some(r) = map.get(&tx(&n["inputs"]["lora_name"])) {
                    n["inputs"]["lora_name"] = json!(r);
                }
            }
        }
        info["idea"] = p.get("idea").cloned().unwrap_or(json!(""));
        let mut params = p.clone();
        params["seed"] = info["seed"].clone();
        params["prompt_resolved"] = info["prompt"].clone();
        crate::diagnostics::remember_secret(&app.diagnostics,ck);
        crate::diagnostics::snapshot("cloud_workflow_api", &Value::Object(nodes.clone()));
        crate::diagnostics::snapshot("cloud_generation_parameters", &params);
        let r = cloud
            .req(reqwest::Method::POST, format!("{}/prompt", ep.comfy), ck)
            .json(&json!({"prompt": Value::Object(nodes), "client_id": "anima-agent"}))
            .send()
            .await
            .map_err(|e| format!("送不到雲端 ComfyUI：{e}"))?;
        let v: Value = r.json().await.unwrap_or(Value::Null);
        crate::diagnostics::event("cloud_prompt_response",v.clone());
        let pid = tx(&v["prompt_id"]);
        if pid.is_empty() {
            errors.push(format!("雲端 ComfyUI 不收：{}", v.to_string().chars().take(300).collect::<String>()));
            continue;
        }
        if let Some(run)=crate::diagnostics::current() {run.store.link_job(&run.id,&format!("cloud-{pid}"));}
        jobs.push((pid, params, info));
    }
    let mut ids = vec![];
    // 停止時連同這一輪還在排隊的一起刪（同一台可能還有 H3 的工作：不清整個佇列）
    let pids: Vec<String> = jobs.iter().map(|j| j.0.clone()).collect();
    for (k, (pid, params, info)) in jobs.into_iter().enumerate() {
        let result = wait_done(cloud, &ep, &pid, &pids[k + 1..], token, &|_| {}).await;
        if let Err(e)=&result {crate::diagnostics::failure(e);}
        let entry=result?;
        app.diagnostics.job_event(&format!("cloud-{pid}"),"history_snapshot",&entry);
        app.diagnostics.job_event(&format!("cloud-{pid}"),if entry["status"]["status_str"]=="success" {"execution_success"} else {"execution_error"},&entry["status"]);
        if entry["status"]["status_str"] != "success" {
            errors.push(format!("雲端生圖失敗：{}", entry["status"]["messages"].to_string().chars().rev().take(300).collect::<String>().chars().rev().collect::<String>()));
            continue;
        }
        let job = format!("cloud-{pid}");
        app.db.add_job(&job, &params, &info).map_err(|e| e.to_string())?;
        // 雲端那台實際畫了多久（history 的 execution_start → execution_success，毫秒）：記成開始時間，圖庫才算得出花費時間
        let stamp = |kind: &str| {
            entry["status"]["messages"].as_array().into_iter().flatten()
                .find(|m| m[0] == kind).and_then(|m| m[1]["timestamp"].as_f64())
        };
        if let (Some(a), Some(b)) = (stamp("execution_start"), stamp("execution_success")) {
            let _ = app.db.run("UPDATE jobs SET started=? WHERE id=?", rusqlite::params![now() - (b - a).max(0.0) / 1000.0, job]);
        }
        for out in entry["outputs"].as_object().into_iter().flat_map(|o| o.values()) {
            for im in out["images"].as_array().into_iter().flatten() {
                if im["type"] != "output" {
                    continue;
                }
                let bytes = cloud
                    .req(reqwest::Method::GET, format!("{}/view", ep.comfy), ck)
                    .query(&[("filename", tx(&im["filename"])), ("subfolder", tx(&im["subfolder"])), ("type", "output".to_string())])
                    .send()
                    .await
                    .map_err(|e| format!("下載雲端的圖失敗：{e}"))?
                    .bytes()
                    .await
                    .map_err(|e| format!("下載雲端的圖失敗：{e}"))?;
                let sub = "AnimaWeb/cloud";
                let dir = app.paths.output.join("AnimaWeb").join("cloud");
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
                let base = super::basename(&tx(&im["filename"]));
                let mut fname = format!("{stamp}_{base}");
                let mut k = 1;
                while dir.join(&fname).exists() {
                    fname = format!("{stamp}_{k}_{base}");
                    k += 1;
                }
                std::fs::write(dir.join(&fname), &bytes).map_err(|e| format!("存圖失敗：{e}"))?;
                let iid = app.db.add_image(&fname, sub, "output", &info, &params, Some(&job)).map_err(|e| e.to_string())?;
                let image = app.db.image(iid).ok().flatten().map(|r| {
                    let mut rows = [r];
                    let _ = app.db.add_gen_seconds(&mut rows);
                    let [r] = rows;
                    Value::Object(r)
                }).unwrap_or(Value::Null);
                app.hub.send(json!({"type": "image", "job": job, "image": image}));
                ids.push(iid);
            }
        }
        let _ = app.db.set_job(&job, "done", None);
    }
    Ok((ids, errors))
}

// ================================================================ 等雲端 ComfyUI 跑完（助手的 render、H3 的 run_graph 共用）
/// 連續讀不到幾次就放棄（每 2 秒一次＝90 秒）
const MAX_POLL_FAILS: u32 = 45;
/// 一個工作最多等多久（秒）：H3 高畫質 15 秒在 A6000 上也遠低於這個
const MAX_WAIT_SECS: u64 = 90 * 60;

/// 要不要放棄等待：一直讀不到、工作不見了、或做太久。放棄＝回錯誤，機器才會照閒置時間自動關（一直等＝一直計費）
fn wait_verdict(fails: u32, lost: bool, elapsed_secs: u64) -> Option<&'static str> {
    if lost {
        Some("雲端 ComfyUI 找不到這個工作了（可能當掉重開）：重新送一次")
    } else if fails >= MAX_POLL_FAILS {
        Some("雲端 ComfyUI 一直沒有回應（可能當掉了）：到「雲端」那裡看狀態，必要時關機再開")
    } else if elapsed_secs > MAX_WAIT_SECS {
        Some("雲端做太久了（超過 90 分鐘）：已經停掉這個工作，到「雲端」那裡看狀態")
    } else {
        None
    }
}

/// /queue 的回應裡有沒有這個工作（跑著或排隊中）
fn in_queue(q: &Value, pid: &str) -> bool {
    ["queue_running", "queue_pending"].iter().any(|k| q[*k].as_array().into_iter().flatten().any(|it| it[1] == pid))
}

/// 讀雲端 ComfyUI 的 JSON；連不上、不是 JSON 都算讀不到
async fn get_json(cloud: &Cloud, ep: &Endpoints, path: &str) -> Option<Value> {
    let r = cloud.req(reqwest::Method::GET, format!("{}{path}", ep.comfy), ep.comfy_key.as_deref()).timeout(Duration::from_secs(30)).send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.json().await.ok()
}

/// 等 pid 跑完，回傳 history 那一筆。
/// 停止：刪掉 also（同一輪還在排隊的）並只中斷 pid：同一台可能還有助手或 H3 的別的工作，不能整台中斷、清佇列。
/// 只有讀得到 ComfyUI 才算「在用」（touch）；一直讀不到、工作不見了、或做太久就放棄
async fn wait_done(cloud: &Cloud, ep: &Endpoints, pid: &str, also: &[String], token: &CancellationToken, on_tick: &(dyn Fn(u64) + Send + Sync)) -> Result<Value, String> {
    let ck = ep.comfy_key.as_deref();
    let interrupt = || cloud.req(reqwest::Method::POST, format!("{}/interrupt", ep.comfy), ck).json(&json!({"prompt_id": pid})).send();
    let t0 = std::time::Instant::now();
    let (mut fails, mut polls) = (0u32, 0u64);
    loop {
        if token.is_cancelled() {
            let del: Vec<&str> = std::iter::once(pid).chain(also.iter().map(String::as_str)).collect();
            let _ = cloud.req(reqwest::Method::POST, format!("{}/queue", ep.comfy), ck).json(&json!({"delete": del})).send().await;
            let _ = interrupt().await;
            return Err("已停止".into());
        }
        let mut lost = false;
        match get_json(cloud, ep, &format!("/history/{pid}")).await {
            Some(h) => {
                fails = 0;
                if h[pid]["status"]["completed"].is_boolean() {
                    cloud.touch();
                    return Ok(h[pid].clone());
                }
                cloud.touch();
                polls += 1;
                // 每 30 秒確認一次工作還在：ComfyUI 重開過的話 history 永遠不會有它
                if polls % 15 == 0 {
                    if let Some(q) = get_json(cloud, ep, "/queue").await {
                        if !in_queue(&q, pid) {
                            // 可能剛好跑完：再看一次 history
                            if let Some(h) = get_json(cloud, ep, &format!("/history/{pid}")).await {
                                if h[pid]["status"]["completed"].is_boolean() {
                                    return Ok(h[pid].clone());
                                }
                            }
                            lost = true;
                        }
                    }
                }
            }
            None => fails += 1,
        }
        if cloud.endpoints().is_none() {
            return Err("雲端斷線了（機器被關掉或通道斷了）：到「雲端」那裡看狀態".into());
        }
        if let Some(e) = wait_verdict(fails, lost, t0.elapsed().as_secs()) {
            let _ = interrupt().await;
            return Err(e.into());
        }
        on_tick(t0.elapsed().as_secs());
        tokio::time::sleep(Duration::from_millis(2000)).await;
    }
}

// ================================================================ H3 影片：素材上傳、送節點圖、收成品
/// 本機的檔用 ComfyUI 的 /upload/image 傳上雲端（fields＝表單的其他欄位：type、subfolder、overwrite）；回傳 ComfyUI 的回應
async fn upload_form(app: &Shared, local: &Path, name: &str, fields: &[(&str, &str)]) -> Result<Value, String> {
    let cloud = &app.agent.cloud;
    let ep = cloud.endpoints().ok_or("雲端斷線了：到「雲端」那裡看狀態")?;
    let bytes = tokio::fs::read(local).await.map_err(|e| format!("讀 {name} 失敗：{e}"))?;
    let boundary = format!("----animaH3{:016x}", now().to_bits());
    let mut body: Vec<u8> = vec![];
    for (k, v) in fields {
        body.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes());
    }
    body.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
    body.extend(&bytes);
    body.extend(format!("\r\n--{boundary}--\r\n").as_bytes());
    let r = cloud
        .req(reqwest::Method::POST, format!("{}/upload/image", ep.comfy), ep.comfy_key.as_deref())
        .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
        .body(body)
        .timeout(Duration::from_secs(900))
        .send()
        .await
        .map_err(|e| format!("{name} 傳不上雲端：{e}"))?;
    if !r.status().is_success() {
        return Err(format!("{name} 傳不上雲端：HTTP {}", r.status()));
    }
    cloud.touch();
    Ok(r.json().await.unwrap_or(Value::Null))
}

/// 本機 input 裡的素材（圖、影片、聲音）傳到雲端 ComfyUI 的 input，同一個檔名。這台已經傳過的就跳過
pub async fn upload_input(app: &Shared, local: &Path) -> Result<String, String> {
    let cloud = &app.agent.cloud;
    let name = local.file_name().and_then(|n| n.to_str()).filter(|n| safe_word(n, "._-")).ok_or("素材檔名只能有英數字、點、底線、減號")?.to_string();
    if cloud.st().inputs.contains(&name) {
        return Ok(name);
    }
    let v = upload_form(app, local, &name, &[("overwrite", "true"), ("type", "input")]).await.map_err(|e| format!("素材{e}"))?;
    let got = tx(&v["name"]);
    if !got.is_empty() && got != name {
        return Err(format!("雲端把素材 {name} 改名成 {got} 了"));
    }
    cloud.st().inputs.insert(name.clone());
    Ok(name)
}

/// 雲端 output 裡的這個檔，這台機器這次開機有沒有（自己生的或傳上去的，記在 inputs，key＝output:<子資料夾>/<檔名>）
pub fn has_output(app: &Shared, subfolder: &str, name: &str) -> bool {
    app.agent.cloud.st().inputs.contains(&format!("output:{subfolder}/{name}"))
}

pub fn invalidate_output(app: &Shared, subfolder: &str, name: &str) {
    app.agent.cloud.st().inputs.remove(&format!("output:{subfolder}/{name}"));
}

pub fn mark_output(app: &Shared, subfolder: &str, name: &str) {
    app.agent.cloud.st().inputs.insert(format!("output:{subfolder}/{name}"));
}

/// 本機的檔傳到雲端 ComfyUI 的 output/<subfolder>/（長片：上一段的 latent，雲端重開過就沒了）
pub async fn upload_output(app: &Shared, local: &Path, subfolder: &str) -> Result<(), String> {
    let name = local.file_name().and_then(|n| n.to_str()).filter(|n| safe_word(n, "._-")).ok_or("檔名只能有英數字、點、底線、減號")?.to_string();
    let v = upload_form(app, local, &name, &[("overwrite", "true"), ("type", "output"), ("subfolder", subfolder)]).await?;
    let got = tx(&v["name"]);
    if !got.is_empty() && got != name {
        return Err(format!("雲端把 {name} 改名成 {got} 了"));
    }
    mark_output(app, subfolder, &name);
    Ok(())
}

/// 雲端 ComfyUI output 裡的一個檔下載到 dest（先寫 .part 再改名，下載到一半斷掉不會留下壞檔）
pub async fn fetch_output(app: &Shared, subfolder: &str, filename: &str, dest: &Path) -> Result<(), String> {
    let cloud = &app.agent.cloud;
    let ep = cloud.endpoints().ok_or("雲端斷線了：到「雲端」那裡看狀態")?;
    let bytes = cloud
        .req(reqwest::Method::GET, format!("{}/view", ep.comfy), ep.comfy_key.as_deref())
        .query(&[("filename", filename), ("subfolder", subfolder), ("type", "output")])
        .timeout(Duration::from_secs(900))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("下載雲端的 {filename} 失敗：{e}"))?
        .bytes()
        .await
        .map_err(|e| format!("下載雲端的 {filename} 失敗：{e}"))?;
    if let Some(d) = dest.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("建立 {} 失敗：{e}", d.display()))?;
    }
    let part = dest.with_extension("part");
    std::fs::write(&part, &bytes).map_err(|e| format!("存 {filename} 失敗：{e}"))?;
    std::fs::rename(&part, dest).map_err(|e| format!("存 {filename} 失敗：{e}"))?;
    cloud.touch();
    Ok(())
}

/// 長片（H3 接續）用的社群節點解開到雲端 ComfyUI 的 custom_nodes（壓縮檔已經用 ssh 傳到 <dir>/h3_infinite.tgz）。
/// 先解到 .new 再換掉舊的；--no-same-owner：root 解別人打包的 tar 會因為擁有者對不上報錯
fn chain_nodes_cmd(dir: &str) -> String {
    format!(
        "cd '{dir}' && N=ComfyUI/custom_nodes/HerrgottsH3Infinite && rm -rf \"$N.new\" && mkdir -p \"$N.new\" && \
         tar xzf '{dir}/h3_infinite.tgz' --no-same-owner -C \"$N.new\" --strip-components=1 && rm -rf \"$N\" && mv \"$N.new\" \"$N\" && echo ok"
    )
}

/// 雲端 ComfyUI 有沒有長片的節點
async fn chain_nodes_ready(cloud: &Cloud) -> bool {
    let Some(ep) = cloud.endpoints() else {
        return false;
    };
    get_json(cloud, &ep, "/object_info/H3ContinuousStartV14").await.is_some_and(|v| v.get("H3ContinuousStartV14").is_some())
}

/// 長片要用的社群節點（resources\extras\h3_infinite.tar.gz，見同資料夾的 h3_infinite.txt）：雲端沒有就用 ssh 裝上去、重開 ComfyUI。
/// 網址模式（沒有 ssh）裝不了
pub async fn ensure_chain_nodes(app: &Shared, on_status: &(dyn Fn(String) + Send + Sync)) -> Result<(), String> {
    let cloud = &app.agent.cloud;
    cloud.endpoints().ok_or("雲端斷線了：到「雲端」那裡看狀態")?;
    if chain_nodes_ready(cloud).await {
        return Ok(());
    }
    let pod = cloud.st().pod.clone().ok_or("長片目前只能用雲端（要能 ssh 的雲端主機）")?;
    let tgz = app.paths.res.join("extras").join("h3_infinite.tar.gz");
    if !tgz.is_file() {
        return Err(format!("找不到 {}：重新安裝生圖站", tgz.display()));
    }
    let d = pod.dir.clone();
    on_status("第一次拍長片：把接續用的節點裝到雲端…".into());
    cloud.ssh(&pod, &format!("cat > '{d}/h3_infinite.tgz'"), Some(&tgz), Duration::from_secs(120)).await?;
    cloud.ssh(&pod, &chain_nodes_cmd(&d), None, Duration::from_secs(60)).await?;
    on_status("重新啟動雲端 ComfyUI（約 1 分鐘）…".into());
    cloud
        .ssh(&pod, &format!("cd '{d}' && (setsid nohup bash '{d}/run.sh' < /dev/null > run.log 2>&1 &) && echo started"), None, Duration::from_secs(60))
        .await?;
    // run.sh 先停舊的再開新的：等一下再問，免得問到還沒停掉的舊 ComfyUI
    tokio::time::sleep(Duration::from_secs(8)).await;
    for _ in 0..60 {
        cloud.touch();
        if cloud.healthy().await.1 && chain_nodes_ready(cloud).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    Err("雲端 ComfyUI 重開後沒有長片的節點：看雲端的 comfy.log，或關機再開一次".into())
}

/// ComfyUI history 裡的錯誤訊息（execution_error 那一筆）
fn exec_error(entry: &Value) -> String {
    let msgs = entry["status"]["messages"].as_array().cloned().unwrap_or_default();
    if let Some(m) = msgs.iter().find(|m| m[0] == "execution_error") {
        let d = &m[1];
        let node = tx(&d["node_type"]);
        let msg = tx(&d["exception_message"]);
        return format!("{node}：{}", msg.trim());
    }
    entry["status"]["messages"].to_string().chars().rev().take(300).collect::<String>().chars().rev().collect()
}

/// 送一張節點圖到雲端 ComfyUI、等它跑完，回傳 (prompt_id, history 那一筆)；沒成功就回錯誤（不下載、不登記圖庫）
pub async fn run_raw(
    app: &Shared,
    nodes: Map<String, Value>,
    token: &CancellationToken,
    on_status: &(dyn Fn(String) + Send + Sync),
) -> Result<(String, Value), String> {
    let cloud = &app.agent.cloud;
    let ep = cloud.endpoints().ok_or("雲端斷線了：到「雲端」那裡看狀態")?;
    let ck = ep.comfy_key.as_deref();
    crate::diagnostics::remember_secret(&app.diagnostics, ck);
    crate::diagnostics::snapshot("cloud_workflow_api", &Value::Object(nodes.clone()));
    let r = cloud
        .req(reqwest::Method::POST, format!("{}/prompt", ep.comfy), ck)
        .json(&json!({"prompt": Value::Object(nodes), "client_id": "anima-h3"}))
        .send()
        .await
        .map_err(|e| format!("送不到雲端 ComfyUI：{e}"))?;
    let v: Value = r.json().await.unwrap_or(Value::Null);
    crate::diagnostics::event("cloud_prompt_response", v.clone());
    let pid = tx(&v["prompt_id"]);
    if !pid.is_empty() {if let Some(run)=crate::diagnostics::current() {run.store.link_job(&run.id,&format!("cloud-{pid}"));}}
    if pid.is_empty() {
        return Err(format!("雲端 ComfyUI 不收：{}", v.to_string().chars().take(600).collect::<String>()));
    }
    on_status("排隊中…".into());
    let said = std::sync::atomic::AtomicU64::new(u64::MAX);
    let tick = |secs: u64| {
        if said.swap(secs / 10, std::sync::atomic::Ordering::Relaxed) != secs / 10 {
            on_status(format!("雲端生成中…已 {secs} 秒"));
        }
    };
    let result = wait_done(cloud, &ep, &pid, &[], token, &tick).await;
    if let Err(e)=&result {
        crate::diagnostics::failure(e);
        app.diagnostics.job_event(&format!("cloud-{pid}"),"execution_interrupted",&json!({"reason":e,"outcome":"unknown"}));
    }
    let entry=result?;
    app.diagnostics.job_event(&format!("cloud-{pid}"),"history_snapshot",&entry);
    app.diagnostics.job_event(&format!("cloud-{pid}"),if entry["status"]["status_str"]=="success" {"execution_success"} else {"execution_error"},&entry["status"]);
    if entry["status"]["status_str"] != "success" {
        return Err(format!("雲端生成失敗：{}", exec_error(&entry)));
    }
    Ok((pid, entry))
}

/// 送一張已經組好的節點圖到雲端 ComfyUI，等它跑完，把成品下載回本機並登記進圖庫（job＝cloud-<編號>）。
/// AnimaWeb/_poster 開頭的（影片封面、最後一格）照原檔名存到本機 output\AnimaWeb\_poster\、不進圖庫（圖庫用 params.poster 找封面）。
/// on_status 收進度文字。回傳 (job, 圖庫編號)
pub async fn run_graph(
    app: &Shared,
    nodes: Map<String, Value>,
    params: &Value,
    info: &Value,
    token: &CancellationToken,
    on_status: &(dyn Fn(String) + Send + Sync),
) -> Result<(String, Vec<i64>), String> {
    let cloud = &app.agent.cloud;
    let ep = cloud.endpoints().ok_or("雲端斷線了：到「雲端」那裡看狀態")?;
    let ck = ep.comfy_key.as_deref();
    let (pid, entry) = run_raw(app, nodes, token, on_status).await?;
    on_status("下載成品…".into());
    let job = format!("cloud-{pid}");
    app.db.add_job(&job, params, info).map_err(|e| e.to_string())?;
    let stamp = |kind: &str| {
        entry["status"]["messages"].as_array().into_iter().flatten().find(|m| m[0] == kind).and_then(|m| m[1]["timestamp"].as_f64())
    };
    if let (Some(a), Some(b)) = (stamp("execution_start"), stamp("execution_success")) {
        let _ = app.db.run("UPDATE jobs SET started=? WHERE id=?", rusqlite::params![now() - (b - a).max(0.0) / 1000.0, job]);
    }
    let mut ids = vec![];
    for out in entry["outputs"].as_object().into_iter().flat_map(|o| o.values()) {
        for im in ["images", "gifs"].into_iter().flat_map(|k| out[k].as_array().into_iter().flatten()) {
            if im["type"] != "output" {
                continue;
            }
            let sub = tx(&im["subfolder"]).replace('\\', "/");
            let base = super::basename(&tx(&im["filename"]));
            let bytes = cloud
                .req(reqwest::Method::GET, format!("{}/view", ep.comfy), ck)
                .query(&[("filename", tx(&im["filename"])), ("subfolder", tx(&im["subfolder"])), ("type", "output".to_string())])
                .timeout(Duration::from_secs(900))
                .send()
                .await
                .map_err(|e| format!("下載雲端的成品失敗：{e}"))?
                .bytes()
                .await
                .map_err(|e| format!("下載雲端的成品失敗：{e}"))?;
            ids.extend(keep_output(app, &sub, &base, &bytes, params, info, &job)?);
        }
    }
    let _ = app.db.set_job(&job, "done", None);
    Ok((job, ids))
}

/// 雲端成品存回本機：封面（AnimaWeb/_poster）放回原位不進圖庫；其他存 AnimaWeb/cloud、進圖庫、通知網頁。回圖庫編號
pub(super) fn keep_output(app: &Shared, sub: &str, base: &str, bytes: &[u8], params: &Value, info: &Value, job: &str) -> Result<Option<i64>, String> {
    if sub.starts_with("AnimaWeb/_poster") {
        let dir = app.paths.output.join("AnimaWeb").join("_poster");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(base), bytes).map_err(|e| format!("存封面失敗：{e}"))?;
        return Ok(None);
    }
    let dir = app.paths.output.join("AnimaWeb").join("cloud");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let t = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let mut fname = format!("{t}_{base}");
    let mut k = 1;
    while dir.join(&fname).exists() {
        fname = format!("{t}_{k}_{base}");
        k += 1;
    }
    std::fs::write(dir.join(&fname), bytes).map_err(|e| format!("存成品失敗：{e}"))?;
    let iid = app.db.add_image(&fname, "AnimaWeb/cloud", "output", info, params, Some(job)).map_err(|e| e.to_string())?;
    let image = app.db.image(iid).ok().flatten().map(|r| {
        let mut rows = [r];
        let _ = app.db.add_gen_seconds(&mut rows);
        let [r] = rows;
        Value::Object(r)
    }).unwrap_or(Value::Null);
    app.hub.send(json!({"type": "image", "job": job, "image": image}));
    Ok(Some(iid))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 長片的社群節點：用生圖站帶的壓縮檔解開（不從 GitHub 抓），解壓不保留原擁有者（root 解 tar 會報錯）
    #[test]
    fn chain_nodes_cmd_safe() {
        let c = chain_nodes_cmd("/workspace/agent");
        assert!(c.contains("--no-same-owner") && c.contains("custom_nodes/HerrgottsH3Infinite") && c.contains("/workspace/agent/h3_infinite.tgz"), "{c}");
        assert!(!c.contains("git clone"));
        let tgz = include_bytes!("../../../resources/extras/h3_infinite.tar.gz");
        assert_eq!(tgz[..2], [0x1f, 0x8b]);
    }

    #[test]
    fn boot_script_filled() {
        let s = boot_script("/workspace/agent");
        assert!(!s.contains('@'), "還有沒換掉的佔位符");
        assert!(s.contains("qwen3.5_9b_w4a8_gptq.safetensors") && s.contains("flux2-vae.safetensors") && s.contains("Comfy-Org/MOSS-Image"));
        assert!(s.contains("APEX-I-Compact.gguf") && s.contains("b11574") && !s.contains("Qwen-Image-2.1"));
        assert!(s.contains("local_dir=\"/workspace/agent/models\"") && s.contains("A=/workspace/agent"));
        assert!(!s.contains('\r'));
        // 先檢查主機，裝完才跑 run.sh；run.sh 最後才寫 ready
        assert!(s.contains("step check") && s.contains("1208") && s.contains("44000"));
        assert!(s.trim_end().ends_with("bash $A/run.sh"));
        let r = run_script("/home/u/anima_agent", SSH_LLM_PORT, SSH_COMFY_PORT);
        assert!(!r.contains('@'));
        assert!(r.contains("--port 38199") && r.contains("--port 38188") && r.contains("A=/home/u/anima_agent"));
        assert!(r.contains("llm.pid") && r.trim_end().ends_with("echo ready > $A/step"));
    }

    #[test]
    fn install_state_parsed() {
        // step 檔本身就有換行：不能靠「第二行」找 packs（審查第 1 點：舊寫法讀到空行，每次都重裝）
        assert_eq!(parse_install("S:installed\n\nP:agent,h3\n"), ("installed".to_string(), Packs { agent: true, h3: true, h3x: false }));
        // 舊版裝的機器沒有 packs 檔：當成只有助手（舊版只會裝它）
        assert_eq!(parse_install("S:ready\n\nP:\n"), ("ready".to_string(), Packs::default()));
        assert_eq!(parse_install("S:\nP:\n").0, "");
        let (step, have) = parse_install("S:installed\n\nP:agent\n");
        assert!(INSTALLED.contains(&step.as_str()) && Packs::default().within(have));
    }

    #[test]
    fn url_mode_follows_packs() {
        // 網址模式只裝 H3 的 ComfyUI：不用填腦袋網址（審查第 5 點）
        let h3 = Packs { agent: false, h3: true, h3x: false };
        assert!(url_missing("http://c", "", h3).is_ok());
        assert!(url_missing("", "http://l", h3).is_err());
        assert!(url_missing("http://c", "", Packs::default()).is_err_and(|e| e.contains("腦袋")));
        assert!(url_missing("http://c", "http://l", Packs::default()).is_ok());
        // 勾了 H3 就要查得到 H3 節點
        assert!(h3_node_ok(&json!({"MiniMaxH3ReferenceToVideo": {"input": {}}})));
        assert!(!h3_node_ok(&json!({})) && !h3_node_ok(&Value::Null));
    }

    #[test]
    fn wait_gives_up() {
        // 雲端 ComfyUI 當掉或把工作弄丟：不能一直等（一直等＝機器一直計費）
        assert_eq!(wait_verdict(0, false, 10), None);
        assert!(wait_verdict(MAX_POLL_FAILS, false, 10).is_some_and(|e| e.contains("沒有回應")));
        assert!(wait_verdict(0, true, 10).is_some_and(|e| e.contains("找不到這個工作")));
        assert!(wait_verdict(0, false, MAX_WAIT_SECS + 1).is_some());
        let q = json!({"queue_running": [[1, "a", {}, {}, []]], "queue_pending": [[2, "b", {}, {}, []]]});
        assert!(in_queue(&q, "a") && in_queue(&q, "b") && !in_queue(&q, "c"));
    }

    #[test]
    fn boot_script_packs() {
        let h3 = Packs::from_value(Some(&json!({"h3": true})));
        assert_eq!(h3, Packs { agent: false, h3: true, h3x: false });
        let s = boot_script_for("/workspace/agent", h3);
        assert!(!s.contains('@'), "還有沒換掉的佔位符");
        assert!(!s.contains("llama") && !s.contains("APEX"), "沒勾助手就不裝腦袋");
        assert!(s.contains("10Eros_Max_h3_TURBO-hybrid_beta5_int8.safetensors") && s.contains("qwen3vl_32b_minimax_h3_nvfp4_awq"));
        assert!(s.contains(UPSCALER_COMMIT) && s.contains("latent_upscale_models") && s.contains("wait $P3"));
        assert!(!s.contains("wait $P1") && !s.contains("Comfy-Org/SDPose"), "h3x 沒勾就不下載進階包");
        // 放大節點下載或切版本失敗要停下來說原因（審查第 16 點：&& 串起來的 clone 失敗不會觸發 set -e）
        let clone = s.lines().find(|l| l.contains(UPSCALER_COMMIT)).unwrap_or("");
        assert!(clone.matches("|| { rm -rf $N; die").count() == 2 && !clone.contains("&& git -C"), "{clone}");
        assert!(s.contains("echo \"h3\" > $A/packs") && s.contains("-ge 44000") && s.contains(&format!("-ge {}", h3.disk_gb() * 1000)));
        assert!(s.trim_end().ends_with("bash $A/run.sh"));
        let r = run_script_for("/workspace/agent", 8199, 8188, h3);
        assert!(!r.contains("--mmproj") && r.contains("ComfyUI/main.py") && !r.contains('@'));

        let all = Packs::from_value(Some(&json!({"agent": true, "h3x": true})));
        assert_eq!(all, Packs { agent: true, h3: true, h3x: true });
        let s = boot_script_for("/workspace/agent", all);
        assert!(s.contains("APEX-I-Compact.gguf") && s.contains("Comfy-Org/SDPose") && s.contains("wait $P1") && s.contains("wait $P3"));
        assert!(s.contains("-ge 90000"), "助手＋H3 要 96GB 等級");
        assert!(run_script_for("/workspace/agent", 8199, 8188, all).contains("--mmproj"));
        // 沒設定＝舊版（只有助手）；什麼都沒勾也當助手
        assert_eq!(Packs::from_value(None), Packs::default());
        assert_eq!(Packs::from_value(Some(&json!({}))), Packs::default());
        assert_eq!(Packs::from_tag(&all.tag()), all);
        assert!(h3.within(all) && !all.within(h3));
        assert!(all.container_gb() >= all.disk_gb() && Packs::default().container_gb() == 120);
    }

    #[test]
    fn pod_roundtrip() {
        let p = Pod { kind: "runpod".into(), id: "abc".into(), price: 0.59, created: 1.0, ip: "1.2.3.4".into(), port: 22084, gpu: "RTX A6000".into(),
                      user: "root".into(), dir: RUNPOD_DIR.into(), key: String::new(), llm_port: 8199, comfy_port: 8188, packs: "h3,h3x".into() };
        let q = Pod::from_value(&p.to_value()).unwrap();
        assert_eq!(Packs::from_tag(&q.packs), Packs { agent: false, h3: true, h3x: true });
        assert_eq!((q.id, q.port, q.ip, q.gpu), ("abc".to_string(), 22084, "1.2.3.4".to_string(), "RTX A6000".to_string()));
        // 舊版 pod.json（只有 RunPod 的欄位）照 RunPod 補
        let old = Pod::from_value(&json!({"id": "x", "price": 0.59, "created": 1.0, "ip": "1.2.3.4", "port": 22, "gpu": "A40"})).unwrap();
        assert_eq!((old.kind.as_str(), old.user.as_str(), old.dir.as_str(), old.llm_port, old.comfy_port), ("runpod", "root", RUNPOD_DIR, 8199, 8188));
        assert!(Pod::from_value(&json!({})).is_none());
        assert_eq!(gpu_name_of(&json!({"machine": {"gpuDisplayName": "RTX A6000"}})).as_deref(), Some("RTX A6000"));
        assert_eq!(gpu_name_of(&json!({"gpu": {"id": "NVIDIA A40"}})).as_deref(), Some("A40"));
        assert!(gpu_name_of(&json!({})).is_none());
        // 每個選項都是 48GB 以上、價格有填、key 不重複
        assert_eq!(gpu_choices(&[])[0].key, "a6000");
        assert!(GPUS.iter().all(|g| !g.ids.is_empty() && g.price_lo > 0.0 && g.price_hi >= g.price_lo));
    }

    #[test]
    fn gpu_catalog_fills_choices() {
        let raw = json!({"data": {"gpuTypes": [
            {"id": "NVIDIA A40", "displayName": "A40", "memoryInGb": 48, "secureCloud": true, "securePrice": 0.6, "lowestPrice": {"stockStatus": "Low"}},
            {"id": "NVIDIA RTX A6000", "displayName": "RTX A6000", "memoryInGb": 48, "secureCloud": true, "securePrice": 0.59, "lowestPrice": {"stockStatus": null}},
            {"id": "NVIDIA H100 80GB HBM3", "displayName": "H100 SXM", "memoryInGb": 80, "secureCloud": true, "securePrice": 3.99, "lowestPrice": {"stockStatus": "Medium"}},
            {"id": "NVIDIA A100 80GB PCIe", "displayName": "A100 PCIe", "memoryInGb": 80, "secureCloud": true, "securePrice": 1.79, "lowestPrice": {"stockStatus": "High"}},
            {"id": "NVIDIA GeForce RTX 4090", "displayName": "RTX 4090", "memoryInGb": 24, "secureCloud": true, "securePrice": 0.89, "lowestPrice": null},
            {"id": "NVIDIA RTX PRO 6000 Blackwell Server Edition MIG 2g.48gb", "displayName": "PRO 6000 MIG 48GB", "memoryInGb": 48, "secureCloud": true, "securePrice": 1.29},
            {"id": "AMD Instinct MI300X OAM", "displayName": "MI300X", "memoryInGb": 192, "secureCloud": true, "securePrice": 2.39},
            {"id": "NVIDIA A100-SXM4-40GB", "displayName": "A100 SXM 40GB", "memoryInGb": 80, "secureCloud": false, "securePrice": 0}
        ]}});
        let live = parse_catalog(&raw);
        assert_eq!(live.len(), 7);
        let c = gpu_choices(&live);
        // 推薦的 A6000：價格照目錄（A6000 0.59、A40 0.6），A6000 租光但 A40 有貨
        assert_eq!((c[0].key.as_str(), c[0].price_lo, c[0].price_hi, c[0].stock.as_deref()), ("a6000", 0.59, 0.6, Some("Low")));
        // 目錄裡沒有的推薦卡：價格用寫死的、貨量未知
        assert!(c[1].preset && c[1].stock.is_none() && c[1].price_lo == GPUS[1].price_lo);
        // 其他卡：只留 48GB 以上、NVIDIA、不是 MIG，照價格排
        let more: Vec<&str> = c.iter().filter(|g| !g.preset).map(|g| g.key.as_str()).collect();
        assert_eq!(more, ["NVIDIA A100 80GB PCIe", "NVIDIA H100 80GB HBM3"]);
        assert_eq!(c[3].name, "A100 PCIe 80GB");
        assert_eq!(c[3].ids, ["NVIDIA A100 80GB PCIe"]);
        assert!(parse_catalog(&json!({"errors": []})).is_empty());
    }

    #[test]
    fn inputs_are_checked() {
        assert!(safe_word("123.45.67.89", ".-_:") && safe_word("ssh.vast.ai", ".-_:"));
        assert!(!safe_word("-oProxyCommand=calc", ".-_:"), "開頭是 - 會被 ssh 當成選項");
        assert!(!safe_word("a b", ".-_:") && !safe_word("a;rm", ".-_:") && !safe_word("", "."));
        assert!(safe_word("/workspace/anima_agent", "/._-~") && !safe_word("/tmp/x'y", "/._-~") && !safe_word("/tmp/$(id)", "/._-~"));
        assert_eq!(norm_url("http://1.2.3.4:8080/v1/"), "http://1.2.3.4:8080");
        assert_eq!(norm_url(" https://x.example.com/ "), "https://x.example.com");
        let info = json!({"N": {"input": {"required": {"a": [["x", "y"]], "b": ["COMBO", {"options": ["z"]}]}}}});
        assert_eq!(combo(&info, "N", "a"), vec!["x", "y"]);
        assert_eq!(combo(&info, "N", "b"), vec!["z"]);
        assert!(combo(&info, "N", "c").is_empty());
    }

    #[test]
    fn secrets_are_sealed_on_disk() {
        let mut m = Map::new();
        m.insert("provider".into(), json!("url"));
        m.insert("llm_key".into(), json!("SECRET-TEST-VALUE"));
        let disk = sealed(&m).unwrap();
        assert!(disk.get("llm_key").is_none());
        let text = Value::Object(disk.clone()).to_string();
        #[cfg(windows)]
        assert!(!text.contains("SECRET-TEST-VALUE"), "Windows 上金鑰要加密");
        let dir = std::env::temp_dir().join(format!("anima_cloud_cfg_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cloud.json"), text).unwrap();
        let back = load_cfg(&dir);
        assert_eq!(back["llm_key"], "SECRET-TEST-VALUE");
        assert_eq!(back["provider"], "url");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
