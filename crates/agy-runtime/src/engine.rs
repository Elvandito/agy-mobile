use crate::api_agent::{
    fetch_compatible_api_models, resume_after_approval, run_bounded_workspace_command,
    run_compatible_agent_turn, AgentTurnOutcome, PendingToolCallState,
};
use crate::native_cli::{
    download_official_arm64_cli, probe_installed_cli, query_native_cli_models, run_native_cli_turn,
};
use agy_auth::{
    await_loopback_redirect_url, bind_loopback_oauth_listener, exchange_pkce_code,
    refresh_oauth_credential, scrub_sensitive_text, AuthStatusSummary, EncryptedTokenStore,
    PkceSession, SecretToken, StoredCredential, GENERATIVE_LANGUAGE_SCOPE, GOOGLE_TOKEN_URL,
};
use agy_core::{
    assess_device_compatibility, AbiTarget, AgentMode, AgyConfig, ApprovalKind,
    CompatibilityReport, DeviceInfo, ExecutionRuntime, MemoryBudget, MemoryPressureLevel,
    ModelSpec, PendingApproval, PermissionPolicy, Role, RuntimeSelection, SessionHeader,
    SessionStore, ThemeMode, TurnMessage,
};
use agy_terminal::{
    parse_terminal_input, AnsiColor, CommandHistory, ParsedTerminalCommand, SignalOutcome,
    SpanStyle, TerminalBuffer, TerminalLine,
};
use agy_workspace::{
    FileReadSlice, GitCommitEntry, GitRepoStatus, SearchMatch, TreeEntry, WorkspaceGit,
    WorkspaceManager,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::runtime::{Builder, Runtime};
use tokio::sync::watch;

enum PendingContinuation {
    AgentTool(PendingToolCallState),
    DirectShellCommand(String),
    DirectFileOverwrite { path: String, content: String },
    DirectFileDelete { path: String },
}

struct SharedState {
    app_data_dir: PathBuf,
    device_info: DeviceInfo,
    budget: MemoryBudget,
    config: AgyConfig,
    compat: CompatibilityReport,
    terminal: TerminalBuffer,
    history: CommandHistory,
    sessions: SessionStore,
    active_session: SessionHeader,
    workspace: Option<WorkspaceManager>,
    vault_key: [u8; 32],
    keystore_backed: bool,
    credential: Option<StoredCredential>,
    pending_pkce: Option<PkceSession>,
    pending_approval: Option<(PendingApproval, PendingContinuation)>,
    cached_models: Vec<ModelSpec>,
    busy: bool,
    status_line: String,
    token_endpoint_override: Option<String>,
}

#[derive(Clone)]
pub struct AgyEngine {
    inner: Arc<Mutex<SharedState>>,
    rt: Arc<Runtime>,
    http: reqwest::Client,
    cancel_tx: Arc<Mutex<watch::Sender<bool>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSnapshot {
    pub lines: Vec<TerminalLine>,
    pub busy: bool,
    pub status_line: String,
    pub runtime_badge: String,
    pub mode_label: String,
    pub model: String,
    pub session_id: String,
    pub session_title: String,
    pub pending_approval: Option<PendingApproval>,
    pub dirty_saf_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullStatusSnapshot {
    pub compat: CompatibilityReport,
    pub auth: AuthStatusSummary,
    pub config: AgyConfig,
    pub active_session: SessionHeader,
    pub sessions: Vec<SessionHeader>,
    pub models: Vec<ModelSpec>,
    pub memory_lines: usize,
    pub memory_bytes: usize,
    pub max_lines: usize,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceOverview {
    pub has_workspace: bool,
    pub root_path: String,
    pub saf_uri: Option<String>,
    pub entries: Vec<TreeEntry>,
    pub git: GitRepoStatus,
    pub commits: Vec<GitCommitEntry>,
}

impl AgyEngine {
    pub fn initialize(
        app_data_dir: PathBuf,
        sdk_int: u32,
        total_ram_mb: u64,
        vault_key: [u8; 32],
        keystore_backed: bool,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(&app_data_dir).map_err(|e| e.to_string())?;
        let budget = MemoryBudget::for_device_ram_mb(total_ram_mb);

        let rt = Builder::new_multi_thread()
            .worker_threads(budget.tokio_worker_threads)
            .thread_stack_size(budget.tokio_stack_bytes)
            .enable_all()
            .build()
            .map_err(|e| format!("Failed to initialize Tokio runtime: {e}"))?;

        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(120))
            .pool_max_idle_per_host(2)
            .user_agent("agy-mobile/0.1.0 (Android; Rust)")
            .build()
            .map_err(|e| format!("Failed to initialize HTTP client: {e}"))?;

        let config_path = app_data_dir.join("config.json");
        let mut config = AgyConfig::load_or_default(&config_path).unwrap_or_default();

        let bin_dir = app_data_dir.join("bin");
        let detected_cli = probe_installed_cli(&bin_dir, sdk_int);
        let device_info = DeviceInfo {
            abi: AbiTarget::current_compiled(),
            android_sdk_int: sdk_int,
            total_ram_mb,
            detected_cli_path: detected_cli.as_ref().map(|d| d.path.clone()),
            detected_cli_version: detected_cli.as_ref().map(|d| d.version.clone()),
        };

        let compat = assess_device_compatibility(
            &device_info,
            config.runtime_selection,
            &config.api_endpoint,
        );

        let sessions_dir = app_data_dir.join("sessions");
        let sessions = SessionStore::new(sessions_dir).map_err(|e| e.to_string())?;
        let existing_sessions = sessions.list_sessions().unwrap_or_default();
        let active_session = if let Some(first) = existing_sessions.first() {
            first.clone()
        } else {
            sessions
                .create_session(
                    "Session 1",
                    &config.selected_model,
                    config.agent_mode,
                    config
                        .active_workspace
                        .as_ref()
                        .map(|p| p.display().to_string()),
                )
                .map_err(|e| e.to_string())?
        };

        let workspace = if let Some(ws_path) = &config.active_workspace {
            WorkspaceManager::open(ws_path).ok()
        } else {
            let default_ws = app_data_dir.join("workspaces").join("default");
            let mgr = WorkspaceManager::open(&default_ws).ok();
            if mgr.is_some() {
                config.active_workspace = Some(default_ws);
                let _ = config.save_atomic(&config_path);
            }
            mgr
        };

        let cred_path = app_data_dir.join("credentials.enc");
        let credential = EncryptedTokenStore::load_sealed(&cred_path, &vault_key)
            .ok()
            .flatten();

        let mut terminal = TerminalBuffer::new(budget);
        print_boot_banner(&mut terminal, &compat, credential.is_some());
        if let Ok(prior_turns) = sessions.load_recent_turns(&active_session.id, 12) {
            for t in prior_turns {
                let prefix = match t.role {
                    Role::User => "\x1b[1;33magy>\x1b[0m ",
                    Role::Assistant => "",
                    Role::System | Role::Tool => "\x1b[2m[sys]\x1b[0m ",
                };
                terminal.feed_stream_chunk(&format!("{prefix}{}\n", t.content.trim_end()));
            }
        }

        let history =
            CommandHistory::load_from_file(&app_data_dir.join("history.json"), 120);
        let (cancel_tx, _) = watch::channel(false);

        let state = SharedState {
            app_data_dir,
            device_info,
            budget,
            cached_models: AgyConfig::builtin_compat_models(),
            config,
            compat,
            terminal,
            history,
            sessions,
            active_session,
            workspace,
            vault_key,
            keystore_backed,
            credential,
            pending_pkce: None,
            pending_approval: None,
            busy: false,
            status_line: "Ready. Type /help for commands or enter an agent prompt.".to_string(),
            token_endpoint_override: None,
        };

        Ok(Self {
            inner: Arc::new(Mutex::new(state)),
            rt: Arc::new(rt),
            http,
            cancel_tx: Arc::new(Mutex::new(cancel_tx)),
        })
    }

    pub fn set_token_endpoint_for_testing(&self, url: String) {
        if let Ok(mut st) = self.inner.lock() {
            st.token_endpoint_override = Some(url);
        }
    }

    pub fn set_api_endpoint(&self, endpoint: &str) -> Result<(), String> {
        let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        st.config.api_endpoint = endpoint.trim().trim_end_matches('/').to_string();
        st.compat = assess_device_compatibility(
            &st.device_info,
            st.config.runtime_selection,
            &st.config.api_endpoint,
        );
        let cfg_path = st.app_data_dir.join("config.json");
        st.config.save_atomic(&cfg_path).map_err(|e| e.to_string())
    }

    pub fn submit_input_async(&self, raw_input: String) {
        let engine = self.clone();
        self.rt.spawn(async move {
            let _ = engine.execute_input(raw_input).await;
        });
    }

    pub fn submit_input_blocking(&self, raw_input: String) -> Result<(), String> {
        let engine = self.clone();
        self.rt.block_on(async move { engine.execute_input(raw_input).await })
    }

    pub async fn execute_input(&self, raw_input: String) -> Result<(), String> {
        let Some(parsed) = parse_terminal_input(&raw_input) else {
            return Ok(());
        };

        {
            let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
            st.terminal.reset_ctrl_d_counter();
            st.history.record(&raw_input);
            let hist_path = st.app_data_dir.join("history.json");
            let _ = st.history.save_to_file(&hist_path);
            st.terminal.push_styled_line(
                &format!("agy> {}", scrub_sensitive_text(raw_input.trim())),
                SpanStyle {
                    fg: AnsiColor::Amber,
                    bold: true,
                    underline: false,
                },
            );
        }

        match parsed {
            ParsedTerminalCommand::Help => {
                self.append_line(
                    "Commands:\n  /status          Show runtime, ABI, auth, workspace, and session state\n  /runtime         Explain Native CLI vs Rust API Compatibility mode on this device\n  /models          Query available models from active runtime\n  /model <id>      Switch active model\n  /mode <m>        Set mode: standard | plan | readonly (alias: /plan)\n  /sessions        List saved sessions\n  /new [title]     Start a new persistent session\n  /resume <id>     Switch to an existing session\n  /tree [depth]    Display workspace file tree\n  /search <query>  Stream-search files in workspace\n  /cat <file> [s] [e] Stream lines from a workspace file\n  /git [status|log|diff <file>] Inspect Git state (works offline without system git)\n  /approve | /deny Confirm or reject a pending write or command approval\n  /mem             Show bounded terminal & 1 GB RAM budget telemetry\n  /clear           Clear scrollback buffer\n  /exit            End current session and rotate to a fresh prompt\n  !<cmd>           Request permission to run a shell command inside workspace",
                    AnsiColor::Default,
                );
            }
            ParsedTerminalCommand::Clear => {
                if let Ok(mut st) = self.inner.lock() {
                    st.terminal.clear();
                    st.status_line = "Terminal scrollback cleared.".to_string();
                }
            }
            ParsedTerminalCommand::Exit => {
                self.rotate_to_fresh_session("New session")?;
                self.append_line("Closed previous session and opened a clean session.", AnsiColor::Muted);
            }
            ParsedTerminalCommand::Status => {
                let snap = self.full_status_snapshot()?;
                let ws_str = snap
                    .config
                    .active_workspace
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "none".to_string());
                let text = format!(
                    "Runtime : {}\nABI     : {} ({}, {}-bit, SDK {})\nAuth    : {} (Keystore: {})\nModel   : {} | Mode: {}\nSession : {} ({} turns)\nWorkdir : {}",
                    snap.compat.active_runtime.badge_label(),
                    snap.compat.android_abi,
                    snap.compat.rust_triple,
                    snap.compat.pointer_bits,
                    snap.compat.android_sdk_int,
                    snap.auth.method,
                    if snap.auth.keystore_protected { "yes" } else { "fallback" },
                    snap.config.selected_model,
                    snap.config.agent_mode.label(),
                    snap.active_session.id,
                    snap.active_session.turn_count,
                    ws_str
                );
                self.append_line(&text, AnsiColor::Default);
            }
            ParsedTerminalCommand::RuntimeInfo => {
                let snap = self.full_status_snapshot()?;
                let detail = match &snap.compat.active_runtime {
                    ExecutionRuntime::NativeCli {
                        binary_path,
                        version,
                    } => format!(
                        "Active: Native CLI ({version}) at {}\nIPC: agy --output-format stream-json",
                        binary_path.display()
                    ),
                    ExecutionRuntime::CompatibleApi {
                        endpoint,
                        fallback_reason,
                    } => format!(
                        "Active: Pure-Rust API Compatibility Mode\nEndpoint: {endpoint}\nWhy: {fallback_reason}"
                    ),
                };
                self.append_line(&detail, AnsiColor::Cyan);
            }
            ParsedTerminalCommand::MemoryStats => {
                let st = self.inner.lock().map_err(|_| "Lock poisoned")?;
                let (lines, bytes, max_l, max_b) = st.terminal.stats();
                let msg = format!(
                    "Device RAM: {} MB | Profile: {:?}\nScrollback: {}/{} lines, {}/{} bytes\nTokio workers: {} (stack {} KB)",
                    st.device_info.total_ram_mb,
                    st.budget.profile,
                    lines,
                    max_l,
                    bytes,
                    max_b,
                    st.budget.tokio_worker_threads,
                    st.budget.tokio_stack_bytes / 1024
                );
                drop(st);
                self.append_line(&msg, AnsiColor::Default);
            }
            ParsedTerminalCommand::ListModels => {
                self.refresh_models_async_inner().await;
            }
            ParsedTerminalCommand::SetModel(m) => {
                self.set_selected_model(&m)?;
                self.append_line(&format!("Active model set to '{m}'."), AnsiColor::Green);
            }
            ParsedTerminalCommand::SetMode(m) => {
                let next = match m.to_ascii_lowercase().as_str() {
                    "plan" => AgentMode::Plan,
                    "readonly" | "read-only" | "ro" => AgentMode::ReadOnly,
                    "standard" | "auto" | "" => AgentMode::Standard,
                    other => {
                        self.append_line(
                            &format!("Unknown mode '{other}'. Valid modes: standard, plan, readonly."),
                            AnsiColor::Red,
                        );
                        return Ok(());
                    }
                };
                self.set_agent_mode(next)?;
                self.append_line(
                    &format!("Agent execution mode switched to {}.", next.label()),
                    AnsiColor::Green,
                );
            }
            ParsedTerminalCommand::ListSessions => {
                let st = self.inner.lock().map_err(|_| "Lock poisoned")?;
                let list = st.sessions.list_sessions().unwrap_or_default();
                let current_id = st.active_session.id.clone();
                drop(st);
                if list.is_empty() {
                    self.append_line("No saved sessions yet.", AnsiColor::Muted);
                } else {
                    let mut out = String::from("Saved sessions:");
                    for s in list.iter().take(20) {
                        let marker = if s.id == current_id { "*" } else { " " };
                        out.push_str(&format!(
                            "\n {marker} {} | {:>3} turns | {} | {}",
                            s.id,
                            s.turn_count,
                            s.mode.label(),
                            s.title
                        ));
                    }
                    self.append_line(&out, AnsiColor::Default);
                }
            }
            ParsedTerminalCommand::NewSession(title) => {
                let header = self.rotate_to_fresh_session(title.as_deref().unwrap_or("Terminal session"))?;
                self.append_line(
                    &format!("Started session {} ({}).", header.id, header.title),
                    AnsiColor::Green,
                );
            }
            ParsedTerminalCommand::ResumeSession(id) => {
                if id.is_empty() {
                    self.append_line("Usage: /resume <session-id>", AnsiColor::Red);
                } else {
                    match self.switch_session(&id) {
                        Ok(h) => self.append_line(
                            &format!("Resumed session {} ({}).", h.id, h.title),
                            AnsiColor::Green,
                        ),
                        Err(e) => self.append_line(&e, AnsiColor::Red),
                    }
                }
            }
            ParsedTerminalCommand::WorkspaceTree(depth) => {
                let res = {
                    let st = self.inner.lock().map_err(|_| "Lock poisoned")?;
                    match &st.workspace {
                        Some(ws) => ws.list_tree(depth, 150).map_err(|e| e.to_string()),
                        None => Err("No workspace open.".to_string()),
                    }
                };
                match res {
                    Ok(entries) if entries.is_empty() => {
                        self.append_line("(Workspace directory is empty)", AnsiColor::Muted);
                    }
                    Ok(entries) => {
                        let mut buf = String::new();
                        for e in entries {
                            let indent = "  ".repeat(e.depth);
                            let suffix = if e.is_dir { "/" } else { "" };
                            if !buf.is_empty() {
                                buf.push('\n');
                            }
                            buf.push_str(&format!("{indent}{}{suffix}", e.name));
                        }
                        self.append_line(&buf, AnsiColor::Default);
                    }
                    Err(e) => self.append_line(&e, AnsiColor::Red),
                }
            }
            ParsedTerminalCommand::WorkspaceSearch(query) => {
                if query.is_empty() {
                    self.append_line("Usage: /search <substring>", AnsiColor::Red);
                    return Ok(());
                }
                let res = {
                    let st = self.inner.lock().map_err(|_| "Lock poisoned")?;
                    let max_hits = st.budget.max_search_hits;
                    match &st.workspace {
                        Some(ws) => ws
                            .search_files_streaming(&query, max_hits)
                            .map_err(|e| e.to_string()),
                        None => Err("No workspace open.".to_string()),
                    }
                };
                match res {
                    Ok(hits) if hits.is_empty() => {
                        self.append_line(&format!("No matches found for '{query}'."), AnsiColor::Muted);
                    }
                    Ok(hits) => {
                        let rendered = hits
                            .iter()
                            .map(|h| format!("{}:{}: {}", h.relative_path, h.line_number, h.preview))
                            .collect::<Vec<_>>()
                            .join("\n");
                        self.append_line(&rendered, AnsiColor::Default);
                    }
                    Err(e) => self.append_line(&e, AnsiColor::Red),
                }
            }
            ParsedTerminalCommand::CatFile {
                path,
                start_line,
                end_line,
            } => {
                if path.is_empty() {
                    self.append_line("Usage: /cat <relative-path> [start] [end]", AnsiColor::Red);
                    return Ok(());
                }
                match self.workspace_read_file(&path, start_line, end_line) {
                    Ok(slice) => {
                        let hdr = format!(
                            "--- {} (lines {}-{} of {}) ---",
                            slice.relative_path, slice.start_line, slice.end_line, slice.total_lines
                        );
                        self.append_line(&hdr, AnsiColor::Cyan);
                        self.append_line(&slice.content, AnsiColor::Default);
                    }
                    Err(e) => self.append_line(&e, AnsiColor::Red),
                }
            }
            ParsedTerminalCommand::GitSubcommand(sub) => {
                let mut parts = sub.splitn(2, char::is_whitespace);
                let verb = parts.next().unwrap_or("status");
                let arg = parts.next().unwrap_or("").trim();
                let root_opt = {
                    let st = self.inner.lock().map_err(|_| "Lock poisoned")?;
                    st.workspace.as_ref().map(|w| w.root().to_path_buf())
                };
                let Some(root) = root_opt else {
                    self.append_line("No workspace open.", AnsiColor::Red);
                    return Ok(());
                };
                match verb {
                    "status" | "branch" => match WorkspaceGit::status(&root) {
                        Ok(st) if !st.is_repo => {
                            self.append_line("Workspace is not a Git repository (.git not found).", AnsiColor::Muted);
                        }
                        Ok(st) => {
                            let mut out = format!(
                                "Branch: {} ({}) [backend: {}]",
                                st.branch,
                                st.head_commit
                                    .as_deref()
                                    .map(|h| &h[..h.len().min(7)])
                                    .unwrap_or("unborn"),
                                st.backend
                            );
                            if st.changes.is_empty() {
                                out.push_str("\nWorking tree clean.");
                            } else {
                                for ch in st.changes {
                                    out.push_str(&format!("\n {:>2} {}", ch.status, ch.path));
                                }
                            }
                            self.append_line(&out, AnsiColor::Default);
                        }
                        Err(e) => self.append_line(&e.to_string(), AnsiColor::Red),
                    },
                    "log" => match WorkspaceGit::log(&root, 12) {
                        Ok(commits) if commits.is_empty() => {
                            self.append_line("No reachable commits found.", AnsiColor::Muted);
                        }
                        Ok(commits) => {
                            let out = commits
                                .iter()
                                .map(|c| format!("{} {} ({})", c.short_id, c.summary, c.author))
                                .collect::<Vec<_>>()
                                .join("\n");
                            self.append_line(&out, AnsiColor::Default);
                        }
                        Err(e) => self.append_line(&e.to_string(), AnsiColor::Red),
                    },
                    "diff" => {
                        if arg.is_empty() {
                            self.append_line("Usage: /git diff <relative-path>", AnsiColor::Red);
                        } else {
                            match WorkspaceGit::diff_file(&root, arg) {
                                Ok(diff) => self.append_line(&diff, AnsiColor::Default),
                                Err(e) => self.append_line(&e.to_string(), AnsiColor::Red),
                            }
                        }
                    }
                    other => self.append_line(
                        &format!("Unsupported /git subcommand '{other}'. Try: status, log, diff <path>."),
                        AnsiColor::Red,
                    ),
                }
            }
            ParsedTerminalCommand::ShellCommand(cmd) => {
                self.queue_direct_shell_approval(cmd)?;
            }
            ParsedTerminalCommand::Approve(id_opt) => {
                self.resolve_pending_approval_inner(id_opt.as_deref(), true).await?;
            }
            ParsedTerminalCommand::Deny(id_opt) => {
                self.resolve_pending_approval_inner(id_opt.as_deref(), false).await?;
            }
            ParsedTerminalCommand::AgentPrompt(prompt) => {
                self.run_agent_prompt_inner(prompt).await?;
            }
        }
        Ok(())
    }

    fn queue_direct_shell_approval(&self, cmd: String) -> Result<(), String> {
        let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        if st.config.agent_mode != AgentMode::Standard
            || st.config.permission_policy == PermissionPolicy::ReadOnlyStrict
        {
            st.terminal.push_styled_line(
                "Shell execution is blocked while Plan or Read-Only mode is active.",
                SpanStyle {
                    fg: AnsiColor::Red,
                    bold: false,
                    underline: false,
                },
            );
            return Ok(());
        }
        let cwd = st
            .workspace
            .as_ref()
            .map(|w| w.root().display().to_string())
            .unwrap_or_else(|| st.app_data_dir.display().to_string());
        let id = format!("ap-{:x}", SessionStore::now_ms() & 0xffff_ffff);
        let kind = ApprovalKind::ExecuteCommand {
            command: cmd.clone(),
            cwd: cwd.clone(),
        };
        let summary = format!("Run `{cmd}` inside {cwd}");
        let approval = PendingApproval {
            id: id.clone(),
            session_id: st.active_session.id.clone(),
            created_at_ms: SessionStore::now_ms(),
            kind,
            summary: summary.clone(),
        };
        st.pending_approval = Some((approval, PendingContinuation::DirectShellCommand(cmd)));
        st.terminal.push_styled_line(
            &format!("[APPROVAL REQUIRED: {id}] {summary}\nConfirm via banner or type /approve {id} (or /deny {id})."),
            SpanStyle {
                fg: AnsiColor::Amber,
                bold: true,
                underline: false,
            },
        );
        st.status_line = "Awaiting command permission confirmation.".to_string();
        Ok(())
    }

    pub fn resolve_approval_async(&self, approved: bool) {
        let engine = self.clone();
        self.rt.spawn(async move {
            let _ = engine.resolve_pending_approval_inner(None, approved).await;
        });
    }

    pub fn resolve_approval_blocking(&self, approved: bool) -> Result<(), String> {
        let engine = self.clone();
        self.rt
            .block_on(async move { engine.resolve_pending_approval_inner(None, approved).await })
    }

    async fn resolve_pending_approval_inner(
        &self,
        expected_id: Option<&str>,
        approved: bool,
    ) -> Result<(), String> {
        let taken = {
            let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
            let Some((ap, cont)) = st.pending_approval.take() else {
                st.terminal.push_styled_line(
                    "No pending permission request.",
                    SpanStyle {
                        fg: AnsiColor::Muted,
                        bold: false,
                        underline: false,
                    },
                );
                return Ok(());
            };
            if let Some(exp) = expected_id {
                if !exp.is_empty() && exp != ap.id {
                    st.pending_approval = Some((ap, cont));
                    return Err(format!("Approval ID '{exp}' did not match active request."));
                }
            }
            (ap, cont)
        };

        match taken.1 {
            PendingContinuation::DirectShellCommand(cmd) => {
                if !approved {
                    self.append_line("Command cancelled by user.", AnsiColor::Amber);
                    return Ok(());
                }
                let cwd = {
                    let st = self.inner.lock().map_err(|_| "Lock poisoned")?;
                    st.workspace
                        .as_ref()
                        .map(|w| w.root().to_path_buf())
                        .unwrap_or_else(|| st.app_data_dir.clone())
                };
                self.set_busy(true, "Running approved workspace command...");
                let res = run_bounded_workspace_command(&cwd, &cmd).await;
                self.set_busy(false, "Ready.");
                match res {
                    Ok((code, out)) => {
                        if !out.trim().is_empty() {
                            self.append_line(out.trim_end(), AnsiColor::Default);
                        }
                        self.append_line(
                            &format!("[exit {code}]"),
                            if code == 0 {
                                AnsiColor::Green
                            } else {
                                AnsiColor::Red
                            },
                        );
                    }
                    Err(e) => self.append_line(&e, AnsiColor::Red),
                }
            }
            PendingContinuation::DirectFileOverwrite { path, content } => {
                if !approved {
                    self.append_line(&format!("Cancelled overwrite of '{path}'."), AnsiColor::Amber);
                    return Ok(());
                }
                let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
                if let Some(ws) = st.workspace.as_mut() {
                    match ws.write_file_atomic(&path, &content) {
                        Ok(norm) => st.terminal.push_styled_line(
                            &format!("Saved workspace file '{norm}'."),
                            SpanStyle {
                                fg: AnsiColor::Green,
                                bold: false,
                                underline: false,
                            },
                        ),
                        Err(e) => st.terminal.push_styled_line(
                            &e.to_string(),
                            SpanStyle {
                                fg: AnsiColor::Red,
                                bold: false,
                                underline: false,
                            },
                        ),
                    }
                }
            }
            PendingContinuation::DirectFileDelete { path } => {
                if !approved {
                    self.append_line(&format!("Cancelled deletion of '{path}'."), AnsiColor::Amber);
                    return Ok(());
                }
                let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
                if let Some(ws) = st.workspace.as_mut() {
                    match ws.delete_file(&path) {
                        Ok(norm) => st.terminal.push_styled_line(
                            &format!("Deleted workspace file '{norm}'."),
                            SpanStyle {
                                fg: AnsiColor::Green,
                                bold: false,
                                underline: false,
                            },
                        ),
                        Err(e) => st.terminal.push_styled_line(
                            &e.to_string(),
                            SpanStyle {
                                fg: AnsiColor::Red,
                                bold: false,
                                underline: false,
                            },
                        ),
                    }
                }
            }
            PendingContinuation::AgentTool(pending_tool) => {
                let (endpoint, cred, model, mode, policy, budget, session_id, mut ws_clone) = {
                    let st = self.inner.lock().map_err(|_| "Lock poisoned")?;
                    let Some(cred) = st.credential.clone() else {
                        return Err("Not authenticated.".to_string());
                    };
                    (
                        st.config.api_endpoint.clone(),
                        cred,
                        st.config.selected_model.clone(),
                        st.config.agent_mode,
                        st.config.permission_policy,
                        st.budget,
                        st.active_session.id.clone(),
                        st.workspace.clone(),
                    )
                };
                let cancel_rx = self.reset_cancel_channel();
                self.set_busy(true, "Resuming agent turn...");
                let outcome = resume_after_approval(
                    &self.http,
                    &endpoint,
                    &cred,
                    &model,
                    mode,
                    policy,
                    budget,
                    &session_id,
                    pending_tool,
                    approved,
                    &mut ws_clone,
                    cancel_rx,
                    |chunk| self.feed_stream(chunk),
                )
                .await;
                self.finish_agent_outcome(outcome, ws_clone);
            }
        }
        Ok(())
    }

    async fn run_agent_prompt_inner(&self, prompt: String) -> Result<(), String> {
        let (runtime, model, mode, policy, budget, session_id, history, ws_clone) = {
            let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
            let max_turns = st.budget.max_in_memory_turns;
            let prior = st
                .sessions
                .load_recent_turns(&st.active_session.id, max_turns)
                .unwrap_or_default();
            let user_turn = TurnMessage {
                timestamp_ms: SessionStore::now_ms(),
                role: Role::User,
                content: prompt.clone(),
                tool_calls: Vec::new(),
            };
            if let Ok(updated) = st.sessions.append_turn(&st.active_session.id, &user_turn) {
                st.active_session = updated;
            }
            (
                st.compat.active_runtime.clone(),
                st.config.selected_model.clone(),
                st.config.agent_mode,
                st.config.permission_policy,
                st.budget,
                st.active_session.id.clone(),
                prior,
                st.workspace.clone(),
            )
        };

        let cancel_rx = self.reset_cancel_channel();

        match runtime {
            ExecutionRuntime::NativeCli { binary_path, .. } => {
                self.set_busy(true, "Streaming from native agy CLI...");
                let ws_dir = ws_clone.as_ref().map(|w| w.root().to_path_buf());
                let res = run_native_cli_turn(
                    &binary_path,
                    ws_dir.as_deref(),
                    &prompt,
                    &model,
                    mode,
                    cancel_rx,
                    |chunk| self.feed_stream(chunk),
                )
                .await;
                self.set_busy(false, "Ready.");
                match res {
                    Ok(reply) => {
                        self.record_assistant_turn(&session_id, &reply);
                    }
                    Err(e) => {
                        self.append_line(&format!("[ERROR] {e}"), AnsiColor::Red);
                        if let Ok(mut st) = self.inner.lock() {
                            st.status_line = e;
                        }
                    }
                }
            }
            ExecutionRuntime::CompatibleApi { endpoint, .. } => {
                let cred = match self.ensure_fresh_credential().await {
                    Ok(c) => c,
                    Err(e) => {
                        self.append_line(
                            &format!("[AUTH REQUIRED] {e}\nOpen Config to sign in with Google OAuth PKCE or save a Gemini API key."),
                            AnsiColor::Amber,
                        );
                        if let Ok(mut st) = self.inner.lock() {
                            st.status_line = "Authentication required.".to_string();
                        }
                        return Ok(());
                    }
                };

                self.set_busy(true, "Streaming from Google Gemini API compatibility runtime...");
                let mut ws_mut = ws_clone;
                let outcome = run_compatible_agent_turn(
                    &self.http,
                    &endpoint,
                    &cred,
                    &model,
                    mode,
                    policy,
                    budget,
                    &session_id,
                    &history,
                    &prompt,
                    &mut ws_mut,
                    cancel_rx,
                    |chunk| self.feed_stream(chunk),
                )
                .await;
                self.finish_agent_outcome(outcome, ws_mut);
            }
        }
        Ok(())
    }

    fn finish_agent_outcome(
        &self,
        outcome: Result<AgentTurnOutcome, String>,
        updated_ws: Option<WorkspaceManager>,
    ) {
        let mut st = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        st.busy = false;
        st.terminal.flush_partial_if_nonempty();
        if let Some(ws) = updated_ws {
            st.workspace = Some(ws);
        }

        match outcome {
            Ok(AgentTurnOutcome::Completed { assistant_text }) => {
                st.status_line = "Ready.".to_string();
                if !assistant_text.trim().is_empty() {
                    let turn = TurnMessage {
                        timestamp_ms: SessionStore::now_ms(),
                        role: Role::Assistant,
                        content: assistant_text,
                        tool_calls: Vec::new(),
                    };
                    if let Ok(hdr) = st.sessions.append_turn(&st.active_session.id, &turn) {
                        st.active_session = hdr;
                    }
                }
            }
            Ok(AgentTurnOutcome::AwaitingApproval {
                partial_text,
                approval,
                pending_tool_call,
            }) => {
                st.status_line = format!("Approval required: {}", approval.summary);
                if !partial_text.trim().is_empty() {
                    let turn = TurnMessage {
                        timestamp_ms: SessionStore::now_ms(),
                        role: Role::Assistant,
                        content: partial_text,
                        tool_calls: Vec::new(),
                    };
                    if let Ok(hdr) = st.sessions.append_turn(&st.active_session.id, &turn) {
                        st.active_session = hdr;
                    }
                }
                st.pending_approval =
                    Some((approval, PendingContinuation::AgentTool(pending_tool_call)));
            }
            Err(e) => {
                st.status_line = e.clone();
                st.terminal.push_styled_line(
                    &format!("[ERROR] {e}"),
                    SpanStyle {
                        fg: AnsiColor::Red,
                        bold: false,
                        underline: false,
                    },
                );
            }
        }
    }

    async fn ensure_fresh_credential(&self) -> Result<StoredCredential, String> {
        let (cred_opt, token_ep) = {
            let st = self.inner.lock().map_err(|_| "Lock poisoned")?;
            (
                st.credential.clone(),
                st.token_endpoint_override
                    .clone()
                    .unwrap_or_else(|| GOOGLE_TOKEN_URL.to_string()),
            )
        };
        let Some(cred) = cred_opt else {
            return Err("No OAuth session or API key stored.".to_string());
        };

        if let StoredCredential::OAuthTokens {
            client_id,
            refresh_token: Some(rt_tok),
            expires_at_ms: Some(exp),
            ..
        } = &cred
        {
            if SessionStore::now_ms() + 60_000 >= *exp {
                let refreshed =
                    refresh_oauth_credential(&self.http, &token_ep, client_id, rt_tok)
                        .await
                        .map_err(|e| e.to_string())?;
                let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
                let cred_path = st.app_data_dir.join("credentials.enc");
                let _ = EncryptedTokenStore::save_sealed(&cred_path, &st.vault_key, &refreshed);
                st.credential = Some(refreshed.clone());
                return Ok(refreshed);
            }
        }
        Ok(cred)
    }

    pub fn send_ctrl_c(&self) -> SignalOutcome {
        if let Ok(tx) = self.cancel_tx.lock() {
            let _ = tx.send(true);
        }
        let mut st = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return SignalOutcome::IdleInterruptNotice,
        };
        st.terminal.reset_ctrl_d_counter();
        if st.busy {
            st.terminal.push_styled_line(
                "^C Interrupt signal sent.",
                SpanStyle {
                    fg: AnsiColor::Amber,
                    bold: true,
                    underline: false,
                },
            );
            st.status_line = "Cancelling active turn...".to_string();
            SignalOutcome::CancelledActiveTask
        } else {
            st.terminal.push_styled_line(
                "^C",
                SpanStyle {
                    fg: AnsiColor::Muted,
                    bold: false,
                    underline: false,
                },
            );
            SignalOutcome::IdleInterruptNotice
        }
    }

    pub fn send_ctrl_d(&self, current_input_empty: bool) -> SignalOutcome {
        let outcome = {
            let Ok(mut st) = self.inner.lock() else {
                return SignalOutcome::ClearInputBuffer;
            };
            st.terminal.handle_ctrl_d(current_input_empty)
        };
        if outcome == SignalOutcome::ExitSession {
            let _ = self.rotate_to_fresh_session("New session");
            self.append_line("[EOF] Session closed. Ready in new session.", AnsiColor::Muted);
        }
        outcome
    }

    pub fn history_previous(&self) -> Option<String> {
        let mut st = self.inner.lock().ok()?;
        st.history.previous().map(ToOwned::to_owned)
    }

    pub fn history_next(&self) -> Option<String> {
        let mut st = self.inner.lock().ok()?;
        st.history.next().map(ToOwned::to_owned)
    }

    pub fn on_trim_memory(&self, android_level: i32) {
        let pressure = MemoryPressureLevel::from_android_trim_level(android_level);
        if let Ok(mut st) = self.inner.lock() {
            st.terminal.apply_memory_pressure(pressure);
            st.status_line = format!("Memory trim applied ({pressure:?}).");
        }
    }

    pub fn save_api_key(&self, raw_key: &str) -> Result<(), String> {
        let trimmed = raw_key.trim();
        if trimmed.is_empty() {
            return Err("API key cannot be empty.".to_string());
        }
        let cred = StoredCredential::GeminiApiKey {
            api_key: SecretToken::new(trimmed),
        };
        let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        let cred_path = st.app_data_dir.join("credentials.enc");
        EncryptedTokenStore::save_sealed(&cred_path, &st.vault_key, &cred)
            .map_err(|e| e.to_string())?;
        st.credential = Some(cred);
        st.status_line = "Saved encrypted API credential.".to_string();
        st.terminal.push_styled_line(
            "[auth] Encrypted Gemini API key stored in Android Keystore vault.",
            SpanStyle {
                fg: AnsiColor::Green,
                bold: false,
                underline: false,
            },
        );
        Ok(())
    }

    pub fn start_oauth_pkce_flow(&self, client_id: &str) -> Result<String, String> {
        let trimmed = client_id.trim();
        if trimmed.is_empty() {
            return Err("Enter your Google OAuth 2.0 Client ID first.".to_string());
        }
        let (port, listener) = self
            .rt
            .block_on(bind_loopback_oauth_listener())
            .map_err(|e| e.to_string())?;
        let redirect_uri = format!("http://127.0.0.1:{port}/oauth2callback");
        let pkce = PkceSession::generate(trimmed, &redirect_uri, GENERATIVE_LANGUAGE_SCOPE)
            .map_err(|e| e.to_string())?;
        let auth_url = pkce.authorization_url.clone();

        {
            let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
            st.config.oauth_client_id = Some(trimmed.to_string());
            let cfg_path = st.app_data_dir.join("config.json");
            let _ = st.config.save_atomic(&cfg_path);
            st.pending_pkce = Some(pkce);
            st.status_line =
                "Browser OAuth PKCE started. Complete sign-in or paste redirect URL.".to_string();
        }

        let engine = self.clone();
        self.rt.spawn(async move {
            if let Ok(Ok(callback_url)) =
                tokio::time::timeout(std::time::Duration::from_secs(180), await_loopback_redirect_url(listener)).await
            {
                let _ = engine.complete_oauth_pkce_inner(&callback_url).await;
            }
        });

        Ok(auth_url)
    }

    pub fn complete_oauth_pkce_blocking(&self, callback_or_code: &str) -> Result<(), String> {
        let input = callback_or_code.to_string();
        let engine = self.clone();
        self.rt
            .block_on(async move { engine.complete_oauth_pkce_inner(&input).await })
    }

    async fn complete_oauth_pkce_inner(&self, callback_or_code: &str) -> Result<(), String> {
        let (pkce, token_ep) = {
            let st = self.inner.lock().map_err(|_| "Lock poisoned")?;
            let pkce = st
                .pending_pkce
                .clone()
                .ok_or_else(|| "No pending OAuth PKCE flow. Start OAuth sign-in first.".to_string())?;
            let ep = st
                .token_endpoint_override
                .clone()
                .unwrap_or_else(|| GOOGLE_TOKEN_URL.to_string());
            (pkce, ep)
        };

        let code = pkce
            .verify_and_extract_code(callback_or_code)
            .map_err(|e| e.to_string())?;
        let cred = exchange_pkce_code(&self.http, &token_ep, &pkce, &code)
            .await
            .map_err(|e| e.to_string())?;

        let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        let cred_path = st.app_data_dir.join("credentials.enc");
        EncryptedTokenStore::save_sealed(&cred_path, &st.vault_key, &cred)
            .map_err(|e| e.to_string())?;
        st.credential = Some(cred);
        st.pending_pkce = None;
        st.status_line = "Google OAuth PKCE sign-in succeeded.".to_string();
        st.terminal.push_styled_line(
            "[auth] Google OAuth 2.0 PKCE token sealed in Android Keystore vault.",
            SpanStyle {
                fg: AnsiColor::Green,
                bold: false,
                underline: false,
            },
        );
        Ok(())
    }

    pub fn sign_out(&self) -> Result<(), String> {
        let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        let cred_path = st.app_data_dir.join("credentials.enc");
        EncryptedTokenStore::clear(&cred_path).map_err(|e| e.to_string())?;
        st.credential = None;
        st.pending_pkce = None;
        st.status_line = "Signed out and cleared stored token.".to_string();
        Ok(())
    }

    pub fn set_selected_model(&self, model_id: &str) -> Result<(), String> {
        let trimmed = model_id.trim();
        if trimmed.is_empty() {
            return Err("Model ID cannot be empty.".to_string());
        }
        let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        st.config.selected_model = trimmed.to_string();
        let cfg_path = st.app_data_dir.join("config.json");
        st.config.save_atomic(&cfg_path).map_err(|e| e.to_string())?;
        st.active_session.model = trimmed.to_string();
        let _ = st.sessions.save_header(&st.active_session);
        Ok(())
    }

    pub fn set_agent_mode(&self, mode: AgentMode) -> Result<(), String> {
        let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        st.config.agent_mode = mode;
        let cfg_path = st.app_data_dir.join("config.json");
        st.config.save_atomic(&cfg_path).map_err(|e| e.to_string())?;
        st.active_session.mode = mode;
        let _ = st.sessions.save_header(&st.active_session);
        Ok(())
    }

    pub fn set_runtime_selection(&self, sel: RuntimeSelection) -> Result<(), String> {
        let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        st.config.runtime_selection = sel;
        st.compat = assess_device_compatibility(&st.device_info, sel, &st.config.api_endpoint);
        let cfg_path = st.app_data_dir.join("config.json");
        st.config.save_atomic(&cfg_path).map_err(|e| e.to_string())
    }

    pub fn set_theme_mode(&self, theme: ThemeMode) -> Result<(), String> {
        let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        st.config.theme_mode = theme;
        let cfg_path = st.app_data_dir.join("config.json");
        st.config.save_atomic(&cfg_path).map_err(|e| e.to_string())
    }

    pub fn refresh_models_async(&self) {
        let engine = self.clone();
        self.rt.spawn(async move {
            engine.refresh_models_async_inner().await;
        });
    }

    async fn refresh_models_async_inner(&self) {
        let runtime = {
            let Ok(st) = self.inner.lock() else { return };
            st.compat.active_runtime.clone()
        };
        let result = match runtime {
            ExecutionRuntime::NativeCli { binary_path, .. } => {
                query_native_cli_models(&binary_path).await
            }
            ExecutionRuntime::CompatibleApi { endpoint, .. } => {
                match self.ensure_fresh_credential().await {
                    Ok(cred) => fetch_compatible_api_models(&self.http, &endpoint, &cred).await,
                    Err(_) => Ok(AgyConfig::builtin_compat_models()),
                }
            }
        };

        match result {
            Ok(models) if !models.is_empty() => {
                let mut text = format!("Available models ({}):", models.len());
                for m in models.iter().take(15) {
                    text.push_str(&format!("\n  {} ({})", m.id, m.display_name));
                }
                if let Ok(mut st) = self.inner.lock() {
                    st.cached_models = models;
                }
                self.append_line(&text, AnsiColor::Default);
            }
            Ok(_) => self.append_line("No models returned by active runtime.", AnsiColor::Muted),
            Err(e) => self.append_line(&e, AnsiColor::Red),
        }
    }

    pub fn install_official_cli_async(&self) {
        let engine = self.clone();
        self.rt.spawn(async move {
            engine.set_busy(true, "Checking & downloading official android_arm64 agy binary...");
            let (bin_dir, sdk_int) = {
                let Ok(st) = engine.inner.lock() else { return };
                (st.app_data_dir.join("bin"), st.device_info.android_sdk_int)
            };
            match download_official_arm64_cli(&engine.http, &bin_dir, sdk_int).await {
                Ok(det) => {
                    if let Ok(mut st) = engine.inner.lock() {
                        st.device_info.detected_cli_path = Some(det.path.clone());
                        st.device_info.detected_cli_version = Some(det.version.clone());
                        st.compat = assess_device_compatibility(
                            &st.device_info,
                            st.config.runtime_selection,
                            &st.config.api_endpoint,
                        );
                        st.busy = false;
                        st.status_line = format!("Installed official agy {} (arm64).", det.version);
                    }
                    engine.append_line(
                        &format!(
                            "Verified SHA-512 and installed official Google agy {} at {}.",
                            det.version,
                            det.path.display()
                        ),
                        AnsiColor::Green,
                    );
                }
                Err(e) => {
                    engine.set_busy(false, "Official CLI install unavailable on this ABI/SDK.");
                    engine.append_line(&format!("[compat] {e}"), AnsiColor::Amber);
                }
            }
        });
    }

    pub fn set_workspace_root(&self, path: PathBuf, saf_uri: Option<String>) -> Result<(), String> {
        let mgr = WorkspaceManager::open(&path).map_err(|e| e.to_string())?;
        let canon = mgr.root().to_path_buf();
        let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        st.workspace = Some(mgr);
        st.config.active_workspace = Some(canon.clone());
        st.config.active_saf_uri = saf_uri;
        let cfg_path = st.app_data_dir.join("config.json");
        st.config.save_atomic(&cfg_path).map_err(|e| e.to_string())?;
        st.active_session.workspace_path = Some(canon.display().to_string());
        let _ = st.sessions.save_header(&st.active_session);
        st.status_line = format!("Active workspace: {}", canon.display());
        st.terminal.push_styled_line(
            &format!("[workspace] Mounted {}", canon.display()),
            SpanStyle {
                fg: AnsiColor::Cyan,
                bold: false,
                underline: false,
            },
        );
        Ok(())
    }

    pub fn workspace_overview(&self) -> Result<WorkspaceOverview, String> {
        let st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        let Some(ws) = &st.workspace else {
            return Ok(WorkspaceOverview {
                has_workspace: false,
                root_path: String::new(),
                saf_uri: st.config.active_saf_uri.clone(),
                entries: Vec::new(),
                git: GitRepoStatus {
                    is_repo: false,
                    branch: String::new(),
                    head_commit: None,
                    changes: Vec::new(),
                    backend: "none".to_string(),
                },
                commits: Vec::new(),
            });
        };
        let entries = ws.list_tree(4, 250).unwrap_or_default();
        let git = WorkspaceGit::status(ws.root()).unwrap_or(GitRepoStatus {
            is_repo: false,
            branch: String::new(),
            head_commit: None,
            changes: Vec::new(),
            backend: "none".to_string(),
        });
        let commits = if git.is_repo {
            WorkspaceGit::log(ws.root(), 10).unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(WorkspaceOverview {
            has_workspace: true,
            root_path: ws.root().display().to_string(),
            saf_uri: st.config.active_saf_uri.clone(),
            entries,
            git,
            commits,
        })
    }

    pub fn workspace_read_file(
        &self,
        rel_path: &str,
        start_line: usize,
        end_line: usize,
    ) -> Result<FileReadSlice, String> {
        let st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        let ws = st
            .workspace
            .as_ref()
            .ok_or_else(|| "No workspace open.".to_string())?;
        ws.read_lines_streaming(
            rel_path,
            start_line,
            end_line,
            st.budget.max_file_preview_bytes,
        )
        .map_err(|e| e.to_string())
    }

    pub fn workspace_read_raw_for_editor(&self, rel_path: &str) -> Result<String, String> {
        let st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        let ws = st
            .workspace
            .as_ref()
            .ok_or_else(|| "No workspace open.".to_string())?;
        ws.read_raw_bounded(rel_path, st.budget.max_file_preview_bytes)
            .map_err(|e| e.to_string())
    }

    pub fn workspace_save_file(
        &self,
        rel_path: &str,
        content: &str,
        confirmed_by_user_ui: bool,
    ) -> Result<String, String> {
        let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        let policy = st.config.permission_policy;
        let mode = st.config.agent_mode;
        if mode != AgentMode::Standard || policy == PermissionPolicy::ReadOnlyStrict {
            return Err("Workspace writes are disabled in Plan or Read-Only mode.".to_string());
        }
        let ws = st
            .workspace
            .as_mut()
            .ok_or_else(|| "No workspace open.".to_string())?;

        if !confirmed_by_user_ui {
            if let Some(kind) = ws
                .check_write_approval_needed(policy, rel_path, content.len() as u64)
                .map_err(|e| e.to_string())?
            {
                let id = format!("ap-{:x}", SessionStore::now_ms() & 0xffff_ffff);
                let summary = format!("Overwrite existing file '{rel_path}'");
                let approval = PendingApproval {
                    id,
                    session_id: st.active_session.id.clone(),
                    created_at_ms: SessionStore::now_ms(),
                    kind,
                    summary: summary.clone(),
                };
                st.pending_approval = Some((
                    approval,
                    PendingContinuation::DirectFileOverwrite {
                        path: rel_path.to_string(),
                        content: content.to_string(),
                    },
                ));
                return Ok("AWAITING_APPROVAL".to_string());
            }
        }

        ws.write_file_atomic(rel_path, content)
            .map_err(|e| e.to_string())
    }

    pub fn workspace_delete_file(
        &self,
        rel_path: &str,
        confirmed_by_user_ui: bool,
    ) -> Result<String, String> {
        let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        if st.config.agent_mode != AgentMode::Standard {
            return Err("Deletes are disabled in Plan or Read-Only mode.".to_string());
        }
        let ws = st
            .workspace
            .as_mut()
            .ok_or_else(|| "No workspace open.".to_string())?;
        let (_, norm) = ws.resolve_safe_path(rel_path).map_err(|e| e.to_string())?;

        if !confirmed_by_user_ui {
            let id = format!("ap-{:x}", SessionStore::now_ms() & 0xffff_ffff);
            let approval = PendingApproval {
                id,
                session_id: st.active_session.id.clone(),
                created_at_ms: SessionStore::now_ms(),
                kind: ApprovalKind::DeleteFile {
                    relative_path: norm.clone(),
                },
                summary: format!("Delete workspace file '{norm}'"),
            };
            st.pending_approval = Some((
                approval,
                PendingContinuation::DirectFileDelete { path: norm },
            ));
            return Ok("AWAITING_APPROVAL".to_string());
        }

        ws.delete_file(&norm).map_err(|e| e.to_string())
    }

    pub fn workspace_search(&self, query: &str) -> Result<Vec<SearchMatch>, String> {
        let st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        let ws = st
            .workspace
            .as_ref()
            .ok_or_else(|| "No workspace open.".to_string())?;
        ws.search_files_streaming(query, st.budget.max_search_hits)
            .map_err(|e| e.to_string())
    }

    pub fn workspace_git_diff(&self, rel_path: &str) -> Result<String, String> {
        let st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        let ws = st
            .workspace
            .as_ref()
            .ok_or_else(|| "No workspace open.".to_string())?;
        WorkspaceGit::diff_file(ws.root(), rel_path).map_err(|e| e.to_string())
    }

    pub fn rotate_to_fresh_session(&self, title: &str) -> Result<SessionHeader, String> {
        let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        let ws_str = st
            .config
            .active_workspace
            .as_ref()
            .map(|p| p.display().to_string());
        let hdr = st
            .sessions
            .create_session(title, &st.config.selected_model, st.config.agent_mode, ws_str)
            .map_err(|e| e.to_string())?;
        st.active_session = hdr.clone();
        st.pending_approval = None;
        Ok(hdr)
    }

    pub fn switch_session(&self, session_id: &str) -> Result<SessionHeader, String> {
        let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        let hdr = st
            .sessions
            .load_header(session_id)
            .map_err(|e| e.to_string())?;
        let turns = st
            .sessions
            .load_recent_turns(&hdr.id, 20)
            .unwrap_or_default();
        let has_credential = st.credential.is_some();
        let compat = st.compat.clone();
        st.terminal.clear();
        print_boot_banner(&mut st.terminal, &compat, has_credential);
        for t in turns {
            let prefix = match t.role {
                Role::User => "\x1b[1;33magy>\x1b[0m ",
                Role::Assistant => "",
                Role::System | Role::Tool => "\x1b[2m[sys]\x1b[0m ",
            };
            st.terminal
                .feed_stream_chunk(&format!("{prefix}{}\n", t.content.trim_end()));
        }
        st.active_session = hdr.clone();
        st.pending_approval = None;
        Ok(hdr)
    }

    pub fn delete_session(&self, session_id: &str) -> Result<(), String> {
        let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        st.sessions
            .delete_session(session_id)
            .map_err(|e| e.to_string())?;
        if st.active_session.id == session_id {
            let remaining = st.sessions.list_sessions().unwrap_or_default();
            if let Some(first) = remaining.first() {
                st.active_session = first.clone();
            } else {
                let ws_str = st
                    .config
                    .active_workspace
                    .as_ref()
                    .map(|p| p.display().to_string());
                st.active_session = st
                    .sessions
                    .create_session(
                        "Session 1",
                        &st.config.selected_model,
                        st.config.agent_mode,
                        ws_str,
                    )
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    pub fn terminal_snapshot(&self) -> Result<TerminalSnapshot, String> {
        let mut st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        let dirty_saf_paths = st
            .workspace
            .as_mut()
            .map(|w| w.take_dirty_saf_paths())
            .unwrap_or_default();
        Ok(TerminalSnapshot {
            lines: st.terminal.snapshot_lines(),
            busy: st.busy,
            status_line: st.status_line.clone(),
            runtime_badge: st.compat.active_runtime.badge_label().to_string(),
            mode_label: st.config.agent_mode.label().to_string(),
            model: st.config.selected_model.clone(),
            session_id: st.active_session.id.clone(),
            session_title: st.active_session.title.clone(),
            pending_approval: st.pending_approval.as_ref().map(|(a, _)| a.clone()),
            dirty_saf_paths,
        })
    }

    pub fn export_terminal_text(&self) -> Result<String, String> {
        let st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        Ok(st.terminal.export_plain_text(600))
    }

    pub fn full_status_snapshot(&self) -> Result<FullStatusSnapshot, String> {
        let st = self.inner.lock().map_err(|_| "Lock poisoned")?;
        let auth = build_auth_summary(&st.credential, &st.compat.active_runtime, st.keystore_backed);
        let sessions = st.sessions.list_sessions().unwrap_or_default();
        let (memory_lines, memory_bytes, max_lines, max_bytes) = st.terminal.stats();
        Ok(FullStatusSnapshot {
            compat: st.compat.clone(),
            auth,
            config: st.config.clone(),
            active_session: st.active_session.clone(),
            sessions,
            models: st.cached_models.clone(),
            memory_lines,
            memory_bytes,
            max_lines,
            max_bytes,
        })
    }

    fn reset_cancel_channel(&self) -> watch::Receiver<bool> {
        let (tx, rx) = watch::channel(false);
        if let Ok(mut guard) = self.cancel_tx.lock() {
            *guard = tx;
        }
        rx
    }

    fn set_busy(&self, busy: bool, status: &str) {
        if let Ok(mut st) = self.inner.lock() {
            st.busy = busy;
            st.status_line = status.to_string();
        }
    }

    fn feed_stream(&self, chunk: &str) {
        if let Ok(mut st) = self.inner.lock() {
            st.terminal.feed_stream_chunk(chunk);
        }
    }

    fn append_line(&self, text: &str, fg: AnsiColor) {
        if let Ok(mut st) = self.inner.lock() {
            st.terminal.push_styled_line(
                text,
                SpanStyle {
                    fg,
                    bold: false,
                    underline: false,
                },
            );
        }
    }

    fn record_assistant_turn(&self, session_id: &str, content: &str) {
        if content.trim().is_empty() {
            return;
        }
        if let Ok(mut st) = self.inner.lock() {
            let turn = TurnMessage {
                timestamp_ms: SessionStore::now_ms(),
                role: Role::Assistant,
                content: content.to_string(),
                tool_calls: Vec::new(),
            };
            if let Ok(hdr) = st.sessions.append_turn(session_id, &turn) {
                st.active_session = hdr;
            }
        }
    }
}

fn print_boot_banner(
    term: &mut TerminalBuffer,
    compat: &CompatibilityReport,
    has_stored_auth: bool,
) {
    term.push_styled_line(
        &format!(
            "AGY Mobile v0.1.0 | {} | {} ({}-bit, API {})",
            compat.active_runtime.badge_label(),
            compat.android_abi,
            compat.pointer_bits,
            compat.android_sdk_int
        ),
        SpanStyle {
            fg: AnsiColor::Amber,
            bold: true,
            underline: false,
        },
    );
    match &compat.active_runtime {
        ExecutionRuntime::NativeCli {
            binary_path,
            version,
        } => {
            term.push_styled_line(
                &format!("Native agy binary ({version}): {}", binary_path.display()),
                SpanStyle {
                    fg: AnsiColor::Green,
                    bold: false,
                    underline: false,
                },
            );
        }
        ExecutionRuntime::CompatibleApi {
            fallback_reason, ..
        } => {
            term.push_styled_line(
                &format!("Mode detail: {fallback_reason}"),
                SpanStyle {
                    fg: AnsiColor::Muted,
                    bold: false,
                    underline: false,
                },
            );
            if !has_stored_auth {
                term.push_styled_line(
                    "Auth status: not signed in yet. Open Config to authenticate via Google OAuth PKCE or Gemini API key.",
                    SpanStyle {
                        fg: AnsiColor::Cyan,
                        bold: false,
                        underline: false,
                    },
                );
            }
        }
    }
    term.push_styled_line(
        "Type /help for slash commands, /plan for Plan Mode, or enter a prompt.",
        SpanStyle {
            fg: AnsiColor::Muted,
            bold: false,
            underline: false,
        },
    );
}

fn build_auth_summary(
    cred: &Option<StoredCredential>,
    runtime: &ExecutionRuntime,
    keystore_backed: bool,
) -> AuthStatusSummary {
    match cred {
        Some(StoredCredential::OAuthTokens {
            expires_at_ms,
            refresh_token,
            ..
        }) => AuthStatusSummary {
            authenticated: true,
            method: "Google OAuth 2.0 (PKCE)".to_string(),
            expires_at_ms: *expires_at_ms,
            has_refresh_token: refresh_token.is_some(),
            keystore_protected: keystore_backed,
        },
        Some(StoredCredential::GeminiApiKey { .. }) => AuthStatusSummary {
            authenticated: true,
            method: "Gemini API Key (Header)".to_string(),
            expires_at_ms: None,
            has_refresh_token: false,
            keystore_protected: keystore_backed,
        },
        None => {
            if runtime.is_native_cli() {
                AuthStatusSummary {
                    authenticated: true,
                    method: "Delegated to Native agy CLI session".to_string(),
                    expires_at_ms: None,
                    has_refresh_token: false,
                    keystore_protected: keystore_backed,
                }
            } else {
                AuthStatusSummary {
                    authenticated: false,
                    method: "Not signed in".to_string(),
                    expires_at_ms: None,
                    has_refresh_token: false,
                    keystore_protected: keystore_backed,
                }
            }
        }
    }
}

pub fn workspace_path_exists(p: &Path) -> bool {
    p.exists()
}
