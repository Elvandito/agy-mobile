use crate::WorkspaceError;
use flate2::read::ZlibDecoder;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFileChange {
    pub status: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCommitEntry {
    pub commit_id: String,
    pub short_id: String,
    pub author: String,
    pub timestamp_sec: i64,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRepoStatus {
    pub is_repo: bool,
    pub branch: String,
    pub head_commit: Option<String>,
    pub changes: Vec<GitFileChange>,
    pub backend: String,
}

// Pure-Rust `.git` reader so Git status, log, and diff work on bare Android 5.0 phones without a `git` binary.
pub struct WorkspaceGit;

impl WorkspaceGit {
    pub fn status(workspace_root: &Path) -> Result<GitRepoStatus, WorkspaceError> {
        let git_dir = workspace_root.join(".git");
        if !git_dir.is_dir() {
            return Ok(GitRepoStatus {
                is_repo: false,
                branch: String::new(),
                head_commit: None,
                changes: Vec::new(),
                backend: "none".to_string(),
            });
        }

        let (branch, head_commit) = resolve_head(&git_dir)?;

        // Parse `.git/index` (DIRC v2/v3) in pure Rust first; fall back to `git status` if index v4/sparse.
        if let Ok(index_map) = parse_git_index(&git_dir.join("index")) {
            let mut working_files = BTreeSet::new();
            collect_working_files(workspace_root, workspace_root, &mut working_files)?;

            let mut changes = Vec::new();
            for (rel_path, indexed_sha1) in &index_map {
                let disk_path = workspace_root.join(rel_path);
                if !disk_path.exists() {
                    changes.push(GitFileChange {
                        status: "D".to_string(),
                        path: rel_path.clone(),
                    });
                } else if disk_path.is_file() {
                    let current_sha1 = compute_git_blob_sha1_streaming(&disk_path)?;
                    if &current_sha1 != indexed_sha1 {
                        changes.push(GitFileChange {
                            status: "M".to_string(),
                            path: rel_path.clone(),
                        });
                    }
                }
            }

            for disk_rel in working_files {
                if !index_map.contains_key(&disk_rel) {
                    changes.push(GitFileChange {
                        status: "?".to_string(),
                        path: disk_rel,
                    });
                }
            }

            return Ok(GitRepoStatus {
                is_repo: true,
                branch,
                head_commit,
                changes,
                backend: "rust-native-git".to_string(),
            });
        }

        if let Some(cli_status) = try_system_git_status(workspace_root, &branch, head_commit.clone()) {
            return Ok(cli_status);
        }

        Ok(GitRepoStatus {
            is_repo: true,
            branch,
            head_commit,
            changes: Vec::new(),
            backend: "rust-native-git".to_string(),
        })
    }

    pub fn log(workspace_root: &Path, max_commits: usize) -> Result<Vec<GitCommitEntry>, WorkspaceError> {
        let git_dir = workspace_root.join(".git");
        if !git_dir.is_dir() {
            return Ok(Vec::new());
        }
        let (_, mut cursor) = resolve_head(&git_dir)?;
        let mut commits = Vec::new();
        let cap = max_commits.clamp(1, 50);

        while let Some(oid) = cursor.take() {
            if commits.len() >= cap {
                break;
            }
            match read_loose_object(&git_dir, &oid) {
                Ok((kind, payload)) if kind == "commit" => {
                    let text = String::from_utf8_lossy(&payload);
                    let parsed = parse_commit_object(&oid, &text);
                    cursor = parsed.0;
                    commits.push(parsed.1);
                }
                _ => {
                    if commits.is_empty() {
                        if let Some(sys_commits) = try_system_git_log(workspace_root, cap) {
                            return Ok(sys_commits);
                        }
                    }
                    break;
                }
            }
        }
        Ok(commits)
    }

    pub fn diff_file(
        workspace_root: &Path,
        rel_path: &str,
    ) -> Result<String, WorkspaceError> {
        let git_dir = workspace_root.join(".git");
        if !git_dir.is_dir() {
            return Ok("Not a Git repository.".to_string());
        }

        if let Ok(index_map) = parse_git_index(&git_dir.join("index")) {
            let norm = rel_path.trim().trim_start_matches("./");
            if let Some(blob_sha) = index_map.get(norm) {
                if let Ok((kind, old_bytes)) = read_loose_object(&git_dir, &hex::encode(blob_sha)) {
                    if kind == "blob" {
                        let old_text = String::from_utf8_lossy(&old_bytes);
                        let disk_path = workspace_root.join(norm);
                        let new_text = if disk_path.is_file() {
                            fs::read_to_string(&disk_path).unwrap_or_default()
                        } else {
                            String::new()
                        };
                        return Ok(render_simple_unified_diff(norm, &old_text, &new_text));
                    }
                }
            }
        }

        if let Ok(output) = Command::new("git")
            .arg("-C")
            .arg(workspace_root)
            .arg("diff")
            .arg("--")
            .arg(rel_path)
            .output()
        {
            if output.status.success() {
                return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
            }
        }

        Ok("No loose object diff available for the requested path.".to_string())
    }
}

fn resolve_head(git_dir: &Path) -> Result<(String, Option<String>), WorkspaceError> {
    let head_path = git_dir.join("HEAD");
    if !head_path.exists() {
        return Ok(("detached".to_string(), None));
    }
    let raw = fs::read_to_string(head_path)?.trim().to_string();
    if let Some(sym_ref) = raw.strip_prefix("ref:") {
        let sym_ref = sym_ref.trim();
        let branch = sym_ref
            .strip_prefix("refs/heads/")
            .unwrap_or(sym_ref)
            .to_string();
        let loose_ref = git_dir.join(sym_ref);
        if loose_ref.is_file() {
            let oid = fs::read_to_string(loose_ref)?.trim().to_string();
            return Ok((branch, if oid.is_empty() { None } else { Some(oid) }));
        }
        let packed = git_dir.join("packed-refs");
        if packed.is_file() {
            let packed_raw = fs::read_to_string(packed)?;
            for line in packed_raw.lines() {
                if line.starts_with('#') || line.starts_with('^') {
                    continue;
                }
                let mut parts = line.split_whitespace();
                if let (Some(oid), Some(rname)) = (parts.next(), parts.next()) {
                    if rname == sym_ref {
                        return Ok((branch, Some(oid.to_string())));
                    }
                }
            }
        }
        Ok((branch, None))
    } else if raw.len() >= 40 {
        Ok(("detached".to_string(), Some(raw)))
    } else {
        Ok(("unborn".to_string(), None))
    }
}

fn parse_git_index(index_path: &Path) -> Result<BTreeMap<String, [u8; 20]>, &'static str> {
    if !index_path.is_file() {
        return Ok(BTreeMap::new());
    }
    let data = fs::read(index_path).map_err(|_| "io")?;
    if data.len() < 12 || &data[0..4] != b"DIRC" {
        return Err("invalid_magic");
    }
    let version = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    if version != 2 && version != 3 {
        return Err("unsupported_index_version");
    }
    let count = u32::from_be_bytes([data[8], data[9], data[10], data[11]]) as usize;
    let mut offset = 12usize;
    let mut entries = BTreeMap::new();

    for _ in 0..count {
        if offset + 62 > data.len() {
            return Err("truncated");
        }
        let mut sha1 = [0u8; 20];
        sha1.copy_from_slice(&data[offset + 40..offset + 60]);
        let flags = u16::from_be_bytes([data[offset + 60], data[offset + 61]]);
        let extended = (flags & 0x4000) != 0 && version == 3;
        let mut name_start = offset + 62;
        if extended {
            name_start += 2;
        }
        if name_start >= data.len() {
            return Err("truncated_name");
        }
        let rel_end = data[name_start..]
            .iter()
            .position(|&b| b == 0)
            .ok_or("missing_nul")?;
        let name = String::from_utf8_lossy(&data[name_start..name_start + rel_end]).into_owned();
        entries.insert(name, sha1);

        let entry_raw_len = (name_start - offset) + rel_end + 1;
        let padded_len = (entry_raw_len + 7) & !7;
        offset += padded_len;
    }
    Ok(entries)
}

fn compute_git_blob_sha1_streaming(path: &Path) -> Result<[u8; 20], std::io::Error> {
    let meta = fs::metadata(path)?;
    let size = meta.len();
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {size}\0").as_bytes());

    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(8 * 1024, file);
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut out = [0u8; 20];
    out.copy_from_slice(&digest);
    Ok(out)
}

fn collect_working_files(
    root: &Path,
    current: &Path,
    out: &mut BTreeSet<String>,
) -> Result<(), std::io::Error> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if matches!(name.as_str(), ".git" | "target" | "node_modules" | ".gradle" | "build") {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            if out.len() < 2000 {
                collect_working_files(root, &path, out)?;
            }
        } else if meta.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                out.insert(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    Ok(())
}

fn read_loose_object(git_dir: &Path, oid_hex: &str) -> Result<(String, Vec<u8>), &'static str> {
    if oid_hex.len() < 40 {
        return Err("short_oid");
    }
    let obj_path: PathBuf = git_dir
        .join("objects")
        .join(&oid_hex[0..2])
        .join(&oid_hex[2..]);
    if !obj_path.is_file() {
        return Err("not_loose");
    }
    let file = File::open(obj_path).map_err(|_| "io")?;
    let decoder = ZlibDecoder::new(file);
    let mut raw = Vec::new();
    decoder
        .take(512 * 1024)
        .read_to_end(&mut raw)
        .map_err(|_| "zlib")?;
    let nul_pos = raw.iter().position(|&b| b == 0).ok_or("header")?;
    let header = String::from_utf8_lossy(&raw[..nul_pos]);
    let kind = header.split_whitespace().next().unwrap_or("").to_string();
    Ok((kind, raw[nul_pos + 1..].to_vec()))
}

fn parse_commit_object(oid: &str, text: &str) -> (Option<String>, GitCommitEntry) {
    let mut parent = None;
    let mut author_name = String::from("unknown");
    let mut timestamp_sec = 0i64;
    let mut in_body = false;
    let mut summary = String::new();

    for line in text.lines() {
        if !in_body {
            if line.is_empty() {
                in_body = true;
                continue;
            }
            if let Some(p) = line.strip_prefix("parent ") {
                if parent.is_none() {
                    parent = Some(p.trim().to_string());
                }
            } else if let Some(a) = line.strip_prefix("author ") {
                let parts: Vec<&str> = a.rsplitn(3, ' ').collect();
                if parts.len() == 3 {
                    timestamp_sec = parts[1].parse::<i64>().unwrap_or(0);
                    author_name = parts[2].to_string();
                } else {
                    author_name = a.to_string();
                }
            }
        } else if summary.is_empty() && !line.trim().is_empty() {
            summary = line.trim().to_string();
            break;
        }
    }

    let short_id = oid.chars().take(7).collect();
    (
        parent,
        GitCommitEntry {
            commit_id: oid.to_string(),
            short_id,
            author: author_name,
            timestamp_sec,
            summary,
        },
    )
}

fn render_simple_unified_diff(path: &str, old_text: &str, new_text: &str) -> String {
    if old_text == new_text {
        return format!("No unstaged changes in {path}.");
    }
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");

    let max_len = old_lines.len().max(new_lines.len()).min(200);
    for idx in 0..max_len {
        match (old_lines.get(idx), new_lines.get(idx)) {
            (Some(a), Some(b)) if a == b => {
                out.push_str(&format!(" {a}\n"));
            }
            (Some(a), Some(b)) => {
                out.push_str(&format!("-{a}\n+{b}\n"));
            }
            (Some(a), None) => {
                out.push_str(&format!("-{a}\n"));
            }
            (None, Some(b)) => {
                out.push_str(&format!("+{b}\n"));
            }
            (None, None) => {}
        }
    }
    out
}

fn try_system_git_status(
    workspace_root: &Path,
    branch: &str,
    head_commit: Option<String>,
) -> Option<GitRepoStatus> {
    let out = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .arg("status")
        .arg("--porcelain=v1")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut changes = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if line.len() > 3 {
            let st = line[0..2].trim().to_string();
            let path = line[3..].trim().to_string();
            changes.push(GitFileChange { status: st, path });
        }
    }
    Some(GitRepoStatus {
        is_repo: true,
        branch: branch.to_string(),
        head_commit,
        changes,
        backend: "system-git".to_string(),
    })
}

fn try_system_git_log(workspace_root: &Path, cap: usize) -> Option<Vec<GitCommitEntry>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .arg("log")
        .arg(format!("-n{cap}"))
        .arg("--format=%H%x1f%an%x1f%ct%x1f%s")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut entries = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let parts: Vec<&str> = line.split('\x1f').collect();
        if parts.len() == 4 {
            entries.push(GitCommitEntry {
                commit_id: parts[0].to_string(),
                short_id: parts[0].chars().take(7).collect(),
                author: parts[1].to_string(),
                timestamp_sec: parts[2].parse::<i64>().unwrap_or(0),
                summary: parts[3].to_string(),
            });
        }
    }
    Some(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn pure_rust_git_reads_real_git_repo_status_log_and_diff() {
        let dir = std::env::temp_dir().join("agy-pure-git-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let status = Command::new("git").arg("init").current_dir(&dir).status();
        if status.is_err() {
            return;
        }
        Command::new("git")
            .args(["config", "user.email", "dev@example.com"])
            .current_dir(&dir)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "AGY Tester"])
            .current_dir(&dir)
            .status()
            .unwrap();
        fs::write(dir.join("hello.txt"), "line one\nline two\n").unwrap();
        Command::new("git")
            .args(["add", "hello.txt"])
            .current_dir(&dir)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(&dir)
            .status()
            .unwrap();

        fs::write(dir.join("hello.txt"), "line one\nline modified\n").unwrap();
        fs::write(dir.join("untracked.rs"), "pub fn demo() {}\n").unwrap();

        let repo_status = WorkspaceGit::status(&dir).unwrap();
        assert!(repo_status.is_repo);
        assert_eq!(repo_status.backend, "rust-native-git");
        assert!(repo_status
            .changes
            .iter()
            .any(|c| c.status == "M" && c.path == "hello.txt"));
        assert!(repo_status
            .changes
            .iter()
            .any(|c| c.status == "?" && c.path == "untracked.rs"));

        let log = WorkspaceGit::log(&dir, 5).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].summary, "Initial commit");

        let diff = WorkspaceGit::diff_file(&dir, "hello.txt").unwrap();
        assert!(diff.contains("-line two"));
        assert!(diff.contains("+line modified"));

        let _ = fs::remove_dir_all(&dir);
    }
}
