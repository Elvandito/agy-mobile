use crate::engine::AgyEngine;
use agy_core::{AgentMode, RuntimeSelection, ThemeMode};
use jni::objects::{JByteArray, JClass, JString};
use jni::sys::{jboolean, jint, jlong, jstring, JNI_TRUE};
use jni::JNIEnv;
use serde::Serialize;
use serde_json::json;
use std::path::PathBuf;
use std::sync::OnceLock;

static GLOBAL_ENGINE: OnceLock<AgyEngine> = OnceLock::new();

#[derive(Serialize)]
struct JniResponse<T: Serialize> {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn to_jstring<T: Serialize>(env: &mut JNIEnv, res: Result<T, String>) -> jstring {
    let payload = match res {
        Ok(data) => serde_json::to_string(&JniResponse {
            ok: true,
            data: Some(data),
            error: None,
        })
        .unwrap_or_else(|e| format!("{{\"ok\":false,\"error\":\"{e}\"}}")),
        Err(err) => serde_json::to_string(&JniResponse::<serde_json::Value> {
            ok: false,
            data: None,
            error: Some(err),
        })
        .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"serialization failure\"}".to_string()),
    };
    env.new_string(payload)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

fn read_jstr(env: &mut JNIEnv, s: &JString) -> String {
    if s.is_null() {
        return String::new();
    }
    env.get_string(s)
        .map(|js| js.into())
        .unwrap_or_default()
}

fn engine_ref() -> Result<&'static AgyEngine, String> {
    GLOBAL_ENGINE
        .get()
        .ok_or_else(|| "Engine is not initialized yet.".to_string())
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeInit(
    mut env: JNIEnv,
    _class: JClass,
    files_dir: JString,
    sdk_int: jint,
    total_ram_mb: jlong,
    vault_key: JByteArray,
    keystore_backed: jboolean,
) -> jstring {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if GLOBAL_ENGINE.get().is_some() {
            return Ok(json!({ "status": "already_initialized" }));
        }
        let dir = PathBuf::from(read_jstr(&mut env, &files_dir));
        let key_vec = env
            .convert_byte_array(&vault_key)
            .map_err(|e| format!("Invalid vault key bytes: {e}"))?;
        if key_vec.len() != 32 {
            return Err(format!("Expected 32-byte vault key, got {}", key_vec.len()));
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&key_vec);

        let eng = AgyEngine::initialize(
            dir,
            sdk_int.max(21) as u32,
            total_ram_mb.max(512) as u64,
            key,
            keystore_backed == JNI_TRUE,
        )?;
        let _ = GLOBAL_ENGINE.set(eng);
        Ok(json!({ "status": "initialized" }))
    }))
    .unwrap_or_else(|_| Err("Panic caught in nativeInit".to_string()));

    to_jstring(&mut env, outcome)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeSubmitInput(
    mut env: JNIEnv,
    _class: JClass,
    input: JString,
) -> jstring {
    let text = read_jstr(&mut env, &input);
    let res = engine_ref().map(|eng| {
        eng.submit_input_async(text);
        json!({ "queued": true })
    });
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeGetTerminalSnapshot(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let res = engine_ref().and_then(|eng| eng.terminal_snapshot());
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeGetFullStatus(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let res = engine_ref().and_then(|eng| eng.full_status_snapshot());
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeSendCtrlC(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let res = engine_ref().map(|eng| {
        let out = eng.send_ctrl_c();
        json!({ "outcome": format!("{out:?}") })
    });
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeSendCtrlD(
    mut env: JNIEnv,
    _class: JClass,
    input_empty: jboolean,
) -> jstring {
    let res = engine_ref().map(|eng| {
        let out = eng.send_ctrl_d(input_empty == JNI_TRUE);
        json!({ "outcome": format!("{out:?}") })
    });
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeHistoryNav(
    mut env: JNIEnv,
    _class: JClass,
    direction: jint,
) -> jstring {
    let res = engine_ref().map(|eng| {
        let entry = if direction < 0 {
            eng.history_previous()
        } else {
            eng.history_next()
        };
        json!({ "entry": entry })
    });
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeExportTerminalText(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let res = engine_ref().and_then(|eng| eng.export_terminal_text());
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeResolveApproval(
    mut env: JNIEnv,
    _class: JClass,
    approved: jboolean,
) -> jstring {
    let res = engine_ref().map(|eng| {
        eng.resolve_approval_async(approved == JNI_TRUE);
        json!({ "submitted": true })
    });
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeOnTrimMemory(
    _env: JNIEnv,
    _class: JClass,
    level: jint,
) {
    if let Ok(eng) = engine_ref() {
        eng.on_trim_memory(level);
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeSaveApiKey(
    mut env: JNIEnv,
    _class: JClass,
    api_key: JString,
) -> jstring {
    let key = read_jstr(&mut env, &api_key);
    let res = engine_ref().and_then(|eng| {
        eng.save_api_key(&key)?;
        Ok(json!({ "saved": true }))
    });
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeStartOAuthPkce(
    mut env: JNIEnv,
    _class: JClass,
    client_id: JString,
) -> jstring {
    let cid = read_jstr(&mut env, &client_id);
    let res = engine_ref().and_then(|eng| {
        let auth_url = eng.start_oauth_pkce_flow(&cid)?;
        Ok(json!({ "authorization_url": auth_url }))
    });
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeCompleteOAuthPkce(
    mut env: JNIEnv,
    _class: JClass,
    callback_or_code: JString,
) -> jstring {
    let code = read_jstr(&mut env, &callback_or_code);
    let res = engine_ref().and_then(|eng| {
        eng.complete_oauth_pkce_blocking(&code)?;
        Ok(json!({ "authenticated": true }))
    });
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeSignOut(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let res = engine_ref().and_then(|eng| {
        eng.sign_out()?;
        Ok(json!({ "signed_out": true }))
    });
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeUpdateConfig(
    mut env: JNIEnv,
    _class: JClass,
    model: JString,
    mode: JString,
    runtime_sel: JString,
    theme: JString,
) -> jstring {
    let model_s = read_jstr(&mut env, &model);
    let mode_s = read_jstr(&mut env, &mode);
    let rt_s = read_jstr(&mut env, &runtime_sel);
    let theme_s = read_jstr(&mut env, &theme);

    let res = engine_ref().and_then(|eng| {
        if !model_s.is_empty() {
            eng.set_selected_model(&model_s)?;
        }
        if !mode_s.is_empty() {
            let m = match mode_s.as_str() {
                "plan" => AgentMode::Plan,
                "read_only" | "readonly" => AgentMode::ReadOnly,
                _ => AgentMode::Standard,
            };
            eng.set_agent_mode(m)?;
        }
        if !rt_s.is_empty() {
            let sel = match rt_s.as_str() {
                "prefer_native_cli" => RuntimeSelection::PreferNativeCli,
                "force_compatible_api" => RuntimeSelection::ForceCompatibleApi,
                _ => RuntimeSelection::Auto,
            };
            eng.set_runtime_selection(sel)?;
        }
        if !theme_s.is_empty() {
            let t = if theme_s == "light_paper" {
                ThemeMode::LightPaper
            } else {
                ThemeMode::DarkHardware
            };
            eng.set_theme_mode(t)?;
        }
        Ok(json!({ "updated": true }))
    });
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeRefreshModels(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let res = engine_ref().map(|eng| {
        eng.refresh_models_async();
        json!({ "refreshing": true })
    });
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeInstallOfficialCli(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let res = engine_ref().map(|eng| {
        eng.install_official_cli_async();
        json!({ "started": true })
    });
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeSetWorkspace(
    mut env: JNIEnv,
    _class: JClass,
    root_path: JString,
    saf_uri: JString,
) -> jstring {
    let root = read_jstr(&mut env, &root_path);
    let uri = read_jstr(&mut env, &saf_uri);
    let saf_opt = if uri.trim().is_empty() {
        None
    } else {
        Some(uri)
    };
    let res = engine_ref().and_then(|eng| {
        eng.set_workspace_root(PathBuf::from(root), saf_opt)?;
        eng.workspace_overview()
    });
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeWorkspaceOverview(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let res = engine_ref().and_then(|eng| eng.workspace_overview());
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeWorkspaceReadFile(
    mut env: JNIEnv,
    _class: JClass,
    rel_path: JString,
) -> jstring {
    let p = read_jstr(&mut env, &rel_path);
    let res = engine_ref().and_then(|eng| eng.workspace_read_raw_for_editor(&p));
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeWorkspaceSaveFile(
    mut env: JNIEnv,
    _class: JClass,
    rel_path: JString,
    content: JString,
    confirmed: jboolean,
) -> jstring {
    let p = read_jstr(&mut env, &rel_path);
    let c = read_jstr(&mut env, &content);
    let res =
        engine_ref().and_then(|eng| eng.workspace_save_file(&p, &c, confirmed == JNI_TRUE));
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeWorkspaceDeleteFile(
    mut env: JNIEnv,
    _class: JClass,
    rel_path: JString,
    confirmed: jboolean,
) -> jstring {
    let p = read_jstr(&mut env, &rel_path);
    let res = engine_ref().and_then(|eng| eng.workspace_delete_file(&p, confirmed == JNI_TRUE));
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeWorkspaceSearch(
    mut env: JNIEnv,
    _class: JClass,
    query: JString,
) -> jstring {
    let q = read_jstr(&mut env, &query);
    let res = engine_ref().and_then(|eng| eng.workspace_search(&q));
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeWorkspaceGitDiff(
    mut env: JNIEnv,
    _class: JClass,
    rel_path: JString,
) -> jstring {
    let p = read_jstr(&mut env, &rel_path);
    let res = engine_ref().and_then(|eng| eng.workspace_git_diff(&p));
    to_jstring(&mut env, res)
}

#[no_mangle]
pub extern "system" fn Java_dev_antigravity_mobile_bridge_NativeBridge_nativeSessionAction(
    mut env: JNIEnv,
    _class: JClass,
    action: JString,
    arg: JString,
) -> jstring {
    let act = read_jstr(&mut env, &action);
    let a = read_jstr(&mut env, &arg);
    let res = engine_ref().and_then(|eng| match act.as_str() {
        "new" => eng
            .rotate_to_fresh_session(if a.is_empty() { "New session" } else { &a })
            .map(|h| json!(h)),
        "switch" => eng.switch_session(&a).map(|h| json!(h)),
        "delete" => eng.delete_session(&a).map(|_| json!({ "deleted": a })),
        _ => Err(format!("Unsupported session action '{act}'")),
    });
    to_jstring(&mut env, res)
}
