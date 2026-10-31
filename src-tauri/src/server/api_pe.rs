//! AI 改寫提示詞的 API（對照舊版 server.py 的 /api/pe/*；實際工作在 crate::pe::Service）。
use super::{internal, ok, ApiResult, Body, Shared};
use crate::py::str_of;
use axum::extract::State;
use axum::response::Response;
use serde_json::{json, Value};

/// SSE 串流：status／reasoning／content／zh_delta／result／error／cancelled 事件
pub async fn rewrite(State(app): State<Shared>, Body(body): Body) -> Response {
    app.pe.rewrite_sse(body)
}

pub async fn cancel(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let rid = str_of(body.get("rid").unwrap_or(&Value::Null));
    ok(json!({"ok": app.pe.cancel(&rid)}))
}

pub async fn status(State(app): State<Shared>) -> ApiResult {
    ok(app.pe.status())
}

/// 姿勢大彙整的選單（id、中文名、類別、人數、有沒有姿勢參考圖、中文說明）
pub async fn poses(State(app): State<Shared>) -> ApiResult {
    ok(json!({"poses": app.pe.poses.menu(), "control_dir": super::POSE_INPUT}))
}

/// 可以自訂的系統提示詞（目前內容、預設內容、有沒有改過）
pub async fn prompts(State(app): State<Shared>) -> ApiResult {
    ok(json!({"prompts": app.pe.prompts_list().map_err(super::bad)?}))
}

/// 儲存自訂系統提示詞：{"id": ..., "text": ...}；text 空白＝還原預設
pub async fn prompt_save(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let id = str_of(body.get("id").unwrap_or(&Value::Null));
    let text = body.get("text").and_then(Value::as_str).unwrap_or("");
    ok(app.pe.prompt_save(&id, text).map_err(super::bad)?)
}

pub async fn stop(State(app): State<Shared>) -> ApiResult {
    let stopped = app.pe.stop("網頁按了關閉").await.map_err(internal)?;
    ok(json!({"stopped": stopped}))
}

/// 升級換模型做完的記號（在 data 資料夾）：有這個檔就不再做
pub const RETIRE_MARK: &str = "llm_switch_2610.done";

/// 2026-10-30 使用者：「把PE版的改寫模型在下一個版本直接砍掉，砍死，再也不見，半強迫的使用Qwen 通用的9B模型。
/// 然後讓下一個更新的人，自動把舊模型真刪除，換新的上去。」升級後開站時做一次：
/// 1. 站找得到的官方 PE（LLM 資料夾＋外部模型資料夾）真刪除
/// 2. 用過 AI 改寫（有 PE 或任何通用模型）卻沒有 Defiant Fable 9B：照安裝清單下載「AI 改寫、生圖助手」包缺的語言模型檔
///    （使用者選「一律換成新 9B」：只有 35B 的人也下載）。從沒裝過改寫的人不硬塞 6GB，用到時改寫框會叫他去下載
/// 3. 新 9B 下載核對完才真刪程式以前發的 Huihui 9B；下載失敗就留著舊的，下次開站再試
///
/// 全部做完寫 data\llm_switch_2610.done。進度放在 AI 改寫狀態的 retire（/api/state 的 pe.retire），網頁在改寫框裡顯示
pub async fn retire_old_models(app: Shared) {
    let mark = app.paths.data.join(RETIRE_MARK);
    let Some(core) = app.pe.core() else { return };
    if mark.exists() {
        return;
    }
    // 用環境變數 ANIMA_ROOT 開的開發／測試站（正式程式不設，見 main.rs env_config）沒指定 ANIMA_PE_MODELS：
    // 找的多半是使用者真的語言模型資料夾（測試區 fakecomfy\models\LLM 是 junction 過去的），不自動刪檔
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    if set("ANIMA_ROOT") && !set("ANIMA_PE_MODELS") {
        eprintln!("[換模型] 測試站（ANIMA_ROOT）沒設 ANIMA_PE_MODELS，不自動刪舊模型");
        return;
    }
    let gb = |b: u64| format!("{:.1} GB", b as f64 / (1u64 << 30) as f64);
    refresh_node(&core.dir, &app.paths.res);
    let plan = core.retire_plan();
    // 舊版留下的 llama-server 還載著 PE：先關掉，不然檔案刪不掉
    if matches!(core.read_state().get("kind").and_then(Value::as_str), Some("t2i" | "i2i")) {
        let _ = app.pe.stop("官方 PE 改寫模型要刪掉了").await;
    }
    let (freed, mut fails) = remove_all(&plan.pe);
    let gone = if plan.pe.is_empty() { String::new() } else { format!("以前的官方 PE 改寫模型已經不再支援，刪掉了（空出 {}）。", gb(freed)) };
    let wanted: Vec<_> = if plan.had_llm && !plan.has_fable {
        crate::setup::llm_pack_files(&app.paths.res).into_iter().filter(|f| !core.has_file(&f.0)).collect()
    } else {
        vec![]
    };
    let mut changed = !plan.pe.is_empty();
    if !wanted.is_empty() {
        changed = true;
        let size = gb(wanted.iter().map(|f| f.1).sum());
        let head = if plan.old.is_empty() {
            format!("{gone}AI 改寫改用新的通用 9B（Qwen3.5 Defiant Fable），正在自動下載（約 {size}）；下載好會自動換上。")
        } else {
            format!("{gone}AI 改寫改用新的通用 9B（Qwen3.5 Defiant Fable），正在自動下載（約 {size}）；下載好會自動換上，舊的 9B 也會刪掉。")
        };
        let ids: Vec<u64> = wanted
            .into_iter()
            .map(|(name, size, url, sha)| super::api_models::download_to(url, core.models.join(&name), size, sha))
            .collect();
        loop {
            let (done, total, over, errs) = super::api_models::progress_of(&ids);
            if !errs.is_empty() && over {
                let errs = errs.join("；");
                // 只剩官方 PE 的人，PE 已經刪了：別說「舊的先留著」，講清楚現在改寫不能用
                let msg = if core.model_files("adult", None).is_ok() {
                    format!("{gone}新的通用 9B 下載失敗：{errs}。舊的模型先留著，下次開啟繪匣會再試；也可以到「安裝與修復」勾「AI 改寫、生圖助手」補下載。")
                } else {
                    format!("{gone}新的通用 9B 下載失敗：{errs}。下載好之前 AI 改寫不能用；下次開啟繪匣會自動再試，也可以到「安裝與修復」勾「AI 改寫、生圖助手」補下載。")
                };
                app.pe.set_retire(json!({"phase": "error", "msg": msg}));
                return;
            }
            if over {
                break;
            }
            let pct = (done * 100).checked_div(total).unwrap_or(0);
            app.pe.set_retire(json!({"phase": "downloading", "msg": head, "pct": pct}));
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }
    // 新 9B 到位（或本來就有）：程式以前發的 Huihui 9B 也刪掉
    let after = core.retire_plan();
    if after.has_fable && !after.old.is_empty() {
        changed = true;
        // 下載那幾分鐘使用者可能自己用了改寫，llama-server 正載著舊 9B 就刪不掉：先關（正在改寫會等它寫完）
        let loaded = core.read_state().get("model").and_then(Value::as_str).map(str::to_string);
        let is_old = |m: &str| after.old.iter().any(|p| p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(m)));
        if loaded.as_deref().is_some_and(is_old) {
            let _ = app.pe.stop("舊的 9B 要刪掉了").await;
        }
        fails.extend(remove_all(&after.old).1);
    }
    if !fails.is_empty() {
        app.pe.set_retire(json!({"phase": "error", "msg": format!(
            "{gone}有幾個舊的改寫模型檔刪不掉（可能正被別的程式開著）：{}。下次開啟繪匣會再試。",
            fails.join("；")
        )}));
        return;
    }
    if let Err(e) = std::fs::write(&mark, b"") {
        eprintln!("寫入 {} 失敗：{e}", mark.display());
    }
    if changed {
        let msg = if after.has_fable {
            format!("{gone}AI 改寫已經換成新的通用 9B（Qwen3.5 Defiant Fable）。")
        } else {
            format!("{gone}AI 改寫照樣用你的通用模型。")
        };
        app.pe.set_retire(json!({"phase": "done", "msg": msg}));
    }
}

/// ComfyUI 裡的「Anima Prompt Enhance」節點：安裝精靈遇到已經有的節點資料夾就跳過，舊安裝的 pe_core.py 還在找官方 PE，
/// PE 刪掉它就壞。認得出是舊版（還在找 pe_{kind}_heretic）才換成程式附帶的新版，舊檔留一份 <檔名>.old_pe
fn refresh_node(node: &std::path::Path, res: &std::path::Path) {
    let old = std::fs::read_to_string(node.join("pe_core.py")).unwrap_or_default();
    if !old.contains("pe_{kind}_heretic") {
        return;
    }
    let src = res.join("extras").join("anima_prompt_enhancer");
    for f in ["pe_core.py", "__init__.py"] {
        let (from, to) = (src.join(f), node.join(f));
        if !from.is_file() {
            continue;
        }
        if to.is_file() {
            let _ = std::fs::copy(&to, node.join(format!("{f}.old_pe")));
        }
        match std::fs::copy(&from, &to) {
            Ok(_) => eprintln!("[換模型] 節點換新 {}", to.display()),
            Err(e) => eprintln!("[換模型] 節點換新 {} 失敗：{e}", to.display()),
        }
    }
}

/// 真刪除這些檔（不進資源回收筒）：回傳（空出的位元組, 刪不掉的原因）
fn remove_all(files: &[std::path::PathBuf]) -> (u64, Vec<String>) {
    let (mut freed, mut fails) = (0, vec![]);
    for f in files {
        let size = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
        match std::fs::remove_file(f) {
            Ok(()) => {
                freed += size;
                eprintln!("[換模型] 刪掉 {}", f.display());
            }
            Err(e) => fails.push(format!("{}（{e}）", f.display())),
        }
    }
    (freed, fails)
}

/// 「下載讀圖檔」：生圖助手、導演台看圖要的 mmproj 不見了，從安裝清單查網址下載到語言模型資料夾。
/// {brain?: "9b"|"35b"} → {ok, have: true}（已經有了）或 {ok, id, name, size}（進度看 /api/models/downloads）
pub async fn mmproj(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let brain = body["brain"].as_str().filter(|b| matches!(*b, "9b" | "35b"));
    match app.pe.mmproj_download(brain).map_err(super::bad)? {
        None => ok(json!({"ok": true, "have": true})),
        Some((dest, url, size, sha)) => {
            let name = dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let id = super::api_models::download_to(url, dest, size, sha);
            ok(json!({"ok": true, "id": id, "name": name, "size": size}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 舊安裝的 ComfyUI 節點（pe_core.py 還在找 pe_{kind}_heretic）升級時換成程式附帶的新版，舊檔留 .old_pe；
    /// 已經是新版、或使用者自己改成別的，不動
    #[test]
    fn refresh_node_replaces_only_the_old_pe_version() {
        let base = std::env::temp_dir().join(format!("anima_refresh_node_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let node = base.join("custom_nodes").join("anima_prompt_enhancer");
        let src = base.join("res").join("extras").join("anima_prompt_enhancer");
        std::fs::create_dir_all(&node).unwrap();
        std::fs::create_dir_all(&src).unwrap();
        let old = "p = f\"pe_{kind}_heretic-{q}.gguf\"\n";
        std::fs::write(node.join("pe_core.py"), old).unwrap();
        std::fs::write(node.join("__init__.py"), "# old init\n").unwrap();
        std::fs::write(src.join("pe_core.py"), "# general model\n").unwrap();
        std::fs::write(src.join("__init__.py"), "# new init\n").unwrap();
        let read = |p: &std::path::Path| std::fs::read_to_string(p).unwrap();
        refresh_node(&node, &base.join("res"));
        assert_eq!(read(&node.join("pe_core.py")), "# general model\n");
        assert_eq!(read(&node.join("__init__.py")), "# new init\n");
        assert_eq!(read(&node.join("pe_core.py.old_pe")), old);
        assert_eq!(read(&node.join("__init__.py.old_pe")), "# old init\n");
        // 新版、使用者改過的：不蓋
        std::fs::write(node.join("pe_core.py"), "# my own edit\n").unwrap();
        refresh_node(&node, &base.join("res"));
        assert_eq!(read(&node.join("pe_core.py")), "# my own edit\n");
        std::fs::remove_dir_all(base).unwrap();
    }
}
