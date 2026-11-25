use agy_auth::{scrub_sensitive_text, StoredCredential};
use agy_core::{
    AgentMode, ApprovalKind, MemoryBudget, ModelSpec, PendingApproval, PermissionPolicy, Role,
    SessionStore, TurnMessage,
};
use agy_workspace::{WorkspaceGit, WorkspaceManager};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::watch;

pub enum AgentTurnOutcome {
    Completed {
        assistant_text: String,
    },
    AwaitingApproval {
        partial_text: String,
        approval: PendingApproval,
        pending_tool_call: PendingToolCallState,
    },
}

#[derive(Debug, Clone)]
pub struct PendingToolCallState {
    pub tool_name: String,
    pub args: Value,
    pub prior_contents: Vec<Value>,
}

#[derive(Deserialize)]
struct ModelsListResponse {
    #[serde(default)]
    models: Vec<RemoteModelEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteModelEntry {
    name: String,
    display_name: Option<String>,
    #[serde(default)]
    supported_generation_methods: Vec<String>,
}

pub async fn fetch_compatible_api_models(
    http: &reqwest::Client,
    api_endpoint: &str,
    credential: &StoredCredential,
) -> Result<Vec<ModelSpec>, String> {
    let url = format!("{}/models", api_endpoint.trim_end_matches('/'));
    let req = attach_auth_headers(http.get(&url), credential);
    let resp = req
        .send()
        .await
        .map_err(|e| format!("Network error fetching models: {e}"))?;

    if !resp.status().is_success() {
        let code = resp.status().as_u16();
        let detail = scrub_sensitive_text(&resp.text().await.unwrap_or_default());
        return Err(format!("Model list failed (HTTP {code}): {}", detail.chars().take(180).collect::<String>()));
    }

    let parsed: ModelsListResponse = resp
        .json()
        .await
        .map_err(|e| format!("Invalid models response JSON: {e}"))?;

    let mut out = Vec::new();
    for m in parsed.models {
        if !m
            .supported_generation_methods
            .iter()
            .any(|g| g == "generateContent")
        {
            continue;
        }
        let id = m.name.strip_prefix("models/").unwrap_or(&m.name).to_string();
        out.push(ModelSpec {
            display_name: m.display_name.unwrap_or_else(|| id.clone()),
            id,
            supports_tools: true,
            source: "google-api".to_string(),
        });
    }
    Ok(out)
}

pub async fn run_compatible_agent_turn<F>(
    http: &reqwest::Client,
    api_endpoint: &str,
    credential: &StoredCredential,
    model: &str,
    mode: AgentMode,
    policy: PermissionPolicy,
    budget: MemoryBudget,
    session_id: &str,
    history: &[TurnMessage],
    user_prompt: &str,
    workspace: &mut Option<WorkspaceManager>,
    mut cancel_rx: watch::Receiver<bool>,
    mut on_chunk: F,
) -> Result<AgentTurnOutcome, String>
where
    F: FnMut(&str),
{
    let mut contents = build_conversation_contents(history, user_prompt);
    drive_agent_loop(
        http,
        api_endpoint,
        credential,
        model,
        mode,
        policy,
        budget,
        session_id,
        &mut contents,
        workspace,
        &mut cancel_rx,
        &mut on_chunk,
    )
    .await
}

pub async fn resume_after_approval<F>(
    http: &reqwest::Client,
    api_endpoint: &str,
    credential: &StoredCredential,
    model: &str,
    mode: AgentMode,
    policy: PermissionPolicy,
    budget: MemoryBudget,
    session_id: &str,
    pending: PendingToolCallState,
    approved: bool,
    workspace: &mut Option<WorkspaceManager>,
    mut cancel_rx: watch::Receiver<bool>,
    mut on_chunk: F,
) -> Result<AgentTurnOutcome, String>
where
    F: FnMut(&str),
{
    let mut contents = pending.prior_contents;
    let tool_output = if approved {
        on_chunk(&format!(
            "\x1b[32m[approved]\x1b[0m Executing {}...\n",
            pending.tool_name
        ));
        execute_tool_unconditionally(&pending.tool_name, &pending.args, budget, workspace).await
    } else {
        on_chunk(&format!(
            "\x1b[33m[denied]\x1b[0m Declined {}.\n",
            pending.tool_name
        ));
        json!({ "status": "denied_by_user", "message": "User declined permission for this operation." })
    };

    contents.push(json!({
        "role": "user",
        "parts": [{
            "functionResponse": {
                "name": pending.tool_name,
                "response": tool_output
            }
        }]
    }));

    drive_agent_loop(
        http,
        api_endpoint,
        credential,
        model,
        mode,
        policy,
        budget,
        session_id,
        &mut contents,
        workspace,
        &mut cancel_rx,
        &mut on_chunk,
    )
    .await
}

async fn drive_agent_loop<F>(
    http: &reqwest::Client,
    api_endpoint: &str,
    credential: &StoredCredential,
    model: &str,
    mode: AgentMode,
    policy: PermissionPolicy,
    budget: MemoryBudget,
    session_id: &str,
    contents: &mut Vec<Value>,
    workspace: &mut Option<WorkspaceManager>,
    cancel_rx: &mut watch::Receiver<bool>,
    on_chunk: &mut F,
) -> Result<AgentTurnOutcome, String>
where
    F: FnMut(&str),
{
    let mut full_assistant_text = String::new();
    let ws_label = workspace
        .as_ref()
        .map(|w| w.root().display().to_string())
        .unwrap_or_else(|| "none selected".to_string());

    let system_instruction = match mode {
        AgentMode::Plan => format!(
            "You are AGY Mobile running in PLAN MODE on Android. Active workspace: {ws_label}. Inspect files using read-only tools and produce a concrete, step-by-step engineering plan. Do not attempt to modify files or run commands."
        ),
        AgentMode::ReadOnly => format!(
            "You are AGY Mobile running in READ-ONLY MODE on Android. Active workspace: {ws_label}. Answer accurately using read-only workspace inspection tools."
        ),
        AgentMode::Standard => format!(
            "You are AGY Mobile, a native Rust Android client for Google Antigravity CLI. Active workspace: {ws_label}. Inspect files before editing, keep changes minimal and verified, and respect workspace boundaries."
        ),
    };

    for _step in 0..8 {
        let model_clean = model.trim().strip_prefix("models/").unwrap_or(model.trim());
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            api_endpoint.trim_end_matches('/'),
            model_clean
        );

        let body = json!({
            "systemInstruction": {
                "parts": [{ "text": system_instruction }]
            },
            "contents": contents,
            "tools": [{
                "functionDeclarations": build_tool_declarations(mode)
            }],
            "generationConfig": {
                "temperature": 0.2,
                "maxOutputTokens": 4096
            }
        });

        let req = attach_auth_headers(http.post(&url), credential).json(&body);
        let resp = tokio::select! {
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    return Err("Interrupted by Ctrl+C.".to_string());
                }
                return Err("Cancelled.".to_string());
            }
            r = req.send() => r.map_err(|e| format!("Network disconnection or timeout: {e}"))?,
        };

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let raw_err = scrub_sensitive_text(&resp.text().await.unwrap_or_default());
            if status == 401 || status == 403 {
                return Err(format!(
                    "Authentication rejected (HTTP {status}). Verify your OAuth session or API key in Config. Detail: {}",
                    raw_err.chars().take(200).collect::<String>()
                ));
            }
            return Err(format!(
                "Agent request failed (HTTP {status}): {}",
                raw_err.chars().take(240).collect::<String>()
            ));
        }

        let mut byte_stream = resp.bytes_stream();
        let mut sse_buffer = String::new();
        let mut step_text = String::new();
        let mut function_calls: Vec<(String, Value)> = Vec::new();

        loop {
            tokio::select! {
                _ = cancel_rx.changed() => {
                    if *cancel_rx.borrow() {
                        return Err("Interrupted by Ctrl+C.".to_string());
                    }
                }
                next_item = byte_stream.next() => {
                    let Some(chunk_res) = next_item else {
                        break;
                    };
                    let chunk = chunk_res.map_err(|e| format!("Stream interrupted: {e}"))?;
                    sse_buffer.push_str(&String::from_utf8_lossy(&chunk));

                    while let Some(newline_idx) = sse_buffer.find('\n') {
                        let line = sse_buffer[..newline_idx].trim_end_matches('\r').to_string();
                        sse_buffer.drain(..=newline_idx);
                        if let Some(json_payload) = line.strip_prefix("data:") {
                            let payload = json_payload.trim();
                            if payload.is_empty() || payload == "[DONE]" {
                                continue;
                            }
                            if let Ok(event_val) = serde_json::from_str::<Value>(payload) {
                                parse_sse_candidate_parts(&event_val, &mut step_text, &mut function_calls, on_chunk);
                            }
                        }
                    }
                }
            }
        }

        if !sse_buffer.trim().is_empty() {
            for line in sse_buffer.lines() {
                if let Some(json_payload) = line.trim().strip_prefix("data:") {
                    if let Ok(event_val) = serde_json::from_str::<Value>(json_payload.trim()) {
                        parse_sse_candidate_parts(
                            &event_val,
                            &mut step_text,
                            &mut function_calls,
                            on_chunk,
                        );
                    }
                }
            }
        }

        if !step_text.is_empty() && !step_text.ends_with('\n') {
            on_chunk("\n");
        }
        full_assistant_text.push_str(&step_text);

        if function_calls.is_empty() {
            return Ok(AgentTurnOutcome::Completed {
                assistant_text: full_assistant_text,
            });
        }

        let mut model_parts = Vec::new();
        if !step_text.is_empty() {
            model_parts.push(json!({ "text": step_text }));
        }
        for (fname, fargs) in &function_calls {
            model_parts.push(json!({
                "functionCall": {
                    "name": fname,
                    "args": fargs
                }
            }));
        }
        contents.push(json!({
            "role": "model",
            "parts": model_parts
        }));

        let mut response_parts = Vec::new();
        for (fname, fargs) in function_calls {
            if let Some(approval_kind) =
                check_tool_requires_approval(&fname, &fargs, mode, policy, workspace)?
            {
                let approval_id = format!("ap-{:x}", SessionStore::now_ms() & 0xffff_ffff);
                let summary = describe_approval(&approval_kind);
                on_chunk(&format!(
                    "\x1b[1;33m[APPROVAL REQUIRED: {approval_id}]\x1b[0m {summary}\nRun /approve {approval_id} or /deny {approval_id} (or use the banner controls).\n"
                ));
                let approval = PendingApproval {
                    id: approval_id,
                    session_id: session_id.to_string(),
                    created_at_ms: SessionStore::now_ms(),
                    kind: approval_kind,
                    summary,
                };
                return Ok(AgentTurnOutcome::AwaitingApproval {
                    partial_text: full_assistant_text,
                    approval,
                    pending_tool_call: PendingToolCallState {
                        tool_name: fname,
                        args: fargs,
                        prior_contents: contents.clone(),
                    },
                });
            }

            on_chunk(&format!("\x1b[36m[tool]\x1b[0m {fname}\n"));
            let tool_out =
                execute_tool_unconditionally(&fname, &fargs, budget, workspace).await;
            response_parts.push(json!({
                "functionResponse": {
                    "name": fname,
                    "response": tool_out
                }
            }));
        }

        contents.push(json!({
            "role": "user",
            "parts": response_parts
        }));
    }

    Ok(AgentTurnOutcome::Completed {
        assistant_text: full_assistant_text,
    })
}

fn attach_auth_headers(
    builder: reqwest::RequestBuilder,
    credential: &StoredCredential,
) -> reqwest::RequestBuilder {
    match credential {
        StoredCredential::OAuthTokens { access_token, .. } => {
            builder.bearer_auth(access_token.expose_secret())
        }
        StoredCredential::GeminiApiKey { api_key } => {
            builder.header("x-goog-api-key", api_key.expose_secret())
        }
    }
}

fn build_conversation_contents(history: &[TurnMessage], latest_user_prompt: &str) -> Vec<Value> {
    let mut contents = Vec::new();
    for msg in history {
        if msg.content.trim().is_empty() {
            continue;
        }
        match msg.role {
            Role::User => contents.push(json!({
                "role": "user",
                "parts": [{ "text": msg.content }]
            })),
            Role::Assistant => contents.push(json!({
                "role": "model",
                "parts": [{ "text": msg.content }]
            })),
            Role::System | Role::Tool => {}
        }
    }
    contents.push(json!({
        "role": "user",
        "parts": [{ "text": latest_user_prompt }]
    }));
    contents
}

fn parse_sse_candidate_parts<F>(
    event_val: &Value,
    step_text: &mut String,
    function_calls: &mut Vec<(String, Value)>,
    on_chunk: &mut F,
) where
    F: FnMut(&str),
{
    let Some(parts) = event_val
        .get("candidates")
        .and_then(|c| c.get(0))
        .and_then(|c0| c0.get("content"))
        .and_then(|cnt| cnt.get("parts"))
        .and_then(|p| p.as_array())
    else {
        return;
    };

    for part in parts {
        if let Some(txt) = part.get("text").and_then(|t| t.as_str()) {
            let clean = scrub_sensitive_text(txt);
            step_text.push_str(&clean);
            on_chunk(&clean);
        }
        if let Some(fc) = part.get("functionCall") {
            if let Some(name) = fc.get("name").and_then(|n| n.as_str()) {
                let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                function_calls.push((name.to_string(), args));
            }
        }
    }
}

fn build_tool_declarations(mode: AgentMode) -> Vec<Value> {
    let mut decls = vec![
        json!({
            "name": "list_files",
            "description": "List files and directories inside the active workspace.",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "max_depth": { "type": "INTEGER", "description": "Maximum directory depth (1-6)." }
                }
            }
        }),
        json!({
            "name": "read_file",
            "description": "Stream a line range from a UTF-8 file in the active workspace.",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "path": { "type": "STRING" },
                    "start_line": { "type": "INTEGER" },
                    "end_line": { "type": "INTEGER" }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "search_files",
            "description": "Search workspace text files for a substring.",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "query": { "type": "STRING" }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "git_status",
            "description": "Inspect Git branch and modified/untracked files in the workspace.",
            "parameters": { "type": "OBJECT", "properties": {} }
        }),
        json!({
            "name": "git_diff",
            "description": "Show unified diff for a workspace file.",
            "parameters": {
                "type": "OBJECT",
                "properties": { "path": { "type": "STRING" } },
                "required": ["path"]
            }
        }),
    ];

    if mode == AgentMode::Standard {
        decls.extend([
            json!({
                "name": "write_file",
                "description": "Create or overwrite a UTF-8 file inside the workspace.",
                "parameters": {
                    "type": "OBJECT",
                    "properties": {
                        "path": { "type": "STRING" },
                        "content": { "type": "STRING" }
                    },
                    "required": ["path", "content"]
                }
            }),
            json!({
                "name": "edit_file",
                "description": "Replace an exact snippet in an existing workspace file.",
                "parameters": {
                    "type": "OBJECT",
                    "properties": {
                        "path": { "type": "STRING" },
                        "target": { "type": "STRING" },
                        "replacement": { "type": "STRING" }
                    },
                    "required": ["path", "target", "replacement"]
                }
            }),
            json!({
                "name": "delete_file",
                "description": "Delete a file inside the workspace after user confirmation.",
                "parameters": {
                    "type": "OBJECT",
                    "properties": { "path": { "type": "STRING" } },
                    "required": ["path"]
                }
            }),
            json!({
                "name": "run_command",
                "description": "Run a shell command inside the workspace directory (always gated by user approval).",
                "parameters": {
                    "type": "OBJECT",
                    "properties": { "command": { "type": "STRING" } },
                    "required": ["command"]
                }
            }),
        ]);
    }
    decls
}

fn check_tool_requires_approval(
    tool_name: &str,
    args: &Value,
    mode: AgentMode,
    policy: PermissionPolicy,
    workspace: &Option<WorkspaceManager>,
) -> Result<Option<ApprovalKind>, String> {
    let is_mutating = matches!(
        tool_name,
        "write_file" | "edit_file" | "delete_file" | "run_command"
    );
    if is_mutating && mode != AgentMode::Standard {
        return Err(format!(
            "Tool '{tool_name}' is blocked while {} mode is active.",
            mode.label()
        ));
    }
    if is_mutating && policy == PermissionPolicy::ReadOnlyStrict {
        return Err("Write and command tools are disabled by ReadOnlyStrict policy.".to_string());
    }

    match tool_name {
        "write_file" => {
            let Some(ws) = workspace.as_ref() else {
                return Err("No active workspace selected.".to_string());
            };
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            ws.check_write_approval_needed(policy, path, content.len() as u64)
                .map_err(|e| e.to_string())
        }
        "edit_file" => {
            let Some(ws) = workspace.as_ref() else {
                return Err("No active workspace selected.".to_string());
            };
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let (_, norm) = ws.resolve_safe_path(path).map_err(|e| e.to_string())?;
            let repl_len = args
                .get("replacement")
                .and_then(|v| v.as_str())
                .map(|s| s.len() as u64)
                .unwrap_or(0);
            Ok(Some(ApprovalKind::OverwriteFile {
                relative_path: norm,
                bytes: repl_len,
            }))
        }
        "delete_file" => {
            let Some(ws) = workspace.as_ref() else {
                return Err("No active workspace selected.".to_string());
            };
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let (_, norm) = ws.resolve_safe_path(path).map_err(|e| e.to_string())?;
            Ok(Some(ApprovalKind::DeleteFile {
                relative_path: norm,
            }))
        }
        "run_command" => {
            let cmd = args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let cwd = workspace
                .as_ref()
                .map(|w| w.root().display().to_string())
                .unwrap_or_else(|| ".".to_string());
            Ok(Some(ApprovalKind::ExecuteCommand { command: cmd, cwd }))
        }
        _ => Ok(None),
    }
}

fn describe_approval(kind: &ApprovalKind) -> String {
    match kind {
        ApprovalKind::OverwriteFile {
            relative_path,
            bytes,
        } => format!("Modify existing workspace file '{relative_path}' ({bytes} bytes)"),
        ApprovalKind::DeleteFile { relative_path } => {
            format!("Delete workspace file '{relative_path}'")
        }
        ApprovalKind::ExecuteCommand { command, cwd } => {
            format!("Execute command `{command}` in {cwd}")
        }
    }
}

pub async fn execute_tool_unconditionally(
    tool_name: &str,
    args: &Value,
    budget: MemoryBudget,
    workspace: &mut Option<WorkspaceManager>,
) -> Value {
    let Some(ws) = workspace.as_mut() else {
        return json!({ "error": "No workspace selected. Open a folder in Workspace tab first." });
    };

    match tool_name {
        "list_files" => {
            let depth = args
                .get("max_depth")
                .and_then(|v| v.as_u64())
                .unwrap_or(3) as usize;
            match ws.list_tree(depth, 200) {
                Ok(entries) => json!({ "entries": entries }),
                Err(e) => json!({ "error": e.to_string() }),
            }
        }
        "read_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let start = args
                .get("start_line")
                .and_then(|v| v.as_u64())
                .unwrap_or(1) as usize;
            let end = args
                .get("end_line")
                .and_then(|v| v.as_u64())
                .unwrap_or((start + 119) as u64) as usize;
            match ws.read_lines_streaming(path, start, end, budget.max_file_preview_bytes) {
                Ok(slice) => json!(slice),
                Err(e) => json!({ "error": e.to_string() }),
            }
        }
        "search_files" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            match ws.search_files_streaming(query, budget.max_search_hits) {
                Ok(matches) => json!({ "matches": matches }),
                Err(e) => json!({ "error": e.to_string() }),
            }
        }
        "git_status" => match WorkspaceGit::status(ws.root()) {
            Ok(st) => json!(st),
            Err(e) => json!({ "error": e.to_string() }),
        },
        "git_diff" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            match WorkspaceGit::diff_file(ws.root(), path) {
                Ok(diff) => json!({ "diff": diff }),
                Err(e) => json!({ "error": e.to_string() }),
            }
        }
        "write_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            match ws.write_file_atomic(path, content) {
                Ok(norm) => json!({ "status": "written", "path": norm }),
                Err(e) => json!({ "error": e.to_string() }),
            }
        }
        "edit_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let target = args.get("target").and_then(|v| v.as_str()).unwrap_or("");
            let replacement = args
                .get("replacement")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match ws.edit_file_replace(path, target, replacement) {
                Ok(norm) => json!({ "status": "edited", "path": norm }),
                Err(e) => json!({ "error": e.to_string() }),
            }
        }
        "delete_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            match ws.delete_file(path) {
                Ok(norm) => json!({ "status": "deleted", "path": norm }),
                Err(e) => json!({ "error": e.to_string() }),
            }
        }
        "run_command" => {
            let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            match run_bounded_workspace_command(ws.root(), cmd).await {
                Ok((code, output)) => json!({ "exit_code": code, "output": output }),
                Err(e) => json!({ "error": e }),
            }
        }
        other => json!({ "error": format!("Unsupported tool '{other}'") }),
    }
}

pub async fn run_bounded_workspace_command(
    cwd: &Path,
    command: &str,
) -> Result<(i32, String), String> {
    let shell = if Path::new("/system/bin/sh").exists() {
        "/system/bin/sh"
    } else {
        "sh"
    };
    let child = tokio::process::Command::new(shell)
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .output();

    let out = tokio::time::timeout(std::time::Duration::from_secs(30), child)
        .await
        .map_err(|_| "Command timed out after 30 seconds.".to_string())?
        .map_err(|e| format!("Failed to spawn shell: {e}"))?;

    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&out.stdout));
    if !out.stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    let scrubbed = scrub_sensitive_text(&combined);
    let bounded: String = scrubbed.chars().take(16 * 1024).collect();
    Ok((out.status.code().unwrap_or(-1), bounded))
}

use std::path::Path;
