use agy_mobile::core::{AgentMode, RuntimeSelection};
use agy_mobile::runtime::AgyEngine;
use serde_json::{json, Value};
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn spawn_mock_google_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_bg = hits.clone();

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let hits_turn = hits_bg.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 32 * 1024];
                let mut total = 0usize;
                loop {
                    let Ok(n) = socket.read(&mut buf[total..]).await else {
                        return;
                    };
                    if n == 0 {
                        break;
                    }
                    total += n;
                    let text = String::from_utf8_lossy(&buf[..total]);
                    if let Some(hdr_end) = text.find("\r\n\r\n") {
                        let headers = &text[..hdr_end];
                        let content_len = headers
                            .lines()
                            .find_map(|l| {
                                let lower = l.to_ascii_lowercase();
                                lower
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if total >= hdr_end + 4 + content_len {
                            break;
                        }
                    }
                }

                let req_str = String::from_utf8_lossy(&buf[..total]).into_owned();
                let first_line = req_str.lines().next().unwrap_or("");

                if first_line.starts_with("POST /oauth/token") {
                    let body_json = json!({
                        "access_token": "ya29.mock_access_token_valid",
                        "refresh_token": "1//mock_refresh_token",
                        "expires_in": 3600,
                        "token_type": "Bearer"
                    })
                    .to_string();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body_json.len(),
                        body_json
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    return;
                }

                if !req_str.to_ascii_lowercase().contains("authorization: bearer ya29.mock_access_token_valid") {
                    let err = "{\"error\":\"unauthorized\"}";
                    let resp = format!(
                        "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        err.len(),
                        err
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    return;
                }

                let call_index = hits_turn.fetch_add(1, Ordering::SeqCst);
                let sse_event: Value = match call_index {
                    0 => json!({
                        "candidates": [{
                            "content": {
                                "parts": [
                                    { "text": "Inspecting src/app.rs first.\n" },
                                    { "functionCall": { "name": "read_file", "args": { "path": "src/app.rs", "start_line": 1, "end_line": 20 } } }
                                ]
                            }
                        }]
                    }),
                    1 => json!({
                        "candidates": [{
                            "content": {
                                "parts": [
                                    { "text": "Applying targeted fix to src/app.rs.\n" },
                                    { "functionCall": {
                                        "name": "edit_file",
                                        "args": {
                                            "path": "src/app.rs",
                                            "target": "broken_value",
                                            "replacement": "verified_value"
                                        }
                                    }}
                                ]
                            }
                        }]
                    }),
                    _ => json!({
                        "candidates": [{
                            "content": {
                                "parts": [
                                    { "text": "Updated src/app.rs to verified_value." }
                                ]
                            }
                        }]
                    }),
                };

                let sse_body = format!("data: {}\n\n", sse_event);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    sse_body.len(),
                    sse_body
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });

    (format!("http://127.0.0.1:{}", addr.port()), hits)
}

#[test]
fn end_to_end_oauth_pkce_sse_agent_approval_and_session_persistence() {
    let server_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (base_url, hits) = server_rt.block_on(spawn_mock_google_server());

    let server_thread = std::thread::spawn(move || {
        server_rt.block_on(async {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        });
    });

    let app_dir = std::env::temp_dir().join("agy-e2e-integration-suite");
    let _ = fs::remove_dir_all(&app_dir);
    let ws_dir = app_dir.join("project-alpha");
    fs::create_dir_all(ws_dir.join("src")).unwrap();
    fs::write(ws_dir.join("src/app.rs"), "pub const STATE: &str = \"broken_value\";\n").unwrap();

    let vault_key = [0x5Au8; 32];
    // Simulate a 1 GB RAM Android 5.0 (API 21) device running pure-Rust API compatibility runtime.
    let engine = AgyEngine::initialize(app_dir.clone(), 21, 1024, vault_key, true).unwrap();
    engine
        .set_runtime_selection(RuntimeSelection::ForceCompatibleApi)
        .unwrap();
    engine.set_api_endpoint(&format!("{base_url}/v1beta")).unwrap();
    engine.set_token_endpoint_for_testing(format!("{base_url}/oauth/token"));
    engine.set_workspace_root(ws_dir.clone(), None).unwrap();

    // 1. OAuth 2.0 PKCE start + state verification + token exchange + AES-GCM seal
    let auth_url = engine
        .start_oauth_pkce_flow("test-client-id.apps.googleusercontent.com")
        .unwrap();
    let parsed = url::Url::parse(&auth_url).unwrap();
    let state_param = parsed
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .unwrap();
    let redirect_uri = parsed
        .query_pairs()
        .find(|(k, _)| k == "redirect_uri")
        .map(|(_, v)| v.into_owned())
        .unwrap();

    let simulated_cb = format!("{redirect_uri}?code=valid_pkce_code_777&state={state_param}");
    engine.complete_oauth_pkce_blocking(&simulated_cb).unwrap();

    let status = engine.full_status_snapshot().unwrap();
    assert!(status.auth.authenticated);
    assert_eq!(status.compat.android_sdk_int, 21);
    assert!(status.compat.low_ram_optimized);

    // 2. Agent prompt triggers read_file automatically, then pauses before edit_file awaiting approval!
    engine
        .submit_input_blocking("Fix STATE in src/app.rs".to_string())
        .unwrap();

    let snap_pending = engine.terminal_snapshot().unwrap();
    assert!(snap_pending.pending_approval.is_some());
    assert_eq!(
        fs::read_to_string(ws_dir.join("src/app.rs")).unwrap(),
        "pub const STATE: &str = \"broken_value\";\n",
        "File must NOT be modified before user confirms approval"
    );

    // 3. Approve the file edit and verify completion + SAF dirty tracking
    engine.resolve_approval_blocking(true).unwrap();
    let snap_done = engine.terminal_snapshot().unwrap();
    assert!(snap_done.pending_approval.is_none());
    assert!(snap_done.dirty_saf_paths.contains(&"src/app.rs".to_string()));
    assert_eq!(
        fs::read_to_string(ws_dir.join("src/app.rs")).unwrap(),
        "pub const STATE: &str = \"verified_value\";\n"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 3);

    // 4. Test Plan mode + low-memory trim + session persistence across process restart
    engine.set_agent_mode(AgentMode::Plan).unwrap();
    engine.on_trim_memory(15); // TRIM_MEMORY_RUNNING_CRITICAL

    drop(engine);

    let reloaded = AgyEngine::initialize(app_dir.clone(), 21, 1024, vault_key, true).unwrap();
    let reloaded_status = reloaded.full_status_snapshot().unwrap();
    assert!(reloaded_status.auth.authenticated);
    assert_eq!(reloaded_status.config.agent_mode, AgentMode::Plan);
    assert!(reloaded_status.active_session.turn_count >= 2);

    let _ = fs::remove_dir_all(&app_dir);
    drop(server_thread);
}
