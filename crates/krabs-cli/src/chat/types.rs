use krabs_core::{Message, TokenUsage, ToolCall, UserInputRequest};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use tokio::sync::oneshot;

// ── chat message types ───────────────────────────────────────────────────────

#[derive(Clone)]
pub(super) enum ChatMsg {
    User(String),
    Assistant(String),
    ToolCall(String),
    ToolResult(String),
    Usage(u32, u32),
    Info(String),
    Error(String),
}

impl ChatMsg {
    pub(super) fn to_lines(&self) -> Vec<Line<'static>> {
        match self {
            ChatMsg::User(t) => vec![
                Line::from(vec![
                    Span::styled(
                        " you ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(t.clone(), Style::default().fg(Color::Cyan)),
                ]),
                Line::raw(""),
            ],
            ChatMsg::Assistant(t) => {
                let mut lines = vec![Line::from(Span::styled(
                    " krabs ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ))];
                for l in t.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {l}"),
                        Style::default().fg(Color::White),
                    )));
                }
                lines.push(Line::raw(""));
                lines
            }
            ChatMsg::ToolCall(t) => vec![Line::from(vec![
                Span::styled(
                    "  ⚙ ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(t.clone(), Style::default().fg(Color::Yellow)),
            ])],
            ChatMsg::ToolResult(t) => {
                let mut lines: Vec<Line> = t
                    .lines()
                    .take(40)
                    .map(|l| {
                        Line::from(Span::styled(
                            format!("    {l}"),
                            Style::default().fg(Color::DarkGray),
                        ))
                    })
                    .collect();
                lines.push(Line::raw(""));
                lines
            }
            ChatMsg::Usage(i, o) => vec![
                Line::from(Span::styled(
                    format!("  [{i} in / {o} out tokens]"),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::raw(""),
            ],
            ChatMsg::Info(t) => vec![
                Line::from(Span::styled(
                    format!("  {t}"),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                )),
                Line::raw(""),
            ],
            ChatMsg::Error(t) => vec![
                Line::from(vec![
                    Span::styled(" error ", Style::default().fg(Color::White).bg(Color::Red)),
                    Span::raw("  "),
                    Span::styled(t.clone(), Style::default().fg(Color::Red)),
                ]),
                Line::raw(""),
            ],
        }
    }
}

// ── display events from background task ─────────────────────────────────────

pub(super) enum DisplayEvent {
    Token(String),
    /// Sent before a tool runs; background task waits on `respond`.
    PermissionRequest {
        tool_name: String,
        args: String,
        respond: oneshot::Sender<bool>,
    },
    /// Sent by `ask_user` tool; TUI renders a choice popup and blocks the agent.
    UserInput(UserInputRequest),
    ToolCallStart(ToolCall),
    ToolResultEnd(String),
    TurnUsage(TokenUsage),
    Done {
        messages: Vec<Message>,
        session_id: Option<String>,
    },
    Error {
        message: String,
        session_id: Option<String>,
    },
    Status(String),
}

/// Active permission prompt waiting for a user keypress.
pub(super) struct PendingPermission {
    pub(super) tool_name: String,
    pub(super) args: String,
    pub(super) respond: oneshot::Sender<bool>,
}

/// Active user-input prompt rendered as a TUI popup.
pub(super) struct PendingUserInput {
    pub(super) mode: krabs_core::InputMode,
    pub(super) question: String,
    /// Choices shown to the user (options + "custom…" appended).
    pub(super) options: Vec<String>,
    /// For ChooseMany: which indices are checked.
    pub(super) selected: Vec<bool>,
    /// Highlighted / focused index.
    pub(super) cursor: usize,
    /// True when the user is typing a custom free-text answer.
    pub(super) custom_mode: bool,
    pub(super) custom_text: String,
    pub(super) custom_cursor: usize,
    pub(super) respond: oneshot::Sender<String>,
}

pub(super) struct InfoBar {
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) cwd: String,
    pub(super) tools: String,
}

pub(super) fn estimate_tokens(s: &str) -> u32 {
    ((s.len() as f32) / 4.0).ceil() as u32
}

pub(super) fn fmt_k(n: u32) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f32 / 1000.0)
    } else {
        format!("{}", n)
    }
}
