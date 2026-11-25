use agy_auth::scrub_sensitive_text;
use agy_core::{
    AbiTarget, AgentMode, ModelSpec, OFFICIAL_ARM64_MANIFEST_URL,
};
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha512};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::watch;

#[derive(Debug, Clone)]
pub struct DetectedCli {
    pub path: PathBuf,
    pub version: String,
}

#[derive(Deserialize)]
struct OfficialManifest {
    version: String,
    url: String,
    sha512: String,
}

pub fn probe_installed_cli(app_bin_dir: &Path, sdk_int: u32) -> Option<DetectedCli> {
    if AbiTarget::current_compiled() != AbiTarget::Arm64 || sdk_int < 24 {
        return None;
    }

    let mut candidates = vec![
        app_bin_dir.join("agy"),
        PathBuf::from("/data/data/com.termux/files/usr/bin/agy"),
    ];
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            candidates.push(dir.join("agy"));
        }
    }

    for candidate in candidates {
        if !candidate.is_file() {
            continue;
        }
        if !is_valid_aarch64_elf_or_script(&candidate) {
            continue;
        }
        if let Ok(output) = std::process::Command::new(&candidate)
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
        {
            if output.status.success() {
                let ver = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("installed")
                    .trim()
                    .to_string();
                return Some(DetectedCli {
                    path: candidate,
                    version: if ver.is_empty() {
                        "installed".to_string()
                    } else {
                        ver
                    },
                });
            }
        }
    }
    None
}

fn is_valid_aarch64_elf_or_script(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let mut header = [0u8; 20];
    let Ok(n) = f.read(&mut header) else {
        return false;
    };
    if n >= 2 && &header[0..2] == b"#!" {
        return true;
    }
    if n < 20 || &header[0..4] != b"\x7fELF" {
        return false;
    }
    // ELF64 little-endian EM_AARCH64 (183 = 0x00B7)
    let is_64 = header[4] == 2;
    let e_machine = u16::from_le_bytes([header[18], header[19]]);
    is_64 && e_machine == 183
}

pub async fn query_native_cli_models(binary: &Path) -> Result<Vec<ModelSpec>, String> {
    let out = Command::new(binary)
        .arg("models")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to invoke agy models: {e}"))?;

    if !out.status.success() {
        let err = scrub_sensitive_text(&String::from_utf8_lossy(&out.stderr));
        return Err(format!("agy models exited with {}: {}", out.status, err.trim()));
    }

    let mut models = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("Fetching") {
            continue;
        }
        let mut parts = trimmed.splitn(2, '\t');
        let id = parts.next().unwrap_or("").trim();
        let label = parts.next().unwrap_or(id).trim();
        if !id.is_empty() && !id.contains(' ') {
            models.push(ModelSpec {
                id: id.to_string(),
                display_name: label.to_string(),
                supports_tools: true,
                source: "native-cli".to_string(),
            });
        }
    }
    Ok(models)
}

pub async fn run_native_cli_turn<F>(
    binary: &Path,
    workspace_dir: Option<&Path>,
    prompt: &str,
    model: &str,
    mode: AgentMode,
    mut cancel_rx: watch::Receiver<bool>,
    mut on_chunk: F,
) -> Result<String, String>
where
    F: FnMut(&str),
{
    let mut cmd = Command::new(binary);
    cmd.arg("-p")
        .arg(prompt)
        .arg("--output-format")
        .arg("stream-json");

    if !model.trim().is_empty() {
        cmd.arg("--model").arg(model.trim());
    }
    match mode {
        AgentMode::Plan | AgentMode::ReadOnly => {
            cmd.arg("--mode").arg("plan");
        }
        AgentMode::Standard => {}
    }
    if let Some(ws) = workspace_dir {
        cmd.current_dir(ws);
        cmd.arg("--add-dir").arg(ws);
    }

    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to start native agy binary: {e}"))?;

    let child_pid = child.id();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Missing stdout pipe from native agy".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Missing stderr pipe from native agy".to_string())?;

    let mut out_lines = BufReader::new(stdout).lines();
    let mut err_lines = BufReader::new(stderr).lines();
    let mut collected_text = String::new();
    let mut stdout_done = false;
    let mut stderr_done = false;

    while !stdout_done || !stderr_done {
        tokio::select! {
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    if let Some(pid) = child_pid {
                        unsafe {
                            let _ = libc::kill(pid as i32, libc::SIGINT);
                        }
                    }
                    let _ = child.kill().await;
                    return Err("Interrupted by Ctrl+C.".to_string());
                }
            }
            line_res = out_lines.next_line(), if !stdout_done => {
                match line_res {
                    Ok(Some(line)) => {
                        if let Some(rendered) = extract_stream_json_text(&line) {
                            collected_text.push_str(&rendered);
                            on_chunk(&rendered);
                        }
                    }
                    Ok(None) => stdout_done = true,
                    Err(e) => {
                        stdout_done = true;
                        on_chunk(&format!("\n[native stdout error: {e}]\n"));
                    }
                }
            }
            err_res = err_lines.next_line(), if !stderr_done => {
                match err_res {
                    Ok(Some(line)) => {
                        let clean = scrub_sensitive_text(&line);
                        if !clean.trim().is_empty() {
                            on_chunk(&format!("\x1b[2m[cli] {clean}\x1b[0m\n"));
                        }
                    }
                    Ok(None) => stderr_done = true,
                    Err(_) => stderr_done = true,
                }
            }
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| format!("Process wait failed: {e}"))?;
    if !status.success() && collected_text.trim().is_empty() {
        return Err(format!("Native agy process exited with status {status}."));
    }
    Ok(collected_text)
}

fn extract_stream_json_text(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Some(format!("{}\n", scrub_sensitive_text(trimmed)));
    };

    if let Some(delta) = val.get("delta").and_then(|v| v.as_str()) {
        return Some(scrub_sensitive_text(delta));
    }
    if let Some(content) = val.get("content").and_then(|v| v.as_str()) {
        if !content.is_empty() {
            return Some(format!("{}\n", scrub_sensitive_text(content)));
        }
    }
    if let Some(text) = val.get("text").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            return Some(scrub_sensitive_text(text));
        }
    }
    if let Some(result) = val.get("result").and_then(|v| v.as_str()) {
        if !result.is_empty() {
            return Some(format!("{}\n", scrub_sensitive_text(result)));
        }
    }
    None
}

// Downloads and SHA-512-verifies Google's official android_arm64 binary only on compatible ARM64 API 24+ hosts.
pub async fn download_official_arm64_cli(
    http: &reqwest::Client,
    app_bin_dir: &Path,
    sdk_int: u32,
) -> Result<DetectedCli, String> {
    if AbiTarget::current_compiled() != AbiTarget::Arm64 {
        return Err(format!(
            "Official Google agy Android binary is only published for arm64-v8a; current ABI is {}. Use API compatibility mode.",
            AbiTarget::current_compiled().android_abi()
        ));
    }
    if sdk_int < 24 {
        return Err(format!(
            "Official Google android_arm64 agy binary requires Android API 24+; current device runs API {sdk_int}."
        ));
    }

    let manifest: OfficialManifest = http
        .get(OFFICIAL_ARM64_MANIFEST_URL)
        .send()
        .await
        .map_err(|e| format!("Manifest request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Manifest HTTP error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Invalid manifest JSON: {e}"))?;

    fs::create_dir_all(app_bin_dir).map_err(|e| e.to_string())?;
    let archive_path = app_bin_dir.join("cli_android_arm64.tar.gz.part");

    let resp = http
        .get(&manifest.url)
        .send()
        .await
        .map_err(|e| format!("Archive download failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Archive HTTP status error: {e}"))?;

    let mut stream = resp.bytes_stream();
    let mut hasher = Sha512::new();
    let mut out_file = File::create(&archive_path).map_err(|e| e.to_string())?;
    let mut downloaded: u64 = 0;

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res.map_err(|e| format!("Network stream interrupted: {e}"))?;
        downloaded += chunk.len() as u64;
        if downloaded > 160 * 1024 * 1024 {
            let _ = fs::remove_file(&archive_path);
            return Err("Official archive exceeded safety size cap.".to_string());
        }
        hasher.update(&chunk);
        std::io::Write::write_all(&mut out_file, &chunk).map_err(|e| e.to_string())?;
    }
    drop(out_file);

    let actual_sha512 = hex::encode(hasher.finalize());
    if !actual_sha512.eq_ignore_ascii_case(manifest.sha512.trim()) {
        let _ = fs::remove_file(&archive_path);
        return Err("SHA-512 checksum mismatch on downloaded agy archive; aborted.".to_string());
    }

    let target_bin = app_bin_dir.join("agy");
    extract_agy_from_tar_gz(&archive_path, &target_bin)?;
    let _ = fs::remove_file(&archive_path);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&target_bin, fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }

    Ok(DetectedCli {
        path: target_bin,
        version: manifest.version,
    })
}

// Minimal streaming POSIX ustar extractor so we avoid pulling an extra archive crate dependency.
fn extract_agy_from_tar_gz(archive_path: &Path, dest_bin: &Path) -> Result<(), String> {
    let gz_file = File::open(archive_path).map_err(|e| e.to_string())?;
    let mut decoder = GzDecoder::new(gz_file);
    let mut header = [0u8; 512];

    loop {
        if let Err(_) = decoder.read_exact(&mut header) {
            break;
        }
        if header.iter().all(|&b| b == 0) {
            break;
        }
        let name_end = header[0..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = String::from_utf8_lossy(&header[0..name_end]).into_owned();
        let size_str = String::from_utf8_lossy(&header[124..136])
            .trim_matches(|c: char| c == '\0' || c.is_whitespace())
            .to_string();
        let size = u64::from_str_radix(&size_str, 8).unwrap_or(0);
        let typeflag = header[156];

        let is_agy_bin = (typeflag == b'0' || typeflag == 0)
            && (name == "agy" || name.ends_with("/agy"));

        if is_agy_bin && size > 0 {
            let tmp = dest_bin.with_extension("extract.tmp");
            let mut out = File::create(&tmp).map_err(|e| e.to_string())?;
            let mut limited = (&mut decoder).take(size);
            std::io::copy(&mut limited, &mut out).map_err(|e| e.to_string())?;
            fs::rename(&tmp, dest_bin).map_err(|e| e.to_string())?;
            return Ok(());
        } else {
            let padded = (size + 511) & !511;
            std::io::copy(&mut (&mut decoder).take(padded), &mut std::io::sink())
                .map_err(|e| e.to_string())?;
        }
    }

    Err("Archive verified, but no 'agy' executable entry was found inside.".to_string())
}
