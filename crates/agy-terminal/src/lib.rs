use agy_core::{MemoryBudget, MemoryPressureLevel};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TerminalError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnsiColor {
    Default,
    Red,
    Green,
    Amber,
    Blue,
    Magenta,
    Cyan,
    Muted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanStyle {
    pub fg: AnsiColor,
    pub bold: bool,
    pub underline: bool,
}

impl Default for SpanStyle {
    fn default() -> Self {
        Self {
            fg: AnsiColor::Default,
            bold: false,
            underline: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyledSpan {
    pub text: String,
    pub style: SpanStyle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalLine {
    pub seq: u64,
    pub spans: Vec<StyledSpan>,
}

impl TerminalLine {
    pub fn plain_text(&self) -> String {
        let mut s = String::new();
        for span in &self.spans {
            s.push_str(&span.text);
        }
        s
    }

    pub fn byte_len(&self) -> usize {
        self.spans.iter().map(|sp| sp.text.len() + 8).sum::<usize>() + 16
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalOutcome {
    CancelledActiveTask,
    IdleInterruptNotice,
    ClearInputBuffer,
    PendingSecondCtrlD,
    ExitSession,
}

// Bounded scrollback buffer that enforces strict line and byte limits for 1 GB RAM devices.
pub struct TerminalBuffer {
    lines: VecDeque<TerminalLine>,
    current_spans: Vec<StyledSpan>,
    current_style: SpanStyle,
    ansi_escape_buf: Option<String>,
    next_seq: u64,
    total_bytes: usize,
    max_lines: usize,
    max_bytes: usize,
    budget: MemoryBudget,
    consecutive_ctrl_d: u8,
}

impl TerminalBuffer {
    pub fn new(budget: MemoryBudget) -> Self {
        Self {
            lines: VecDeque::with_capacity(budget.max_terminal_lines.min(256)),
            current_spans: Vec::new(),
            current_style: SpanStyle::default(),
            ansi_escape_buf: None,
            next_seq: 1,
            total_bytes: 0,
            max_lines: budget.max_terminal_lines,
            max_bytes: budget.max_terminal_bytes,
            budget,
            consecutive_ctrl_d: 0,
        }
    }

    pub fn reset_ctrl_d_counter(&mut self) {
        self.consecutive_ctrl_d = 0;
    }

    pub fn handle_ctrl_d(&mut self, current_input_empty: bool) -> SignalOutcome {
        if !current_input_empty {
            self.consecutive_ctrl_d = 0;
            return SignalOutcome::ClearInputBuffer;
        }
        self.consecutive_ctrl_d = self.consecutive_ctrl_d.saturating_add(1);
        if self.consecutive_ctrl_d >= 2 {
            self.consecutive_ctrl_d = 0;
            SignalOutcome::ExitSession
        } else {
            self.push_styled_line(
                "[EOF] Press Ctrl+D again on an empty prompt (or run /exit) to close session.",
                SpanStyle {
                    fg: AnsiColor::Muted,
                    bold: false,
                    underline: false,
                },
            );
            SignalOutcome::PendingSecondCtrlD
        }
    }

    pub fn apply_memory_pressure(&mut self, pressure: MemoryPressureLevel) {
        let (lines_cap, bytes_cap) = self.budget.reduced_for_pressure(pressure);
        self.max_lines = lines_cap;
        self.max_bytes = bytes_cap;
        self.enforce_bounds();
        if matches!(
            pressure,
            MemoryPressureLevel::RunningCritical | MemoryPressureLevel::BackgroundTrim
        ) {
            self.lines.shrink_to_fit();
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.current_spans.clear();
        self.ansi_escape_buf = None;
        self.total_bytes = 0;
        self.lines.shrink_to_fit();
    }

    pub fn push_styled_line(&mut self, text: &str, style: SpanStyle) {
        self.flush_partial_if_nonempty();
        for raw_line in text.split('\n') {
            let clean = raw_line.trim_end_matches('\r');
            let line = TerminalLine {
                seq: self.alloc_seq(),
                spans: vec![StyledSpan {
                    text: clean.to_string(),
                    style,
                }],
            };
            self.push_completed_line(line);
        }
    }

    pub fn feed_stream_chunk(&mut self, chunk: &str) {
        let mut run = String::new();
        for ch in chunk.chars() {
            if let Some(mut esc) = self.ansi_escape_buf.take() {
                esc.push(ch);
                let complete = if esc.starts_with('[') {
                    esc.len() > 1 && ('@'..='~').contains(&ch)
                } else if esc.starts_with(']') {
                    ch == '\x07' || esc.ends_with("\x1b\\")
                } else {
                    true
                };
                if complete {
                    self.apply_csi_sequence(&esc);
                } else if esc.len() < 32 {
                    self.ansi_escape_buf = Some(esc);
                }
                continue;
            }

            match ch {
                '\x1b' => {
                    self.append_run(&mut run);
                    self.ansi_escape_buf = Some(String::new());
                }
                '\n' => {
                    self.append_run(&mut run);
                    let spans = std::mem::take(&mut self.current_spans);
                    let line = TerminalLine {
                        seq: self.alloc_seq(),
                        spans,
                    };
                    self.push_completed_line(line);
                }
                '\r' => {
                    // Carriage return without newline overwrites active in-progress line (spinners/progress).
                    self.append_run(&mut run);
                }
                '\x08' => {
                    if !run.is_empty() {
                        run.pop();
                    } else if let Some(last) = self.current_spans.last_mut() {
                        last.text.pop();
                    }
                }
                c if c.is_control() && c != '\t' => {}
                c => run.push(c),
            }
        }
        self.append_run(&mut run);
    }

    pub fn flush_partial_if_nonempty(&mut self) {
        if !self.current_spans.is_empty() {
            let spans = std::mem::take(&mut self.current_spans);
            let line = TerminalLine {
                seq: self.alloc_seq(),
                spans,
            };
            self.push_completed_line(line);
        }
    }

    pub fn snapshot_lines(&self) -> Vec<TerminalLine> {
        let mut out: Vec<TerminalLine> = self.lines.iter().cloned().collect();
        if !self.current_spans.is_empty() {
            out.push(TerminalLine {
                seq: self.next_seq,
                spans: self.current_spans.clone(),
            });
        }
        out
    }

    pub fn export_plain_text(&self, max_lines: usize) -> String {
        let snap = self.snapshot_lines();
        let start = snap.len().saturating_sub(max_lines);
        snap[start..]
            .iter()
            .map(TerminalLine::plain_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn stats(&self) -> (usize, usize, usize, usize) {
        (
            self.lines.len(),
            self.total_bytes,
            self.max_lines,
            self.max_bytes,
        )
    }

    fn alloc_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        seq
    }

    fn append_run(&mut self, run: &mut String) {
        if run.is_empty() {
            return;
        }
        let text = std::mem::take(run);
        if let Some(last) = self.current_spans.last_mut() {
            if last.style == self.current_style {
                last.text.push_str(&text);
                return;
            }
        }
        self.current_spans.push(StyledSpan {
            text,
            style: self.current_style,
        });
    }

    fn apply_csi_sequence(&mut self, esc: &str) {
        if !esc.starts_with('[') {
            return;
        }
        let body = &esc[1..];
        let Some(final_byte) = body.chars().last() else {
            return;
        };
        let params = &body[..body.len() - final_byte.len_utf8()];
        match final_byte {
            'm' => {
                if params.is_empty() {
                    self.current_style = SpanStyle::default();
                    return;
                }
                for code_str in params.split(';') {
                    match code_str.parse::<u16>().unwrap_or(0) {
                        0 => self.current_style = SpanStyle::default(),
                        1 => self.current_style.bold = true,
                        2 => self.current_style.fg = AnsiColor::Muted,
                        4 => self.current_style.underline = true,
                        22 => self.current_style.bold = false,
                        24 => self.current_style.underline = false,
                        31 | 91 => self.current_style.fg = AnsiColor::Red,
                        32 | 92 => self.current_style.fg = AnsiColor::Green,
                        33 | 93 => self.current_style.fg = AnsiColor::Amber,
                        34 | 94 => self.current_style.fg = AnsiColor::Blue,
                        35 | 95 => self.current_style.fg = AnsiColor::Magenta,
                        36 | 96 => self.current_style.fg = AnsiColor::Cyan,
                        37 | 39 | 97 => self.current_style.fg = AnsiColor::Default,
                        90 => self.current_style.fg = AnsiColor::Muted,
                        _ => {}
                    }
                }
            }
            'J' if params == "2" || params == "3" => {
                self.clear();
            }
            'K' => {
                self.current_spans.clear();
            }
            _ => {}
        }
    }

    fn push_completed_line(&mut self, mut line: TerminalLine) {
        // Bound individual runaway lines (e.g. minified JS) to avoid single-line OOM on 1 GB devices.
        let max_line_chars = 2048;
        let mut seen = 0usize;
        for span in &mut line.spans {
            if seen >= max_line_chars {
                span.text.clear();
            } else if seen + span.text.len() > max_line_chars {
                let allowed = max_line_chars - seen;
                let cut = span
                    .text
                    .char_indices()
                    .map(|(i, _)| i)
                    .take_while(|&i| i <= allowed)
                    .last()
                    .unwrap_or(0);
                span.text.truncate(cut);
                span.text.push_str(" ...[truncated]");
                seen = max_line_chars;
            } else {
                seen += span.text.len();
            }
        }
        line.spans.retain(|s| !s.text.is_empty());

        self.total_bytes = self.total_bytes.saturating_add(line.byte_len());
        self.lines.push_back(line);
        self.enforce_bounds();
    }

    fn enforce_bounds(&mut self) {
        while self.lines.len() > self.max_lines
            || (self.total_bytes > self.max_bytes && self.lines.len() > 1)
        {
            if let Some(removed) = self.lines.pop_front() {
                self.total_bytes = self.total_bytes.saturating_sub(removed.byte_len());
            } else {
                break;
            }
        }
    }
}

pub struct CommandHistory {
    entries: VecDeque<String>,
    cursor: Option<usize>,
    max_entries: usize,
}

impl CommandHistory {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(max_entries.min(128)),
            cursor: None,
            max_entries: max_entries.max(16),
        }
    }

    pub fn load_from_file(path: &Path, max_entries: usize) -> Self {
        let mut hist = Self::new(max_entries);
        if let Ok(raw) = fs::read_to_string(path) {
            if let Ok(list) = serde_json::from_str::<Vec<String>>(&raw) {
                for item in list {
                    hist.record(&item);
                }
            }
        }
        hist
    }

    pub fn save_to_file(&self, path: &Path) -> Result<(), TerminalError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let vec: Vec<&String> = self.entries.iter().collect();
        fs::write(path, serde_json::to_vec(&vec)?)?;
        Ok(())
    }

    pub fn record(&mut self, command: &str) {
        let trimmed = command.trim();
        if trimmed.is_empty() {
            self.cursor = None;
            return;
        }
        if self.entries.back().map(|s| s.as_str()) == Some(trimmed) {
            self.cursor = None;
            return;
        }
        if self.entries.len() == self.max_entries {
            self.entries.pop_front();
        }
        self.entries.push_back(trimmed.to_string());
        self.cursor = None;
    }

    pub fn previous(&mut self) -> Option<&str> {
        if self.entries.is_empty() {
            return None;
        }
        let next_idx = match self.cursor {
            None => self.entries.len() - 1,
            Some(0) => 0,
            Some(idx) => idx - 1,
        };
        self.cursor = Some(next_idx);
        self.entries.get(next_idx).map(|s| s.as_str())
    }

    pub fn next(&mut self) -> Option<&str> {
        let idx = self.cursor?;
        if idx + 1 >= self.entries.len() {
            self.cursor = None;
            None
        } else {
            self.cursor = Some(idx + 1);
            self.entries.get(idx + 1).map(|s| s.as_str())
        }
    }

    pub fn entries(&self) -> Vec<String> {
        self.entries.iter().cloned().collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedTerminalCommand {
    Help,
    Status,
    RuntimeInfo,
    MemoryStats,
    Clear,
    Exit,
    ListModels,
    SetModel(String),
    SetMode(String),
    ListSessions,
    NewSession(Option<String>),
    ResumeSession(String),
    Approve(Option<String>),
    Deny(Option<String>),
    WorkspaceTree(usize),
    WorkspaceSearch(String),
    CatFile {
        path: String,
        start_line: usize,
        end_line: usize,
    },
    GitSubcommand(String),
    ShellCommand(String),
    AgentPrompt(String),
}

pub fn parse_terminal_input(raw: &str) -> Option<ParsedTerminalCommand> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(shell_cmd) = trimmed.strip_prefix('!') {
        let cmd = shell_cmd.trim();
        if cmd.is_empty() {
            return Some(ParsedTerminalCommand::Help);
        }
        return Some(ParsedTerminalCommand::ShellCommand(cmd.to_string()));
    }
    if !trimmed.starts_with('/') {
        return Some(ParsedTerminalCommand::AgentPrompt(trimmed.to_string()));
    }

    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();

    match head {
        "/help" | "/?" => Some(ParsedTerminalCommand::Help),
        "/status" => Some(ParsedTerminalCommand::Status),
        "/runtime" => Some(ParsedTerminalCommand::RuntimeInfo),
        "/mem" => Some(ParsedTerminalCommand::MemoryStats),
        "/clear" => Some(ParsedTerminalCommand::Clear),
        "/exit" | "/quit" => Some(ParsedTerminalCommand::Exit),
        "/models" => Some(ParsedTerminalCommand::ListModels),
        "/model" => {
            if rest.is_empty() {
                Some(ParsedTerminalCommand::ListModels)
            } else {
                Some(ParsedTerminalCommand::SetModel(rest.to_string()))
            }
        }
        "/plan" => Some(ParsedTerminalCommand::SetMode("plan".to_string())),
        "/mode" => Some(ParsedTerminalCommand::SetMode(rest.to_string())),
        "/sessions" => Some(ParsedTerminalCommand::ListSessions),
        "/new" => Some(ParsedTerminalCommand::NewSession(if rest.is_empty() {
            None
        } else {
            Some(rest.to_string())
        })),
        "/resume" => Some(ParsedTerminalCommand::ResumeSession(rest.to_string())),
        "/approve" => Some(ParsedTerminalCommand::Approve(if rest.is_empty() {
            None
        } else {
            Some(rest.to_string())
        })),
        "/deny" => Some(ParsedTerminalCommand::Deny(if rest.is_empty() {
            None
        } else {
            Some(rest.to_string())
        })),
        "/tree" => {
            let depth = rest.parse::<usize>().unwrap_or(3).clamp(1, 8);
            Some(ParsedTerminalCommand::WorkspaceTree(depth))
        }
        "/search" => Some(ParsedTerminalCommand::WorkspaceSearch(rest.to_string())),
        "/cat" => {
            let mut args = rest.split_whitespace();
            let path = args.next().unwrap_or("").to_string();
            let start_line = args
                .next()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1);
            let end_line = args
                .next()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(start_line.saturating_add(79));
            Some(ParsedTerminalCommand::CatFile {
                path,
                start_line,
                end_line,
            })
        }
        "/git" => Some(ParsedTerminalCommand::GitSubcommand(if rest.is_empty() {
            "status".to_string()
        } else {
            rest.to_string()
        })),
        _ => Some(ParsedTerminalCommand::AgentPrompt(trimmed.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_parser_and_ring_buffer_bound_memory() {
        let budget = MemoryBudget::for_device_ram_mb(1024);
        let mut term = TerminalBuffer::new(budget);

        term.feed_stream_chunk("\x1b[1;33mWARN:\x1b[0m low ram check\n");
        let snap = term.snapshot_lines();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].plain_text(), "WARN: low ram check");
        assert_eq!(snap[0].spans[0].style.fg, AnsiColor::Amber);
        assert!(snap[0].spans[0].style.bold);

        for idx in 0..1000 {
            term.feed_stream_chunk(&format!("streaming output row {idx}\n"));
        }
        let (line_count, byte_count, max_l, max_b) = term.stats();
        assert!(line_count <= max_l);
        assert!(byte_count <= max_b);

        term.apply_memory_pressure(MemoryPressureLevel::RunningCritical);
        let (crit_lines, _, _, _) = term.stats();
        assert!(crit_lines <= 150);
    }

    #[test]
    fn double_ctrl_d_and_history_navigation_work() {
        let budget = MemoryBudget::for_device_ram_mb(1024);
        let mut term = TerminalBuffer::new(budget);
        assert_eq!(term.handle_ctrl_d(false), SignalOutcome::ClearInputBuffer);
        assert_eq!(term.handle_ctrl_d(true), SignalOutcome::PendingSecondCtrlD);
        assert_eq!(term.handle_ctrl_d(true), SignalOutcome::ExitSession);

        let mut hist = CommandHistory::new(10);
        hist.record("/status");
        hist.record("/tree 2");
        assert_eq!(hist.previous(), Some("/tree 2"));
        assert_eq!(hist.previous(), Some("/status"));
        assert_eq!(hist.next(), Some("/tree 2"));
        assert_eq!(hist.next(), None);
    }
}
