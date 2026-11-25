pub mod git;

use agy_core::{ApprovalKind, PermissionPolicy};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

pub use git::{GitCommitEntry, GitFileChange, GitRepoStatus, WorkspaceGit};

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("No workspace selected: open a folder via Storage Access Framework or create a local project")]
    NoActiveWorkspace,
    #[error("Workspace boundary violation: path '{0}' escapes the permitted workspace root")]
    BoundaryViolation(String),
    #[error("Access denied for sensitive credential file '{0}'")]
    SensitiveFileBlocked(String),
    #[error("File '{0}' appears to be binary and cannot be edited as UTF-8 text")]
    BinaryFile(String),
    #[error("Target snippet was not found in '{0}'")]
    EditTargetNotFound(String),
    #[error("Read-only mode active: write and delete operations are blocked")]
    ReadOnlyBlocked,
    #[error("File not found: '{0}'")]
    NotFound(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeEntry {
    pub relative_path: String,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReadSlice {
    pub relative_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub total_lines: usize,
    pub truncated_by_bytes: bool,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchMatch {
    pub relative_path: String,
    pub line_number: usize,
    pub preview: String,
}

#[derive(Debug, Clone)]
pub struct WorkspaceManager {
    root: PathBuf,
    dirty_saf_paths: Vec<String>,
}

impl WorkspaceManager {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        let p = root.as_ref();
        fs::create_dir_all(p)?;
        let canonical = p.canonicalize()?;
        Ok(Self {
            root: canonical,
            dirty_saf_paths: Vec::new(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn take_dirty_saf_paths(&mut self) -> Vec<String> {
        std::mem::take(&mut self.dirty_saf_paths)
    }

    // Resolves a workspace-relative path and rejects `..` components or symlinks escaping the root.
    pub fn resolve_safe_path(&self, rel: &str) -> Result<(PathBuf, String), WorkspaceError> {
        let trimmed = rel.trim().trim_start_matches("./");
        if trimmed.is_empty() || trimmed == "." {
            return Ok((self.root.clone(), ".".to_string()));
        }
        let candidate_rel = Path::new(trimmed);
        if candidate_rel.is_absolute() {
            return Err(WorkspaceError::BoundaryViolation(rel.to_string()));
        }
        let mut normalized_parts = Vec::new();
        for comp in candidate_rel.components() {
            match comp {
                Component::Normal(os) => {
                    let s = os.to_string_lossy();
                    normalized_parts.push(s.into_owned());
                }
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(WorkspaceError::BoundaryViolation(rel.to_string()));
                }
            }
        }
        if normalized_parts.is_empty() {
            return Ok((self.root.clone(), ".".to_string()));
        }

        let normalized_rel = normalized_parts.join("/");
        if is_blocked_credential_file(&normalized_rel) {
            return Err(WorkspaceError::SensitiveFileBlocked(normalized_rel));
        }

        let full = self.root.join(&normalized_rel);
        if full.exists() {
            let canon = full.canonicalize()?;
            if !canon.starts_with(&self.root) {
                return Err(WorkspaceError::BoundaryViolation(normalized_rel));
            }
            Ok((canon, normalized_rel))
        } else {
            let mut ancestor = full.parent();
            while let Some(anc) = ancestor {
                if anc.exists() {
                    let canon_anc = anc.canonicalize()?;
                    if !canon_anc.starts_with(&self.root) {
                        return Err(WorkspaceError::BoundaryViolation(normalized_rel));
                    }
                    break;
                }
                ancestor = anc.parent();
            }
            Ok((full, normalized_rel))
        }
    }

    pub fn list_tree(&self, max_depth: usize, max_entries: usize) -> Result<Vec<TreeEntry>, WorkspaceError> {
        let mut entries = Vec::new();
        self.walk_tree(&self.root, 0, max_depth.clamp(1, 8), max_entries.max(16), &mut entries)?;
        Ok(entries)
    }

    fn walk_tree(
        &self,
        dir: &Path,
        depth: usize,
        max_depth: usize,
        max_entries: usize,
        out: &mut Vec<TreeEntry>,
    ) -> Result<(), WorkspaceError> {
        if depth >= max_depth || out.len() >= max_entries {
            return Ok(());
        }
        let mut children: Vec<_> = fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .collect();
        children.sort_by(|a, b| {
            let a_dir = a.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let b_dir = b.file_type().map(|t| t.is_dir()).unwrap_or(false);
            b_dir
                .cmp(&a_dir)
                .then_with(|| a.file_name().cmp(&b.file_name()))
        });

        for child in children {
            if out.len() >= max_entries {
                break;
            }
            let name = child.file_name().to_string_lossy().into_owned();
            if should_skip_dir(&name) {
                continue;
            }
            let path = child.path();
            let meta = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            let is_dir = meta.is_dir();
            let rel = path
                .strip_prefix(&self.root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push(TreeEntry {
                relative_path: rel,
                name,
                is_dir,
                depth,
                size_bytes: if is_dir { 0 } else { meta.len() },
            });
            if is_dir {
                self.walk_tree(&path, depth + 1, max_depth, max_entries, out)?;
            }
        }
        Ok(())
    }

    // Streams lines in 8 KB chunks so multi-megabyte files stay safe on 1 GB RAM devices.
    pub fn read_lines_streaming(
        &self,
        rel_path: &str,
        start_line: usize,
        end_line: usize,
        max_bytes: usize,
    ) -> Result<FileReadSlice, WorkspaceError> {
        let (full, norm_rel) = self.resolve_safe_path(rel_path)?;
        if !full.is_file() {
            return Err(WorkspaceError::NotFound(norm_rel));
        }
        if file_has_null_bytes(&full)? {
            return Err(WorkspaceError::BinaryFile(norm_rel));
        }

        let start = start_line.max(1);
        let end = end_line.max(start);
        let file = File::open(&full)?;
        let reader = BufReader::with_capacity(8 * 1024, file);

        let mut content = String::new();
        let mut total_lines = 0usize;
        let mut truncated = false;
        let byte_cap = max_bytes.max(4096);

        for line_res in reader.lines() {
            let line = line_res?;
            total_lines += 1;
            if total_lines >= start && total_lines <= end && !truncated {
                if content.len() + line.len() + 12 > byte_cap {
                    truncated = true;
                } else {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str(&format!("{total_lines:>4} | {line}"));
                }
            }
        }

        Ok(FileReadSlice {
            relative_path: norm_rel,
            start_line: start,
            end_line: end.min(total_lines.max(1)),
            total_lines,
            truncated_by_bytes: truncated,
            content,
        })
    }

    pub fn read_raw_bounded(&self, rel_path: &str, max_bytes: usize) -> Result<String, WorkspaceError> {
        let (full, norm_rel) = self.resolve_safe_path(rel_path)?;
        if !full.is_file() {
            return Err(WorkspaceError::NotFound(norm_rel));
        }
        if file_has_null_bytes(&full)? {
            return Err(WorkspaceError::BinaryFile(norm_rel));
        }
        let f = File::open(&full)?;
        let mut buf = Vec::new();
        f.take(max_bytes as u64).read_to_end(&mut buf)?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    pub fn search_files_streaming(
        &self,
        query: &str,
        max_hits: usize,
    ) -> Result<Vec<SearchMatch>, WorkspaceError> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let mut hits = Vec::new();
        let entries = self.list_tree(6, 800)?;
        for entry in entries {
            if entry.is_dir || entry.size_bytes > 2 * 1024 * 1024 {
                continue;
            }
            if hits.len() >= max_hits {
                break;
            }
            let (full, norm_rel) = match self.resolve_safe_path(&entry.relative_path) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if file_has_null_bytes(&full).unwrap_or(true) {
                continue;
            }
            let Ok(file) = File::open(&full) else {
                continue;
            };
            let reader = BufReader::with_capacity(8 * 1024, file);
            for (idx, line_res) in reader.lines().enumerate() {
                let Ok(line) = line_res else {
                    break;
                };
                if line.to_lowercase().contains(&needle) {
                    let preview: String = line.trim().chars().take(140).collect();
                    hits.push(SearchMatch {
                        relative_path: norm_rel.clone(),
                        line_number: idx + 1,
                        preview,
                    });
                    if hits.len() >= max_hits {
                        break;
                    }
                }
            }
        }
        Ok(hits)
    }

    pub fn check_write_approval_needed(
        &self,
        policy: PermissionPolicy,
        rel_path: &str,
        new_bytes: u64,
    ) -> Result<Option<ApprovalKind>, WorkspaceError> {
        if policy == PermissionPolicy::ReadOnlyStrict {
            return Err(WorkspaceError::ReadOnlyBlocked);
        }
        let (full, norm_rel) = self.resolve_safe_path(rel_path)?;
        let exists = full.exists();
        match policy {
            PermissionPolicy::ConfirmAllWrites => Ok(Some(ApprovalKind::OverwriteFile {
                relative_path: norm_rel,
                bytes: new_bytes,
            })),
            PermissionPolicy::ConfirmDestructiveAndCommands => {
                if exists {
                    Ok(Some(ApprovalKind::OverwriteFile {
                        relative_path: norm_rel,
                        bytes: new_bytes,
                    }))
                } else {
                    Ok(None)
                }
            }
            PermissionPolicy::ReadOnlyStrict => Err(WorkspaceError::ReadOnlyBlocked),
        }
    }

    pub fn write_file_atomic(&mut self, rel_path: &str, content: &str) -> Result<String, WorkspaceError> {
        let (full, norm_rel) = self.resolve_safe_path(rel_path)?;
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = full.with_extension("agy-write.tmp");
        fs::write(&tmp, content.as_bytes())?;
        fs::rename(&tmp, &full)?;
        self.mark_dirty(&norm_rel);
        Ok(norm_rel)
    }

    pub fn edit_file_replace(
        &mut self,
        rel_path: &str,
        target_snippet: &str,
        replacement: &str,
    ) -> Result<String, WorkspaceError> {
        let (full, norm_rel) = self.resolve_safe_path(rel_path)?;
        if !full.is_file() {
            return Err(WorkspaceError::NotFound(norm_rel));
        }
        if file_has_null_bytes(&full)? {
            return Err(WorkspaceError::BinaryFile(norm_rel));
        }
        let original = fs::read_to_string(&full)?;
        if !original.contains(target_snippet) {
            return Err(WorkspaceError::EditTargetNotFound(norm_rel));
        }
        let updated = original.replacen(target_snippet, replacement, 1);
        self.write_file_atomic(&norm_rel, &updated)
    }

    pub fn delete_file(&mut self, rel_path: &str) -> Result<String, WorkspaceError> {
        let (full, norm_rel) = self.resolve_safe_path(rel_path)?;
        if !full.exists() {
            return Err(WorkspaceError::NotFound(norm_rel));
        }
        fs::remove_file(&full)?;
        self.mark_dirty(&format!("DELETED:{norm_rel}"));
        Ok(norm_rel)
    }

    fn mark_dirty(&mut self, entry: &str) {
        if !self.dirty_saf_paths.iter().any(|p| p == entry) {
            self.dirty_saf_paths.push(entry.to_string());
        }
    }
}

fn should_skip_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "target" | "node_modules" | ".gradle" | "build" | "__pycache__"
    )
}

fn is_blocked_credential_file(rel: &str) -> bool {
    let file_name = Path::new(rel)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    file_name == ".env"
        || file_name.starts_with(".env.")
        || file_name.ends_with(".keystore")
        || file_name.ends_with(".jks")
        || file_name == "oauth_creds.json"
        || file_name == "antigravity-oauth-token"
}

fn file_has_null_bytes(path: &Path) -> Result<bool, std::io::Error> {
    let mut file = File::open(path)?;
    let mut sample = [0u8; 1024];
    let n = file.read(&mut sample)?;
    Ok(sample[..n].contains(&0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_blocks_traversal_and_sensitive_files() {
        let root = std::env::temp_dir().join("agy-ws-boundary-test");
        let _ = fs::remove_dir_all(&root);
        let mut ws = WorkspaceManager::open(&root).unwrap();

        assert!(matches!(
            ws.resolve_safe_path("../etc/passwd"),
            Err(WorkspaceError::BoundaryViolation(_))
        ));
        assert!(matches!(
            ws.resolve_safe_path("/etc/hosts"),
            Err(WorkspaceError::BoundaryViolation(_))
        ));
        assert!(matches!(
            ws.resolve_safe_path("config/.env"),
            Err(WorkspaceError::SensitiveFileBlocked(_))
        ));

        ws.write_file_atomic("src/main.rs", "fn main() {\n    println!(\"hi\");\n}\n")
            .unwrap();
        let slice = ws.read_lines_streaming("src/main.rs", 1, 10, 8192).unwrap();
        assert_eq!(slice.total_lines, 3);
        assert!(slice.content.contains("println!"));

        ws.edit_file_replace("src/main.rs", "hi", "agy-mobile")
            .unwrap();
        let hits = ws.search_files_streaming("agy-mobile", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line_number, 2);

        let _ = fs::remove_dir_all(&root);
    }
}
