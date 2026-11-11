//! 訓練台的魔塔（ModelScope）這條路。實驗性：魔塔沒有公開的訓練 API，AIGC 專區的訓練頁只能用網頁操作。
//! 目前做到：把訓練圖和描述包成魔塔訓練頁收的壓縮檔（圖和同名 .txt 放在同一層），告訴使用者去哪裡上傳。
use super::api_train::{live, pdir, read_caption};
use super::{blocking, internal, ok, ApiResult, Shared};
use serde_json::{json, Value};
use std::path::Path;

pub const TRAIN_PAGE: &str = "https://modelscope.cn/aigc/model-training";

/// 圖和描述包成一個 zip（同一層、同名 .txt），回筆數
pub fn pack(dir: &Path, files: &[String], dest: &Path) -> Result<usize, String> {
    use std::io::Write;
    let f = std::fs::File::create(dest).map_err(|e| format!("建不了壓縮檔：{e}"))?;
    let mut z = zip::ZipWriter::new(f);
    let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for file in files {
        let data = std::fs::read(dir.join("images").join(file)).map_err(|e| format!("讀不到訓練圖 {file}：{e}"))?;
        z.start_file(file.as_str(), opt).map_err(|e| e.to_string())?;
        z.write_all(&data).map_err(|e| e.to_string())?;
        z.start_file(Path::new(file).with_extension("txt").to_string_lossy(), opt).map_err(|e| e.to_string())?;
        z.write_all(read_caption(dir, file).as_bytes()).map_err(|e| e.to_string())?;
    }
    z.finish().map_err(|e| e.to_string())?;
    Ok(files.len())
}

pub async fn start(app: &Shared, id: &str, p: &Value) -> ApiResult {
    let dir = pdir(app, id);
    let files: Vec<String> = live(p).iter().filter_map(|i| i["file"].as_str()).map(str::to_string).collect();
    let dest = dir.join(format!("{}_modelscope.zip", crate::train::lora_name(p)));
    let (d, dst) = (dir.clone(), dest.clone());
    let n = blocking(move || pack(&d, &files, &dst)).await.map_err(internal)?;
    ok(json!({
        "ok": true, "manual": true, "zip": dest.to_string_lossy(), "files": n, "url": TRAIN_PAGE,
        "base": crate::train::fam(p["family"].as_str().unwrap_or("")).and_then(|f| f.modelscope),
        "msg": "魔塔沒有開放訓練用的 API，要在它的網頁上送出。訓練資料已經包好：到魔塔的訓練頁建資料集、上傳這個壓縮檔，照畫面上的參數填。",
    }))
}
