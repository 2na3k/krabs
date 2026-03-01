use std::collections::HashSet;

use krabs_core::AgentPersona;

use super::types::{ChatMsg, PendingPermission, PendingUserInput};

// ── app state ────────────────────────────────────────────────────────────────

pub(super) struct App {
    pub(super) chat: Vec<ChatMsg>,
    pub(super) input: String,
    pub(super) cursor: usize,
    pub(super) scroll: u16,
    pub(super) auto_scroll: bool,
    pub(super) history: Vec<String>,
    pub(super) history_idx: Option<usize>,
    pub(super) spinning: bool,
    pub(super) spin_i: usize,
    pub(super) total_input: u32,
    pub(super) total_output: u32,
    pub(super) suggest_idx: Option<usize>, // selected index in suggestion popup
    pub(super) active_persona: Option<AgentPersona>,
    pub(super) system_prompt_text: String,
    pub(super) persona_text: String,
    pub(super) tools_text: String,
    pub(super) memory_text: String,
    pub(super) personas: Vec<AgentPersona>,
    /// Tools approved with "always allow" — no prompt on subsequent calls.
    pub(super) approved_tools: HashSet<String>,
    /// Active permission prompt waiting for y / a / n keypress.
    pub(super) pending_permission: Option<PendingPermission>,
    /// Active user-input popup waiting for the user to select / confirm.
    pub(super) pending_user_input: Option<PendingUserInput>,
}

impl App {
    pub(super) fn new() -> Self {
        Self {
            chat: Vec::new(),
            input: String::new(),
            cursor: 0,
            scroll: 0,
            auto_scroll: true,
            history: Vec::new(),
            history_idx: None,
            spinning: false,
            suggest_idx: None,
            spin_i: 0,
            total_input: 0,
            total_output: 0,
            active_persona: None,
            personas: Vec::new(),
            approved_tools: HashSet::new(),
            pending_permission: None,
            pending_user_input: None,
            system_prompt_text: String::new(),
            persona_text: String::new(),
            tools_text: String::new(),
            memory_text: String::new(),
        }
    }

    pub(super) fn push(&mut self, msg: ChatMsg) {
        self.chat.push(msg);
        if self.auto_scroll {
            self.scroll = u16::MAX;
        }
    }

    pub(super) fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub(super) fn backspace(&mut self) {
        if self.cursor > 0 {
            let i = self.input[..self.cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.input.drain(i..self.cursor);
            self.cursor = i;
        }
    }

    pub(super) fn cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.input[..self.cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    pub(super) fn cursor_right(&mut self) {
        if self.cursor < self.input.len() {
            let n = self.input[self.cursor..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
            self.cursor += n;
        }
    }
}

// ── error formatting ──────────────────────────────────────────────────────────

pub(super) fn extract_api_error(raw: &str) -> String {
    // Try to find JSON in the error string and extract the message field
    if let Some(start) = raw.find('[').or_else(|| raw.find('{')) {
        let json_str = &raw[start..];
        if let Ok(v) = json_str.parse::<serde_json::Value>() {
            // Handle array wrapper [ { "error": { "message": "..." } } ]
            let obj: serde_json::Value = if v.is_array() { v[0].clone() } else { v };
            if let Some(msg) = obj["error"]["message"].as_str() {
                // Trim after ". Please refer to" for brevity
                let trimmed: &str = msg.split(". Please refer to").next().unwrap_or(msg).trim();
                return format!("API error: {}", trimmed);
            }
        }
    }
    // Fallback: strip the verbose HTTP preamble, keep from "status" onward
    if let Some(pos) = raw.find("status ") {
        return raw[pos..].to_string();
    }
    raw.to_string()
}
