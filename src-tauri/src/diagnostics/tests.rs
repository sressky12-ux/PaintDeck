use super::*;

fn fixture() -> (crate::paths::Paths, Arc<Store>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../待刪除")
        .join(format!(
            "diagnostics-test-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ));
    let paths = crate::paths::Paths::new(&root, &root.join("ui"), Some(root.join("comfy")));
    let store = Store::new(&paths);
    (paths, store)
}

#[test]
fn secrets_are_removed_before_disk_and_inline_media_is_not_saved() {
    let (_, store) = fixture();
    let run=store.begin("/api/generate",&json!({"api_key":"fixture-super-secret","prompt":"use fixture-super-secret", "image":"data:image/png;base64,AAAA","url":"https://user:password@example.test/p?token=private"})).unwrap();
    run.http_finished(400);
    drop(run);
    let all = safe_files(&store.root)
        .unwrap()
        .iter()
        .map(|p| String::from_utf8_lossy(&fs::read(p).unwrap()).into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!all.contains("fixture-super-secret"));
    assert!(!all.contains("base64,AAAA"));
    assert!(!all.contains("token=private"));
}

#[test]
fn quota_and_retention_preserve_active_records_and_original_files() {
    let (paths, store) = fixture();
    fs::create_dir_all(&paths.output).unwrap();
    fs::write(paths.output.join("original.png"), b"original").unwrap();
    let success = store.begin("/api/generate", &json!({})).unwrap();
    let success_id = success.id.clone();
    success.http_finished(200);
    drop(success);
    let failed = store.begin("/api/agent/say", &json!({})).unwrap();
    let error_id = failed.id.clone();
    failed.fail("fixture OOM");
    failed.http_finished(200);
    drop(failed);
    let active = store.begin("/api/generate", &json!({})).unwrap();
    {
        let mut inner = store.inner.lock().unwrap();
        for id in [&success_id, &error_id] {
            let r = inner.records.get_mut(id).unwrap();
            r.created = now() - 172800.0;
            r.updated = r.created;
            r.ended = Some(r.created);
            store.persist(&mut inner, id).unwrap();
        }
    }
    store.cleanup(false);
    assert!(!store.root.join("runs").join(success_id).exists());
    assert!(store.root.join("runs").join(error_id).exists());
    store.cleanup(true);
    assert!(store.root.join("runs").join(&active.id).exists());
    assert_eq!(
        fs::read(paths.output.join("original.png")).unwrap(),
        b"original"
    );
}

#[tokio::test]
#[allow(clippy::async_yields_async)] // Return the worker handle to test ownership after the request has ended.
async fn background_and_blocking_work_keep_ownership_until_finished() {
    let (_, store) = fixture();
    let run = store.begin("/api/agent/say", &json!({})).unwrap();
    let id = run.id.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let handle = scope(run.clone(), async {
        spawn(async move {
            let _ = rx.await;
            blocking(|| snapshot("worker", &json!({"ok":true}))).await;
        })
    })
    .await;
    run.http_finished(200);
    drop(run);
    assert!(store.inner.lock().unwrap().records[&id].active);
    store.cleanup(true);
    assert!(store.root.join("runs").join(&id).exists());
    tx.send(()).unwrap();
    handle.await.unwrap();
    assert_eq!(store.inner.lock().unwrap().records[&id].status, "success");
}

#[test]
fn export_has_manifest_hashes_and_download_lease_prevents_cleanup() {
    use std::io::Read;
    let (_, store) = fixture();
    let run = store
        .begin("/api/generate", &json!({"prompt":"fixture"}))
        .unwrap();
    let id = run.id.clone();
    run.http_finished(200);
    drop(run);
    let export = store.export(&id).unwrap();
    let path = export.path.clone();
    store.cleanup(true);
    assert!(store.root.join("runs").join(&id).exists());
    let mut archive = zip::ZipArchive::new(fs::File::open(&path).unwrap()).unwrap();
    let manifest: Value = {
        let mut bytes = vec![];
        archive
            .by_name("manifest.json")
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    };
    assert_eq!(manifest["is_example"], false);
    for entry in manifest["files"].as_array().unwrap() {
        let mut b = vec![];
        archive
            .by_name(entry["path"].as_str().unwrap())
            .unwrap()
            .read_to_end(&mut b)
            .unwrap();
        assert_eq!(sha256(&b), entry["sha256"]);
    }
    drop(archive);
    drop(export);
    assert!(!path.exists());
    store.cleanup(true);
    assert!(!store.root.join("runs").join(id).exists());
}

#[test]
fn pending_comfy_job_survives_restart_as_unknown_until_confirmed_terminal() {
    let (paths, store) = fixture();
    let run = store.begin("/api/generate", &json!({})).unwrap();
    let id = run.id.clone();
    store.bind_job(&id, "fixture-job");
    run.http_finished(200);
    drop(run);
    drop(store);
    let store = Store::new(&paths);
    assert_eq!(store.inner.lock().unwrap().records[&id].status, "unknown");
    store.cleanup(true);
    assert!(store.check_dir(&id).is_ok());
    store.job_event(
        "fixture-job",
        "execution_error",
        &json!({"exception_type":"FixtureError","traceback":["fixture.py:42"]}),
    );
    assert_eq!(store.inner.lock().unwrap().records[&id].status, "error");
}

#[test]
fn quota_reclaims_closed_success_but_truncates_instead_of_removing_active_data() {
    let (_, store) = fixture();
    store
        .preferences(Policy {
            max_mib: 50,
            ..Policy::default()
        })
        .unwrap();
    let old = store.begin("/fixture/old", &json!({})).unwrap();
    let old_id = old.id.clone();
    old.http_finished(200);
    drop(old);
    let filler = store.check_dir(&old_id).unwrap().join("fixture.bin");
    fs::File::create(filler).unwrap().set_len(48 * MIB).unwrap();
    store.inner.lock().unwrap().used = tree_bytes(&store.root).unwrap();
    let active = store.begin("/fixture/new", &json!({})).unwrap();
    let large = json!(vec!["x".repeat(8192); 256]);
    active.snapshot("large", &large);
    assert!(!store.root.join("runs").join(old_id).exists());
    let filler = store.check_dir(&active.id).unwrap().join("active.bin");
    fs::File::create(filler).unwrap().set_len(47 * MIB).unwrap();
    store.inner.lock().unwrap().used = tree_bytes(&store.root).unwrap();
    let before = store.inner.lock().unwrap().records[&active.id].truncated;
    active.snapshot("does_not_fit", &large);
    assert!(store.inner.lock().unwrap().records[&active.id].truncated > before);
    assert!(tree_bytes(&store.root).unwrap() <= 50 * MIB);
    assert!(store.check_dir(&active.id).is_ok());
    drop(active);
    store.cleanup(true);
}

#[test]
fn bounded_log_sampling_omits_old_bytes_and_marks_gaps() {
    let (paths, store) = fixture();
    let path = paths.data.join("logs/comfyui.log");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"before-operation").unwrap();
    let run = store.begin("/fixture/logs", &json!({})).unwrap();
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(&vec![b'x'; 300 * 1024]).unwrap();
    file.write_all(b" FixtureOOM").unwrap();
    drop(file);
    store.sample_logs(&run.id);
    let output = fs::read_to_string(
        store
            .check_dir(&run.id)
            .unwrap()
            .join("comfy_stdout_stderr.jsonl"),
    )
    .unwrap();
    // 開機那段另外收成 process_log_comfy_startup；這次操作期間的取樣不重複帶舊的
    let sampled: Vec<Value> = output.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| v["type"] == "process_log_comfy").collect();
    assert_eq!(sampled.len(), 1);
    let data = &sampled[0];
    assert!(!data["data"]["text"].as_str().unwrap().contains("before-operation"));
    assert!(data["data"]["text"].as_str().unwrap().contains("FixtureOOM"));
    assert!(data["data"]["skipped_bytes"].as_u64().unwrap() > 0);
    assert!(store.inner.lock().unwrap().records[&run.id].truncated > 0);
}

/// 2026-11-05 回報：A 卡網友的診斷包沒有 ComfyUI 開機那段，看不到 [繪匣] A 卡狀態行和 PyTorch 版本
#[test]
fn startup_log_head_is_captured() {
    let (paths, store) = fixture();
    let path = paths.data.join("logs/comfyui.log");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "[繪匣] A 卡：AMD Radeon RX 6750 GRE 10GB（gfx1031）\npytorch version: 2.13.0+rocm10.0.0\nTo see the GUI go to: http://127.0.0.1:8188\ngot prompt\nlater-output\n").unwrap();
    let run = store.begin("/fixture/startup", &json!({})).unwrap();
    let output = fs::read_to_string(store.check_dir(&run.id).unwrap().join("comfy_stdout_stderr.jsonl")).unwrap();
    let head: Value = output.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).find(|v| v["type"] == "process_log_comfy_startup").unwrap();
    let text = head["data"]["text"].as_str().unwrap();
    assert!(text.contains("[繪匣] A 卡") && text.contains("rocm10.0.0") && text.contains("To see the GUI"), "{text}");
    assert!(!text.contains("later-output"), "{text}");
}

#[tokio::test]
#[allow(clippy::async_yields_async)] // The test must abort the child after its owning request returns.
async fn aborted_worker_is_unknown_and_absent_job_releases_protection() {
    let (_, store) = fixture();
    let run = store.begin("/fixture/cancel", &json!({})).unwrap();
    let id = run.id.clone();
    let (ready, rx) = tokio::sync::oneshot::channel();
    let task = scope(run.clone(), async {
        spawn(async move {
            let _ = ready.send(());
            std::future::pending::<()>().await;
        })
    })
    .await;
    rx.await.unwrap();
    run.http_finished(200);
    drop(run);
    task.abort();
    let _ = task.await;
    assert_eq!(store.inner.lock().unwrap().records[&id].status, "unknown");
    let run = store.begin("/fixture/missing", &json!({})).unwrap();
    let id = run.id.clone();
    store.bind_job(&id, "not-in-backend");
    run.http_finished(200);
    drop(run);
    store.job_missing("not-in-backend");
    let inner = store.inner.lock().unwrap();
    assert_eq!(inner.records[&id].status, "unknown");
    assert!(!inner.records[&id].protected());
}

#[test]
fn unknown_folders_and_invalid_paths_are_never_removed_or_exported() {
    let (_, store) = fixture();
    let path = store.root.join("runs/diag-foreign");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("precious.txt"), b"fixture-do-not-delete").unwrap();
    store.cleanup(true);
    assert!(path.join("precious.txt").exists());
    assert!(store.export("../outside").is_err());
    assert!(store.export("diag-foreign").is_err());
    let run = store.begin("/fixture/emptydirs", &json!({})).unwrap();
    let id = run.id.clone();
    fs::create_dir_all(store.check_dir(&id).unwrap().join("empty/nested")).unwrap();
    run.http_finished(200);
    drop(run);
    store.cleanup(true);
    assert!(!store.root.join("runs").join(id).exists());
}

#[cfg(windows)]
#[test]
fn junction_in_owned_record_cannot_delete_or_export_the_target() {
    use std::os::windows::process::CommandExt;
    let (paths, store) = fixture();
    let run = store.begin("/fixture/junction", &json!({})).unwrap();
    let id = run.id.clone();
    run.http_finished(200);
    drop(run);
    let target = paths.root.join("fixture-outside-diagnostics");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("precious.txt"), b"keep").unwrap();
    let link = store.check_dir(&id).unwrap().join("linked");
    let target_argument = target
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_owned();
    let link_argument = link
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_owned();
    // cmd creates the junction only; no shell is used for deletion or moving.
    let output = std::process::Command::new("cmd.exe")
        .args(["/c", "mklink", "/J"])
        .arg(&link_argument)
        .arg(&target_argument)
        .creation_flags(0x08000000)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(store.export(&id).is_err());
    store.cleanup(true);
    assert_eq!(fs::read(target.join("precious.txt")).unwrap(), b"keep");
    assert!(store.check_dir(&id).is_ok());
    // Remove only the junction entry, never recurse into its target.
    fs::remove_dir(&link).unwrap();
    store.cleanup(true);
    assert!(!store.root.join("runs").join(id).exists());
    assert!(target.join("precious.txt").exists());
}

#[test]
fn confirmed_success_after_restart_preserves_capture_gap_in_manifest() {
    use std::io::Read;
    let (paths, store) = fixture();
    let run = store.begin("/fixture/restart", &json!({})).unwrap();
    let id = run.id.clone();
    store.bind_job(&id, "resumed-job");
    run.http_finished(200);
    drop(run);
    drop(store);
    let store = Store::new(&paths);
    store.job_event("resumed-job", "execution_success", &json!({}));
    assert_eq!(store.inner.lock().unwrap().records[&id].status, "success");
    let export = store.export(&id).unwrap();
    let mut zip = zip::ZipArchive::new(fs::File::open(&export.path).unwrap()).unwrap();
    let mut manifest = String::new();
    zip.by_name("manifest.json")
        .unwrap()
        .read_to_string(&mut manifest)
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&manifest).unwrap()["partial"],
        true
    );
}

#[test]
fn manual_report_starts_a_fresh_abnormal_retention_window() {
    let (_, store) = fixture();
    let run = store.begin("/fixture/report", &json!({})).unwrap();
    let id = run.id.clone();
    run.http_finished(200);
    drop(run);
    {
        let mut inner = store.inner.lock().unwrap();
        let r = inner.records.get_mut(&id).unwrap();
        r.created = now() - 5.0 * 86400.0;
        r.updated = r.created;
        r.ended = Some(r.created);
        store.persist(&mut inner, &id).unwrap();
    }
    store
        .report(
            &id,
            &json!({"expected":"fixture","actual":"fixture mismatch"}),
        )
        .unwrap();
    store.cleanup(false);
    assert!(store.check_dir(&id).is_ok());
    assert!(store.inner.lock().unwrap().records[&id].ended.unwrap() > now() - 10.0);
}
