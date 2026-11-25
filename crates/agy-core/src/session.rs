use crate::config::AgentMode;
use crate::CoreError;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub id: String,
    pub name: String,
    pub arguments_json: String,
    pub output_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnMessage {
    pub timestamp_ms: u64,
    pub role: Role,
    pub content: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    OverwriteFile { relative_path: String, bytes: u64 },
    DeleteFile { relative_path: String },
    ExecuteCommand { command: String, cwd: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalAction {
    ApproveOnce,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApproval {
    pub id: String,
    pub session_id: String,
    pub created_at_ms: u64,
    pub kind: ApprovalKind,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHeader {
    pub id: String,
    pub title: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub model: String,
    pub mode: AgentMode,
    pub workspace_path: Option<String>,
    pub turn_count: u32,
}

pub struct SessionStore {
    root_dir: PathBuf,
}

impl SessionStore {
    pub fn new(root_dir: PathBuf) -> Result<Self, CoreError> {
        fs::create_dir_all(&root_dir)?;
        Ok(Self { root_dir })
    }

    pub fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    fn header_path(&self, session_id: &str) -> PathBuf {
        let clean = sanitize_id(session_id);
        self.root_dir.join(format!("{clean}.meta.json"))
    }

    fn turns_path(&self, session_id: &str) -> PathBuf {
        let clean = sanitize_id(session_id);
        self.root_dir.join(format!("{clean}.turns.jsonl"))
    }

    pub fn create_session(
        &self,
        title: &str,
        model: &str,
        mode: AgentMode,
        workspace_path: Option<String>,
    ) -> Result<SessionHeader, CoreError> {
        let now = Self::now_ms();
        let short = format!("s-{now:x}");
        let header = SessionHeader {
            id: short,
            title: if title.trim().is_empty() {
                "Terminal session".to_string()
            } else {
                title.trim().chars().take(72).collect()
            },
            created_at_ms: now,
            updated_at_ms: now,
            model: model.to_string(),
            mode,
            workspace_path,
            turn_count: 0,
        };
        self.save_header(&header)?;
        File::create(self.turns_path(&header.id))?;
        Ok(header)
    }

    pub fn save_header(&self, header: &SessionHeader) -> Result<(), CoreError> {
        let path = self.header_path(&header.id);
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(header)?)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn load_header(&self, session_id: &str) -> Result<SessionHeader, CoreError> {
        let path = self.header_path(session_id);
        if !path.exists() {
            return Err(CoreError::SessionNotFound(session_id.to_string()));
        }
        let raw = fs::read(&path)?;
        Ok(serde_json::from_slice(&raw)?)
    }

    // Reads only small `.meta.json` headers so listing sessions never loads full transcripts into RAM.
    pub fn list_sessions(&self) -> Result<Vec<SessionHeader>, CoreError> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.root_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with(".meta.json"))
                .unwrap_or(false)
            {
                if let Ok(raw) = fs::read(&path) {
                    if let Ok(h) = serde_json::from_slice::<SessionHeader>(&raw) {
                        out.push(h);
                    }
                }
            }
        }
        out.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));
        Ok(out)
    }

    pub fn append_turn(&self, session_id: &str, turn: &TurnMessage) -> Result<SessionHeader, CoreError> {
        let mut header = self.load_header(session_id)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.turns_path(session_id))?;
        let line = serde_json::to_string(turn)?;
        writeln!(file, "{line}")?;
        file.flush()?;

        header.updated_at_ms = turn.timestamp_ms.max(Self::now_ms());
        header.turn_count = header.turn_count.saturating_add(1);
        if header.turn_count == 1 && turn.role == Role::User && header.title == "Terminal session" {
            let derived: String = turn.content.lines().next().unwrap_or("Session").chars().take(56).collect();
            if !derived.trim().is_empty() {
                header.title = derived.trim().to_string();
            }
        }
        self.save_header(&header)?;
        Ok(header)
    }

    // Streams JSONL line-by-line and keeps only the most recent `limit` turns in memory.
    pub fn load_recent_turns(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<TurnMessage>, CoreError> {
        let path = self.turns_path(session_id);
        if !path.exists() {
            return Err(CoreError::SessionNotFound(session_id.to_string()));
        }
        let file = File::open(&path)?;
        let reader = BufReader::with_capacity(8 * 1024, file);
        let cap = limit.max(1);
        let mut ring = std::collections::VecDeque::with_capacity(cap.min(64));
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(msg) = serde_json::from_str::<TurnMessage>(&line) {
                if ring.len() == cap {
                    ring.pop_front();
                }
                ring.push_back(msg);
            }
        }
        Ok(ring.into_iter().collect())
    }

    pub fn delete_session(&self, session_id: &str) -> Result<(), CoreError> {
        let hp = self.header_path(session_id);
        let tp = self.turns_path(session_id);
        if hp.exists() {
            fs::remove_file(hp)?;
        }
        if tp.exists() {
            fs::remove_file(tp)?;
        }
        Ok(())
    }

    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }
}

fn sanitize_id(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_store_streams_bounded_recent_turns() {
        let dir = std::env::temp_dir().join(format!("agy-core-test-{}", SessionStore::now_ms()));
        let store = SessionStore::new(dir.clone()).unwrap();
        let session = store
            .create_session("", "gemini-2.5-flash", AgentMode::Standard, None)
            .unwrap();
        for idx in 0..10 {
            store
                .append_turn(
                    &session.id,
                    &TurnMessage {
                        timestamp_ms: SessionStore::now_ms() + idx,
                        role: if idx % 2 == 0 {
                            Role::User
                        } else {
                            Role::Assistant
                        },
                        content: format!("message {idx}"),
                        tool_calls: Vec::new(),
                    },
                )
                .unwrap();
        }
        let recent = store.load_recent_turns(&session.id, 4).unwrap();
        assert_eq!(recent.len(), 4);
        assert_eq!(recent[0].content, "message 6");
        assert_eq!(recent[3].content, "message 9");

        let headers = store.list_sessions().unwrap();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].turn_count, 10);
        assert_eq!(headers[0].title, "message 0");
        let _ = fs::remove_dir_all(dir);
    }
}
