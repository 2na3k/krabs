use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use krabs_core::{
    skills::loader::SkillLoader, AgentPersona, BaseAgent, BashTool, Credentials, CustomModelEntry,
    DelegateTool, DispatchTool, GlobTool, GrepTool, HookConfig, HookEntry, KrabsConfig,
    LlmProvider, McpRegistry, McpServer, Message, ReadTool, Role, SkillsConfig, StreamChunk,
    TokenUsage, ToolCall, ToolRegistry, WebFetchTool, WriteTool,
};
use krabs_core::{InputMode, UserInputRequest, UserInputTool};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use std::{collections::HashSet, io, sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};

// ── constants ────────────────────────────────────────────────────────────────

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/tools", "list available tools"),
    ("/skills", "list project skills"),
    (
        "/mcp",
        "list/add/remove MCP servers  usage: /mcp [list|add|add-sse|remove|tools]",
    ),
    (
        "/hooks",
        "list/add/remove hooks  usage: /hooks [list|add|remove]",
    ),
    ("/agents", "list agent personas  |  use @<name> to activate"),
    (
        "/models",
        "list or switch models  usage: /models [<model> | <provider> <model>]",
    ),
    ("/usage", "show context window usage"),
    ("/clear", "clear screen and conversation"),
    ("/resume", "resume a session  usage: /resume <session-id>"),
    ("/quit", "exit Krabs"),
];

/// Well-known models grouped by provider. Used by `/models` for display and tab-completion.
const KNOWN_MODELS: &[(&str, &[&str])] = &[
    (
        "anthropic",
        &[
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-haiku-4-5-20251001",
        ],
    ),
    (
        "openai",
        &["gpt-4o", "gpt-4o-mini", "gpt-4-turbo", "o1", "o3-mini"],
    ),
    (
        "gemini",
        &[
            "gemini-2.0-flash",
            "gemini-2.0-flash-lite",
            "gemini-1.5-pro",
            "gemini-1.5-flash",
        ],
    ),
    (
        "
        ",
        &["llama3.2", "mistral", "codestral", "qwen2.5-coder"],
    ),
];

fn context_limit(model: &str) -> u32 {
    let m = model.to_lowercase();
    if m.contains("gemini") {
        1_000_000
    } else if m.contains("claude") {
        200_000
    } else if m.contains("gpt-4o") || m.contains("gpt-4-turbo") {
        128_000
    } else if m.contains("gpt-4") {
        8_192
    } else if m.contains("gpt-3.5") {
        16_385
    } else {
        1_000_000
    }
}

/// Load a persisted session's history and convert it to display messages.
/// Returns `(messages_for_agent, display_messages_for_tui)`.
async fn load_resume_history(
    config: &KrabsConfig,
    session_id: &str,
) -> (Vec<Message>, Vec<ChatMsg>) {
    use krabs_core::{session::session::Session as KrabsSession, SessionStore};

    let store = match SessionStore::open(&config.db_path).await {
        Ok(s) => s,
        Err(_) => return (Vec::new(), Vec::new()),
    };
    let session = match store.load_session(session_id).await {
        Ok(s) => s,
        Err(_) => return (Vec::new(), Vec::new()),
    };

    let stored = match session.latest_checkpoint().await {
        Ok(Some(cp)) => {
            let _ = session.rollback_to(cp.last_msg_id).await;
            session
                .messages_up_to(cp.last_msg_id)
                .await
                .unwrap_or_default()
        }
        _ => session.messages().await.unwrap_or_default(),
    };

    let mut messages = Vec::new();
    let mut display: Vec<ChatMsg> = Vec::new();

    for s in &stored {
        match KrabsSession::stored_to_message(s) {
            Ok(msg) => {
                let dm = match s.role.as_str() {
                    "user" => ChatMsg::User(s.content.clone()),
                    "assistant" if s.tool_args.is_none() => ChatMsg::Assistant(s.content.clone()),
                    _ => ChatMsg::Info(format!("[{}] {}", s.role, s.content)),
                };
                display.push(dm);
                messages.push(msg);
            }
            Err(_) => {}
        }
    }

    (messages, display)
}

fn build_registry() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(Arc::new(BashTool));
    r.register(Arc::new(GlobTool));
    r.register(Arc::new(GrepTool));
    r.register(Arc::new(ReadTool));
    r.register(Arc::new(WebFetchTool::new()));
    r.register(Arc::new(WriteTool));
    r
}

// ── chat message types ───────────────────────────────────────────────────────

#[derive(Clone)]
enum ChatMsg {
    User(String),
    Assistant(String),
    ToolCall(String),
    ToolResult(String),
    Usage(u32, u32),
    Info(String),
    Error(String),
}

impl ChatMsg {
    fn to_lines(&self) -> Vec<Line<'static>> {
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

enum DisplayEvent {
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
    Done(Vec<Message>),
    Error(String),
}

/// Active permission prompt waiting for a user keypress.
struct PendingPermission {
    tool_name: String,
    args: String,
    respond: oneshot::Sender<bool>,
}

/// Active user-input prompt rendered as a TUI popup.
struct PendingUserInput {
    mode: InputMode,
    question: String,
    /// Choices shown to the user (options + "custom…" appended).
    options: Vec<String>,
    /// For ChooseMany: which indices are checked.
    selected: Vec<bool>,
    /// Highlighted / focused index.
    cursor: usize,
    /// True when the user is typing a custom free-text answer.
    custom_mode: bool,
    custom_text: String,
    custom_cursor: usize,
    respond: oneshot::Sender<String>,
}

// ── app state ────────────────────────────────────────────────────────────────

struct App {
    chat: Vec<ChatMsg>,
    input: String,
    cursor: usize,
    scroll: u16,
    auto_scroll: bool,
    history: Vec<String>,
    history_idx: Option<usize>,
    spinning: bool,
    spin_i: usize,
    total_input: u32,
    total_output: u32,
    suggest_idx: Option<usize>, // selected index in suggestion popup
    active_persona: Option<AgentPersona>,
    system_prompt_text: String,
    persona_text: String,
    tools_text: String,
    memory_text: String,
    personas: Vec<AgentPersona>,
    /// Tools approved with "always allow" — no prompt on subsequent calls.
    approved_tools: HashSet<String>,
    /// Active permission prompt waiting for y / a / n keypress.
    pending_permission: Option<PendingPermission>,
    /// Active user-input popup waiting for the user to select / confirm.
    pending_user_input: Option<PendingUserInput>,
}

impl App {
    fn new() -> Self {
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

    fn push(&mut self, msg: ChatMsg) {
        self.chat.push(msg);
        if self.auto_scroll {
            self.scroll = u16::MAX;
        }
    }

    fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn backspace(&mut self) {
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

    fn cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.input[..self.cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    fn cursor_right(&mut self) {
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

fn extract_api_error(raw: &str) -> String {
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

// ── TUI hook — bridges KrabsAgent lifecycle events into DisplayEvents ─────────

struct TuiHook {
    tx: mpsc::Sender<DisplayEvent>,
}

#[async_trait::async_trait]
impl krabs_core::Hook for TuiHook {
    async fn on_event(
        &self,
        event: &krabs_core::HookEvent,
    ) -> anyhow::Result<krabs_core::HookOutput> {
        use krabs_core::{HookEvent, HookOutput, ToolUseDecision};
        match event {
            // Before a tool runs: ask the user for permission
            HookEvent::PreToolUse {
                tool_name,
                args,
                tool_use_id: _,
            } => {
                let (respond, rx) = oneshot::channel::<bool>();
                let args_str = serde_json::to_string(args).unwrap_or_default();
                // If the send fails the channel is closed (turn cancelled) — deny
                if self
                    .tx
                    .send(DisplayEvent::PermissionRequest {
                        tool_name: tool_name.clone(),
                        args: args_str,
                        respond,
                    })
                    .await
                    .is_err()
                {
                    return Ok(HookOutput::ToolDecision(ToolUseDecision::Deny {
                        reason: "channel closed".into(),
                    }));
                }
                let allowed = rx.await.unwrap_or(false);
                if allowed {
                    Ok(HookOutput::Continue)
                } else {
                    Ok(HookOutput::ToolDecision(ToolUseDecision::Deny {
                        reason: "denied by user".into(),
                    }))
                }
            }
            // After a tool succeeds: show the result in the TUI
            HookEvent::PostToolUse { result, .. } => {
                let _ = self
                    .tx
                    .send(DisplayEvent::ToolResultEnd(result.clone()))
                    .await;
                Ok(HookOutput::Continue)
            }
            _ => Ok(HookOutput::Continue),
        }
    }
}

// ── background agentic task ──────────────────────────────────────────────────

/// Build a per-turn `KrabsAgent` with the given provider, registry, system
/// prompt, and a `TuiHook` wired to the display-event channel.
async fn build_agent(
    config: &KrabsConfig,
    provider: Arc<dyn LlmProvider>,
    registry: Arc<ToolRegistry>,
    system_prompt: String,
    tx: mpsc::Sender<DisplayEvent>,
    resume_session_id: Option<String>,
) -> Arc<krabs_core::KrabsAgent> {
    let mut tool_registry = ToolRegistry::new();
    for name in registry.names() {
        if let Some(t) = registry.get(&name) {
            tool_registry.register(t);
        }
    }
    // Register orchestration tools so the agent can spawn specialised sub-agents.
    tool_registry.register(Arc::new(DelegateTool::new(
        config.clone(),
        Arc::clone(&provider),
        tool_registry.clone(),
        krabs_core::PermissionGuard::new(),
    )));
    tool_registry.register(Arc::new(DispatchTool::new(
        config.clone(),
        Arc::clone(&provider),
        tool_registry.clone(),
        krabs_core::PermissionGuard::new(),
    )));
    // Register the ask_user tool: a dedicated channel forwards requests to the
    // TUI event loop as DisplayEvent::UserInput, blocking the agent until the
    // user confirms their choice in the popup.
    let (ui_tx, mut ui_rx) = mpsc::channel::<UserInputRequest>(4);
    let fwd_tx = tx.clone();
    tokio::spawn(async move {
        while let Some(req) = ui_rx.recv().await {
            let _ = fwd_tx.send(DisplayEvent::UserInput(req)).await;
        }
    });
    tool_registry.register(Arc::new(UserInputTool::new(ui_tx)));
    let builder = krabs_core::KrabsAgentBuilder::new(config.clone(), provider)
        .registry(tool_registry)
        .system_prompt(system_prompt)
        .hook(Arc::new(TuiHook { tx }));
    let builder = match resume_session_id {
        Some(sid) => builder.resume_session(sid),
        None => builder,
    };
    builder.build_async().await
}

async fn run_agent_turn(
    agent: Arc<krabs_core::KrabsAgent>,
    messages: Vec<Message>,
    tx: mpsc::Sender<DisplayEvent>,
) {
    let (mut stream, done_rx) = match agent.run_streaming_with_history(messages).await {
        Ok(r) => r,
        Err(e) => {
            let _ = tx
                .send(DisplayEvent::Error(extract_api_error(&e.to_string())))
                .await;
            return;
        }
    };

    while let Some(chunk) = stream.recv().await {
        match chunk {
            StreamChunk::Delta { text } => {
                if tx.send(DisplayEvent::Token(text)).await.is_err() {
                    return;
                }
            }
            StreamChunk::ToolCallReady { call } => {
                if tx.send(DisplayEvent::ToolCallStart(call)).await.is_err() {
                    return;
                }
            }
            StreamChunk::Done { usage } => {
                if tx.send(DisplayEvent::TurnUsage(usage)).await.is_err() {
                    return;
                }
            }
        }
    }

    // Stream closed — get final message history from done channel
    let final_messages = match done_rx.await {
        Ok(Ok(msgs)) => msgs,
        Ok(Err(e)) => {
            let _ = tx
                .send(DisplayEvent::Error(extract_api_error(&e.to_string())))
                .await;
            return;
        }
        Err(_) => {
            // sender dropped without sending — treat as empty (turn was cancelled)
            return;
        }
    };
    let _ = tx.send(DisplayEvent::Done(final_messages)).await;
}

// ── slash suggestions ─────────────────────────────────────────────────────────

fn slash_suggestions(prefix: &str) -> Vec<(&'static str, &'static str)> {
    SLASH_COMMANDS
        .iter()
        .filter(|(cmd, _)| cmd.starts_with(prefix))
        .copied()
        .collect()
}

/// Return persona names whose names start with `prefix` (after stripping `@`).
fn at_suggestions<'a>(prefix: &str, personas: &'a [AgentPersona]) -> Vec<(&'a str, &'a str)> {
    personas
        .iter()
        .filter(|p| p.name.starts_with(prefix))
        .map(|p| (p.name.as_str(), p.description.as_deref().unwrap_or("")))
        .collect()
}

fn cmd_agents(app: &mut App, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    match parts.as_slice() {
        [] | ["list"] => {
            // ── built-in base agents ──────────────────────────────────────────
            let base = BaseAgent::all();
            app.push(ChatMsg::Info(format!("{} built-in agent(s):", base.len())));
            for agent in base {
                app.push(ChatMsg::Info(format!(
                    "  @{:<20}  (built-in)",
                    agent.name()
                )));
            }

            // ── project personas (discovered from ./krabs/agents/) ────────────
            let personas = AgentPersona::discover();
            app.personas = personas;
            if app.personas.is_empty() {
                app.push(ChatMsg::Info(
                    "no project personas found — add markdown files to ./krabs/agents/".into(),
                ));
            } else {
                let lines: Vec<String> = {
                    let mut v = vec![format!("{} project persona(s):", app.personas.len())];
                    for p in &app.personas {
                        let desc = p.description.as_deref().unwrap_or("");
                        v.push(format!("  @{:<20}  {}", p.name, desc));
                    }
                    v
                };
                for line in lines {
                    app.push(ChatMsg::Info(line));
                }
            }
        }
        _ => {
            app.push(ChatMsg::Error(
                "usage: /agents [list]  |  type @<name> to activate a persona".into(),
            ));
        }
    }
}

// ── rendering ────────────────────────────────────────────────────────────────

struct InfoBar {
    provider: String,
    model: String,
    cwd: String,
    tools: String,
}

fn render(app: &mut App, max_ctx: u32, info: &InfoBar, frame: &mut Frame) {
    let area = frame.area();
    let info_height: u16 = if app.active_persona.is_some() { 7 } else { 6 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(info_height), // info box
            Constraint::Min(1),              // chat
            Constraint::Length(3),           // input
        ])
        .split(area);

    // ── info box ──────────────────────────────────────────────────────────────
    let used = app.total_input + app.total_output;
    let pct = (used as f32 / max_ctx as f32 * 100.0).min(100.0);

    // Build segmented context bar
    const CTX_BAR_WIDTH: usize = 20;
    let t_system = estimate_tokens(&app.system_prompt_text);
    let t_persona = estimate_tokens(&app.persona_text);
    let t_memory = estimate_tokens(&app.memory_text);
    let t_tools = estimate_tokens(&app.tools_text);
    let t_messages = used.saturating_sub(t_system + t_persona + t_memory + t_tools);
    let t_free = max_ctx.saturating_sub(used);
    let seg_w = |tok: u32| -> usize {
        ((tok as f32 / max_ctx as f32) * CTX_BAR_WIDTH as f32).round() as usize
    };
    let cat_segs = [
        (seg_w(t_system), Color::Green),
        (seg_w(t_persona), MR_KRABS_ORANGE),
        (seg_w(t_tools), Color::Magenta),
        (seg_w(t_memory), Color::Blue),
        (seg_w(t_messages), Color::Cyan),
        (seg_w(t_free), Color::DarkGray),
    ];
    let mut ctx_spans: Vec<Span> = vec![Span::raw("[")];
    for (w, color) in &cat_segs {
        if *w > 0 {
            ctx_spans.push(Span::styled("█".repeat(*w), Style::default().fg(*color)));
        }
    }
    ctx_spans.push(Span::raw("] "));
    ctx_spans.push(Span::styled(
        format!("{:.1}%", pct),
        Style::default().fg(Color::Yellow),
    ));

    let mut info_lines = vec![
        Line::from(vec![
            Span::styled("  provider  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                &info.provider,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("   model  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                &info.model,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  cwd     ", Style::default().fg(Color::DarkGray)),
            Span::styled(&info.cwd, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("  tools   ", Style::default().fg(Color::DarkGray)),
            Span::styled(&info.tools, Style::default().fg(Color::White)),
        ]),
        Line::from({
            let mut spans = vec![Span::styled(
                "  ctx     ",
                Style::default().fg(Color::DarkGray),
            )];
            spans.extend(ctx_spans);
            spans
        }),
    ];
    if let Some(ref persona) = app.active_persona {
        info_lines.push(Line::from(vec![
            Span::styled("  persona ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("@{}", persona.name),
                Style::default()
                    .fg(MR_KRABS_ORANGE)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    let info_widget = Paragraph::new(info_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(MR_KRABS_ORANGE))
            .title(Span::styled(
                " krabs ",
                Style::default()
                    .fg(MR_KRABS_ORANGE)
                    .add_modifier(Modifier::BOLD),
            )),
    );
    frame.render_widget(info_widget, chunks[0]);

    // ── chat messages ─────────────────────────────────────────────────────────
    let mut lines: Vec<Line> = vec![Line::raw("")];
    for msg in &app.chat {
        lines.extend(msg.to_lines());
    }

    // Spinner at end while thinking
    if app.spinning {
        lines.push(Line::from(Span::styled(
            format!("  {} thinking…", SPINNER[app.spin_i % SPINNER.len()]),
            Style::default().fg(Color::Cyan),
        )));
    }

    // Scroll clamping
    let total = lines.len() as u16;
    let view_h = chunks[1].height.saturating_sub(2);
    let max_scroll = total.saturating_sub(view_h);
    if app.scroll == u16::MAX {
        app.scroll = max_scroll;
    }
    app.scroll = app.scroll.min(max_scroll);

    let msg_widget = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(Span::styled(" chat ", Style::default().fg(Color::DarkGray))),
        )
        .scroll((app.scroll, 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(msg_widget, chunks[1]);

    // ── input box ─────────────────────────────────────────────────────────────
    let busy = app.spinning;
    let border_col = if busy { Color::DarkGray } else { Color::Cyan };

    let before = &app.input[..app.cursor];
    let (cur_ch, after) = if app.cursor < app.input.len() {
        let ch = app.input[app.cursor..].chars().next().unwrap();
        let end = app.cursor + ch.len_utf8();
        (ch.to_string(), app.input[end..].to_string())
    } else {
        (" ".to_string(), String::new())
    };

    let input_line = Line::from(vec![
        Span::styled(before.to_string(), Style::default().fg(Color::White)),
        Span::styled(cur_ch, Style::default().fg(Color::Black).bg(Color::White)),
        Span::styled(after, Style::default().fg(Color::White)),
    ]);

    let input_widget = Paragraph::new(input_line).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_col))
            .title(Span::styled(" message ", Style::default().fg(border_col))),
    );

    frame.render_widget(input_widget, chunks[2]);

    // Suggestion popup (only when input starts with '/')
    if !app.spinning && app.input.starts_with('/') {
        let suggestions = slash_suggestions(&app.input);
        if !suggestions.is_empty() {
            let pop_h = suggestions.len() as u16 + 2;
            let pop_w = 40u16.min(area.width);
            let pop_x = chunks[2].x + 1;
            let pop_y = chunks[2].y.saturating_sub(pop_h);
            let pop_rect = ratatui::layout::Rect::new(pop_x, pop_y, pop_w, pop_h);

            let lines: Vec<Line> = suggestions
                .iter()
                .enumerate()
                .map(|(i, (cmd, desc))| {
                    let selected = app.suggest_idx == Some(i);
                    let style = if selected {
                        Style::default().fg(Color::Black).bg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    let desc_style = if selected {
                        Style::default().fg(Color::Black).bg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    };
                    Line::from(vec![
                        Span::styled(format!(" {:<12}", cmd), style),
                        Span::styled(format!(" {}", desc), desc_style),
                    ])
                })
                .collect();

            let popup = Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(Span::styled(" commands ", Style::default().fg(Color::Cyan))),
            );

            frame.render_widget(ratatui::widgets::Clear, pop_rect);
            frame.render_widget(popup, pop_rect);
        }
    }

    // ── Permission dialog ──────────────────────────────────────────────────────
    if let Some(ref perm) = app.pending_permission {
        let pop_w = (area.width * 3 / 4).clamp(40, 72);
        let pop_h = 7u16;
        let pop_x = area.x + (area.width.saturating_sub(pop_w)) / 2;
        let pop_y = area.y + (area.height.saturating_sub(pop_h)) / 2;
        let pop_rect = ratatui::layout::Rect::new(pop_x, pop_y, pop_w, pop_h);

        // Truncate args to fit in the dialog width
        let max_arg_len = (pop_w as usize).saturating_sub(6);
        let args_display = if perm.args.len() > max_arg_len {
            format!("{}…", &perm.args[..max_arg_len.saturating_sub(1)])
        } else {
            perm.args.clone()
        };

        let perm_lines = vec![
            Line::raw(""),
            Line::from(vec![
                Span::styled("  tool  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    perm.tool_name.clone(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("  args  ", Style::default().fg(Color::DarkGray)),
                Span::styled(args_display, Style::default().fg(Color::White)),
            ]),
            Line::raw(""),
            Line::from(vec![Span::styled(
                "  [y] allow once   [a] always allow   [n] deny",
                Style::default().fg(Color::Cyan),
            )]),
        ];

        let perm_widget = Paragraph::new(perm_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(Span::styled(
                    " ⚠ tool permission ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
        );

        frame.render_widget(ratatui::widgets::Clear, pop_rect);
        frame.render_widget(perm_widget, pop_rect);
    }

    // ── user-input popup ─────────────────────────────────────────────────────
    if let Some(ref ui) = app.pending_user_input {
        let pop_w = (area.width * 3 / 4).clamp(44, 76);
        let n_opts = ui.options.len() as u16; // includes "custom…"
        let pop_h = 4 + n_opts + if ui.custom_mode { 2 } else { 1 };
        let pop_x = area.x + (area.width.saturating_sub(pop_w)) / 2;
        let pop_y = area.y + (area.height.saturating_sub(pop_h)) / 2;
        let pop_rect = ratatui::layout::Rect::new(pop_x, pop_y, pop_w, pop_h);

        let mut lines: Vec<Line> = vec![
            Line::raw(""),
            Line::from(Span::styled(
                format!("  {}", ui.question),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::raw(""),
        ];

        for (i, opt) in ui.options.iter().enumerate() {
            let focused = i == ui.cursor;
            let prefix = match ui.mode {
                InputMode::ChooseOne => {
                    if focused {
                        "  ● "
                    } else {
                        "  ○ "
                    }
                }
                InputMode::ChooseMany => {
                    if ui.selected[i] {
                        "  [x] "
                    } else {
                        "  [ ] "
                    }
                }
            };
            let style = if focused {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            // Last option is always the custom placeholder
            let label = if i == ui.options.len() - 1 && ui.custom_mode {
                format!("{}{}_", prefix, ui.custom_text) // blinking cursor sim
            } else {
                format!("{}{}", prefix, opt)
            };
            lines.push(Line::from(Span::styled(label, style)));
        }

        lines.push(Line::raw(""));
        let hint = match ui.mode {
            InputMode::ChooseOne => "  ↑↓ move   enter select   esc cancel",
            InputMode::ChooseMany => "  ↑↓ move   space toggle   enter confirm   esc cancel",
        };
        lines.push(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        )));

        let (border_color, title) = match ui.mode {
            InputMode::ChooseOne => (Color::Cyan, " agent question — choose one "),
            InputMode::ChooseMany => (Color::Magenta, " agent question — choose many "),
        };

        let popup = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(Span::styled(
                    title,
                    Style::default()
                        .fg(border_color)
                        .add_modifier(Modifier::BOLD),
                )),
        );

        frame.render_widget(ratatui::widgets::Clear, pop_rect);
        frame.render_widget(popup, pop_rect);
    }

    // @<name> suggestion popup
    if !app.spinning && app.input.starts_with('@') && !app.input.contains(' ') {
        let prefix = &app.input[1..];
        let suggestions = at_suggestions(prefix, &app.personas);
        if !suggestions.is_empty() {
            let pop_h = suggestions.len() as u16 + 2;
            let pop_w = 44u16.min(area.width);
            let pop_x = chunks[2].x + 1;
            let pop_y = chunks[2].y.saturating_sub(pop_h);
            let pop_rect = ratatui::layout::Rect::new(pop_x, pop_y, pop_w, pop_h);

            let popup_lines: Vec<Line> = suggestions
                .iter()
                .enumerate()
                .map(|(i, (name, desc))| {
                    let selected = app.suggest_idx == Some(i);
                    let style = if selected {
                        Style::default().fg(Color::Black).bg(MR_KRABS_ORANGE)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    let desc_style = if selected {
                        Style::default().fg(Color::Black).bg(MR_KRABS_ORANGE)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    };
                    Line::from(vec![
                        Span::styled(format!(" @{:<12}", name), style),
                        Span::styled(format!(" {}", desc), desc_style),
                    ])
                })
                .collect();

            let popup = Paragraph::new(popup_lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(MR_KRABS_ORANGE))
                    .title(Span::styled(
                        " personas ",
                        Style::default().fg(MR_KRABS_ORANGE),
                    )),
            );

            frame.render_widget(ratatui::widgets::Clear, pop_rect);
            frame.render_widget(popup, pop_rect);
        }
    }
}

// ── slash command helpers ────────────────────────────────────────────────────

/// /models                    — list known + custom models
/// /models <name|model>       — switch by custom entry name or model id (keep provider)
/// /models <provider> <model> — switch provider and model
fn cmd_models(
    app: &mut App,
    args: &str,
    creds: &mut Credentials,
    provider: &mut Arc<dyn LlmProvider>,
    info: &mut InfoBar,
    max_ctx: &mut u32,
    custom_models: &[CustomModelEntry],
) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    match parts.as_slice() {
        // /models — list
        [] => {
            app.push(ChatMsg::Info(format!(
                "current: {}  {}  ({})",
                creds.provider, creds.model, creds.base_url
            )));
            app.push(ChatMsg::Info(String::new()));

            // Built-in known models
            for (prov, models) in KNOWN_MODELS {
                app.push(ChatMsg::Info(format!("  {}:", prov)));
                for m in *models {
                    let active = *prov == creds.provider && *m == creds.model;
                    let marker = if active { " ◀" } else { "" };
                    app.push(ChatMsg::Info(format!("    {}{}", m, marker)));
                }
            }

            // Custom models from config
            if !custom_models.is_empty() {
                app.push(ChatMsg::Info(String::new()));
                app.push(ChatMsg::Info("  custom (from config):".into()));
                for entry in custom_models {
                    let active = entry.provider == creds.provider
                        && entry.model == creds.model
                        && entry.base_url == creds.base_url;
                    let marker = if active { " ◀" } else { "" };
                    app.push(ChatMsg::Info(format!(
                        "    {:<20}  {}  {}  {}{}",
                        entry.name, entry.provider, entry.model, entry.base_url, marker
                    )));
                }
            }

            app.push(ChatMsg::Info(String::new()));
            app.push(ChatMsg::Info(
                "  usage: /models <name|model>  |  /models <provider> <model>".into(),
            ));
        }

        // /models <name|model> — check custom entries first, then fall back to model-id switch
        [name_or_model] => {
            if let Some(entry) = custom_models.iter().find(|e| e.name == *name_or_model) {
                // Matched a named custom entry — apply all its fields
                creds.provider = entry.provider.clone();
                creds.model = entry.model.clone();
                creds.base_url = entry.base_url.clone();
                if !entry.api_key.is_empty() {
                    creds.api_key = entry.api_key.clone();
                }
                *provider = Arc::from(creds.build_provider());
                *max_ctx = context_limit(&creds.model);
                info.provider = creds.provider.clone();
                info.model = creds.model.clone();
                app.push(ChatMsg::Info(format!(
                    "switched to custom model '{}' → {}  {}  ({})",
                    entry.name, creds.provider, creds.model, creds.base_url
                )));
            } else {
                // Treat as a bare model id — keep current provider and base_url
                creds.model = name_or_model.to_string();
                *provider = Arc::from(creds.build_provider());
                *max_ctx = context_limit(&creds.model);
                info.model = creds.model.clone();
                app.push(ChatMsg::Info(format!(
                    "switched model → {}  {}",
                    creds.provider, creds.model
                )));
            }
        }

        // /models <provider> <model>
        [prov, model] => {
            creds.provider = prov.to_string();
            creds.model = model.to_string();
            *provider = Arc::from(creds.build_provider());
            *max_ctx = context_limit(&creds.model);
            info.provider = creds.provider.clone();
            info.model = creds.model.clone();
            app.push(ChatMsg::Info(format!(
                "switched → {}  {}",
                creds.provider, creds.model
            )));
        }

        _ => {
            app.push(ChatMsg::Error(
                "usage: /models [<name|model> | <provider> <model>]".into(),
            ));
        }
    }
}

fn cmd_tools(app: &mut App, registry: &ToolRegistry) {
    app.push(ChatMsg::Info("available tools:".into()));
    for d in registry.tool_defs() {
        app.push(ChatMsg::Info(format!("  {:10}  {}", d.name, d.description)));
    }
}

fn cmd_skills(app: &mut App, skills_config: &SkillsConfig) {
    let skills = SkillLoader::discover(skills_config);
    if skills.is_empty() {
        app.push(ChatMsg::Info(
            "no skills found — add skill directories under skills/".into(),
        ));
    } else {
        app.push(ChatMsg::Info(format!("{} skill(s):", skills.len())));
        for s in &skills {
            app.push(ChatMsg::Info(format!("  {:20}  {}", s.name, s.description)));
        }
    }
}

/// /mcp                          — list configured servers
/// /mcp add <name> <cmd> [args…] — add a stdio server
/// /mcp add-sse <name> <url>     — add an SSE server
/// /mcp remove <name>            — remove a server
/// /mcp tools                    — list tools from all connected servers
async fn cmd_mcp(app: &mut App, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();

    match parts.as_slice() {
        // /mcp  or  /mcp list
        [] | ["list"] => {
            let reg = McpRegistry::load().await;
            if reg.servers.is_empty() {
                app.push(ChatMsg::Info("no MCP servers configured".into()));
                app.push(ChatMsg::Info(
                    "  /mcp add <name> <command> [args…]    — stdio server".into(),
                ));
                app.push(ChatMsg::Info(
                    "  /mcp add-sse <name> <url>            — SSE server".into(),
                ));
            } else {
                app.push(ChatMsg::Info("MCP servers:".into()));
                for s in &reg.servers {
                    let dot = if s.enabled { "●" } else { "○" };
                    let transport = s.transport_label();
                    let endpoint = s.endpoint();
                    app.push(ChatMsg::Info(format!(
                        "  {} {:20}  [{transport}] {endpoint}",
                        dot, s.name
                    )));
                }
            }
        }

        ["add", name, rest @ ..] if !rest.is_empty() => {
            let command = rest[0];
            let server_args: Vec<String> = rest[1..].iter().map(|s| s.to_string()).collect();
            let server = McpServer::stdio(*name, command, server_args);
            let mut reg = McpRegistry::load().await;
            reg.add(server);
            if let Err(e) = reg.save().await {
                app.push(ChatMsg::Error(format!("failed to save: {e}")));
            } else {
                app.push(ChatMsg::Info(format!("added stdio server '{name}'")));
            }
        }

        ["add-sse", name, url] => {
            let server = McpServer::sse(*name, *url);
            let mut reg = McpRegistry::load().await;
            reg.add(server);
            if let Err(e) = reg.save().await {
                app.push(ChatMsg::Error(format!("failed to save: {e}")));
            } else {
                app.push(ChatMsg::Info(format!("added SSE server '{name}'")));
            }
        }

        ["remove", name] => {
            let mut reg = McpRegistry::load().await;
            if reg.remove(name) {
                if let Err(e) = reg.save().await {
                    app.push(ChatMsg::Error(format!("failed to save: {e}")));
                } else {
                    app.push(ChatMsg::Info(format!("removed server '{name}'")));
                }
            } else {
                app.push(ChatMsg::Error(format!("server '{name}' not found")));
            }
        }

        ["tools"] => {
            let reg = McpRegistry::load().await;
            if reg.servers.is_empty() {
                app.push(ChatMsg::Info("no MCP servers configured".into()));
                return;
            }
            app.push(ChatMsg::Info("connecting to MCP servers…".into()));
            let live = reg.connect_all().await;
            if live.is_empty() {
                app.push(ChatMsg::Error("no servers connected".into()));
                return;
            }
            let tools = live.tools_for_all().await;
            if tools.is_empty() {
                app.push(ChatMsg::Info("no tools discovered".into()));
            } else {
                app.push(ChatMsg::Info(format!("{} MCP tools:", tools.len())));
                for t in &tools {
                    app.push(ChatMsg::Info(format!("  {}", t.name())));
                }
            }
        }

        _ => {
            app.push(ChatMsg::Info(
                "usage: /mcp [list|add <name> <cmd> [args…]|add-sse <name> <url>|remove <name>|tools]".into(),
            ));
        }
    }
}

/// /hooks [list]
/// /hooks add <name> <event> [matcher] [action] [reason…]
/// /hooks remove <name>
///
/// event   : AgentStart | AgentStop | TurnStart | TurnEnd |
///           PreToolUse | PostToolUse | PostToolUseFailure
/// action  : deny | stop | log  (default: log)
fn cmd_hooks(app: &mut App, args: &str) {
    let mut config = HookConfig::load();
    let parts: Vec<&str> = args.split_whitespace().collect();

    match parts.as_slice() {
        // /hooks  or  /hooks list
        [] | ["list"] => {
            if config.hooks.is_empty() {
                app.push(ChatMsg::Info(
                    "no hooks configured — use /hooks add <name> <event> [matcher] [action] [reason]".into(),
                ));
            } else {
                app.push(ChatMsg::Info(format!("{} hook(s):", config.hooks.len())));
                for h in &config.hooks {
                    let matcher = h.matcher.as_deref().unwrap_or("*");
                    let reason = h.reason.as_deref().unwrap_or("");
                    app.push(ChatMsg::Info(format!(
                        "  {:20}  event={:<22}  matcher={:<12}  action={:<6}  {}",
                        h.name, h.event, matcher, h.action, reason,
                    )));
                }
            }
        }

        // /hooks add <name> <event> [matcher] [action] [reason…]
        ["add", name, event, rest @ ..] => {
            let (matcher, action, reason) = parse_hook_rest(rest);
            let entry = HookEntry {
                name: name.to_string(),
                event: event.to_string(),
                matcher,
                action,
                reason,
            };
            config.add(entry);
            match config.save() {
                Ok(()) => app.push(ChatMsg::Info(format!("hook '{}' saved", name))),
                Err(e) => app.push(ChatMsg::Error(format!("failed to save hook: {e}"))),
            }
        }

        // /hooks remove <name>
        ["remove", name] => {
            if config.remove(name) {
                match config.save() {
                    Ok(()) => app.push(ChatMsg::Info(format!("hook '{}' removed", name))),
                    Err(e) => app.push(ChatMsg::Error(format!("failed to save: {e}"))),
                }
            } else {
                app.push(ChatMsg::Error(format!("hook '{}' not found", name)));
            }
        }

        _ => {
            app.push(ChatMsg::Error(
                "usage: /hooks [list]  |  /hooks add <name> <event> [matcher] [action] [reason]  |  /hooks remove <name>".into(),
            ));
        }
    }
}

/// Parse the trailing `[matcher] [action] [reason…]` tokens.
/// matcher — any token that is not a known action keyword
/// action  — deny | stop | log  (default: log)
/// reason  — remaining tokens joined by space
fn parse_hook_rest(rest: &[&str]) -> (Option<String>, String, Option<String>) {
    const ACTIONS: &[&str] = &["deny", "stop", "log"];
    let mut matcher: Option<String> = None;
    let mut action = "log".to_string();
    let mut reason_parts: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let tok = rest[i];
        if ACTIONS.contains(&tok) {
            action = tok.to_string();
            reason_parts.extend_from_slice(&rest[i + 1..]);
            break;
        } else if matcher.is_none() {
            matcher = Some(tok.to_string());
        } else {
            // unexpected token before action — treat rest as reason
            reason_parts.push(tok);
        }
        i += 1;
    }
    let reason = if reason_parts.is_empty() {
        None
    } else {
        Some(reason_parts.join(" "))
    };
    (matcher, action, reason)
}

fn estimate_tokens(s: &str) -> u32 {
    ((s.len() as f32) / 4.0).ceil() as u32
}

fn fmt_k(n: u32) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f32 / 1000.0)
    } else {
        format!("{}", n)
    }
}

fn cmd_usage(app: &mut App, max_ctx: u32, skills_config: &SkillsConfig) {
    const BAR: usize = 40;

    let used = app.total_input + app.total_output;
    let pct = (used as f32 / max_ctx as f32 * 100.0).min(100.0);

    // Compute estimated token counts per category
    let t_system = estimate_tokens(&app.system_prompt_text);
    let t_persona = estimate_tokens(&app.persona_text);
    let t_memory = estimate_tokens(&app.memory_text);
    let t_tools = estimate_tokens(&app.tools_text);

    // Skills: compute lazily from config (same source as agent does)
    let skills = SkillLoader::discover(skills_config);
    let skills_text: String = skills
        .iter()
        .map(|s| format!("{}: {}", s.name, s.description))
        .collect::<Vec<_>>()
        .join("\n");
    let t_skills = estimate_tokens(&skills_text);

    // Messages estimate from API-reported totals, minus estimated overhead
    let overhead = t_system + t_persona + t_memory + t_tools + t_skills;
    let t_messages = used.saturating_sub(overhead);
    let t_free = max_ctx.saturating_sub(used);

    // Build per-category bar segments (proportional to max_ctx)
    let seg = |tok: u32| -> usize { ((tok as f32 / max_ctx as f32) * BAR as f32).round() as usize };

    let segs = [
        (seg(t_system), 'S', Color::Green),
        (seg(t_persona), 'P', Color::Rgb(255, 128, 0)),
        (seg(t_tools), 'T', Color::Magenta),
        (seg(t_skills), 'K', Color::LightGreen),
        (seg(t_memory), 'M', Color::Blue),
        (seg(t_messages), 'C', Color::Cyan),
        (seg(t_free), 'F', Color::DarkGray),
    ];

    let header = format!(
        "context usage  {pct:.1}%  ({} / {} tokens)",
        fmt_k(used),
        fmt_k(max_ctx)
    );
    app.push(ChatMsg::Info(header));
    app.push(ChatMsg::Info(String::new()));

    let rows = [
        ("system  ", t_system, 0usize),
        ("persona ", t_persona, 1),
        ("tools   ", t_tools, 2),
        ("skills  ", t_skills, 3),
        ("memory  ", t_memory, 4),
        ("messages", t_messages, 5),
        ("free    ", t_free, 6),
    ];

    for (label, tok, idx) in &rows {
        let w = segs[*idx].0.min(BAR);
        let ch = segs[*idx].1;
        // Build bar: this category chars filled, rest as ░
        let bar_str: String = std::iter::repeat_n(ch, w)
            .chain(std::iter::repeat_n('░', BAR - w))
            .collect();
        let tok_pct = (*tok as f32 / max_ctx as f32 * 100.0).min(100.0);
        app.push(ChatMsg::Info(format!(
            "  {label}  [{bar_str}]  ~{:>5}  {tok_pct:.1}%",
            fmt_k(*tok)
        )));
    }
    app.push(ChatMsg::Info(format!("  {}", "─".repeat(BAR + 30))));
    app.push(ChatMsg::Info(format!(
        "  total     ~{} / {} ({pct:.1}% used)",
        fmt_k(used),
        fmt_k(max_ctx)
    )));
}

// ── async helper: recv or park ───────────────────────────────────────────────

async fn recv_event(rx: &mut Option<mpsc::Receiver<DisplayEvent>>) -> Option<DisplayEvent> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

// ── splash screen ────────────────────────────────────────────────────────────

const LOGO: &[&str] = &[
    "██╗  ██╗██████╗  █████╗ ██████╗ ███████╗",
    "██║ ██╔╝██╔══██╗██╔══██╗██╔══██╗██╔════╝",
    "█████╔╝ ██████╔╝███████║██████╔╝███████╗",
    "██╔═██╗ ██╔══██╗██╔══██║██╔══██╗╚════██║",
    "██║  ██╗██║  ██║██║  ██║██████╔╝███████║",
    "╚═╝  ╚═╝╚═╝  ╚═╝╚═╝  ╚═╝╚═════╝ ╚══════╝",
];

const MR_KRABS_ORANGE: Color = Color::Rgb(255, 128, 0);

async fn show_splash(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    key_rx: &mut mpsc::Receiver<Event>,
    provider: &str,
    model: &str,
) -> Result<()> {
    let subtitle = format!("{}  |  {}", provider, model);
    loop {
        terminal.draw(|f| {
            let area = f.area();
            let logo_w = 42u16;
            let logo_h = LOGO.len() as u16;
            let box_w = logo_w + 4;
            let box_h = logo_h + 6; // logo + subtitle + hint + padding

            let x = area.width.saturating_sub(box_w) / 2;
            let y = area.height.saturating_sub(box_h) / 2;
            let rect =
                ratatui::layout::Rect::new(x, y, box_w.min(area.width), box_h.min(area.height));

            let mut lines: Vec<Line> = LOGO
                .iter()
                .map(|row| {
                    Line::from(Span::styled(
                        *row,
                        Style::default()
                            .fg(MR_KRABS_ORANGE)
                            .add_modifier(Modifier::BOLD),
                    ))
                })
                .collect();
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                subtitle.as_str(),
                Style::default().fg(Color::White),
            )));
            lines.push(Line::from(Span::styled(
                "press any key to start",
                Style::default().fg(Color::DarkGray),
            )));

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(MR_KRABS_ORANGE))
                .title(Span::styled(
                    " krabs ",
                    Style::default()
                        .fg(MR_KRABS_ORANGE)
                        .add_modifier(Modifier::BOLD),
                ));

            let para = Paragraph::new(lines)
                .block(block)
                .alignment(ratatui::layout::Alignment::Center);

            f.render_widget(para, rect);
        })?;

        if let Ok(Event::Key(k)) = key_rx.try_recv() {
            if k.kind == KeyEventKind::Press {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

// ── main entry ───────────────────────────────────────────────────────────────

pub async fn run(creds: Credentials, resume_id: Option<String>) -> Result<()> {
    let krabs_config = KrabsConfig::load().unwrap_or_default();
    let mut creds = creds;
    let mut provider: Arc<dyn LlmProvider> = Arc::from(creds.build_provider());
    let registry = Arc::new(build_registry());
    let mut max_ctx = context_limit(&creds.model);
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".into());
    let mut info = InfoBar {
        provider: creds.provider.clone(),
        model: creds.model.clone(),
        cwd,
        tools: registry.names().join(", "),
    };

    // Terminal setup — install a panic hook so we always restore the terminal
    // even if something panics, otherwise the shell is left in raw mode.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(info);
    }));

    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;

    // Keyboard reader thread (needed for splash too)
    let (key_tx, mut key_rx) = mpsc::channel::<Event>(32);
    tokio::task::spawn_blocking(move || loop {
        if event::poll(Duration::from_millis(100)).unwrap_or(false) {
            if let Ok(ev) = event::read() {
                if key_tx.blocking_send(ev).is_err() {
                    break;
                }
            }
        }
    });

    show_splash(&mut terminal, &mut key_rx, &creds.provider, &creds.model).await?;

    let mut app = App::new();
    app.personas = AgentPersona::discover();
    let mut messages: Vec<Message> = Vec::new();

    // If resuming, reconstruct history from the persisted session.
    let mut active_resume_id: Option<String> = None;
    if let Some(ref sid) = resume_id {
        let (history, display_msgs) = load_resume_history(&krabs_config, sid).await;
        if !history.is_empty() {
            for dm in display_msgs {
                app.chat.push(dm);
            }
            messages = history;
            active_resume_id = Some(sid.clone());
            app.push(ChatMsg::Info(format!("Resumed session {sid}")));
        }
    }

    let mut stream_rx: Option<mpsc::Receiver<DisplayEvent>> = None;
    let mut turn_handle: Option<tokio::task::JoinHandle<()>> = None;

    'main: loop {
        terminal.draw(|f| render(&mut app, max_ctx, &info, f))?;

        tokio::select! {
            // ── keyboard ──
            key = key_rx.recv() => {
                let Some(ev) = key else { break };

                let Event::Key(key) = ev else { continue 'main };
                if key.kind != KeyEventKind::Press { continue 'main; }

                // Ctrl+C: cancel turn if running, quit if idle
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    if app.pending_permission.is_some() || app.spinning || stream_rx.is_some() {
                        // Deny any pending permission prompt (dropping sender signals false to task)
                        app.pending_permission = None;
                        if let Some(h) = turn_handle.take() { h.abort(); }
                        stream_rx = None;
                        app.spinning = false;
                        app.push(ChatMsg::Info("cancelled".into()));
                    } else {
                        break;
                    }
                    continue 'main;
                }

                // Scroll (always available)
                match key.code {
                    KeyCode::Up if !app.spinning && stream_rx.is_none() => {
                        let slash_sugg = slash_suggestions(&app.input);
                        let at_sugg = if app.input.starts_with('@') && !app.input.contains(' ') {
                            at_suggestions(&app.input[1..], &app.personas)
                        } else {
                            vec![]
                        };
                        if app.input.starts_with('/') && !slash_sugg.is_empty() {
                            let len = slash_sugg.len();
                            app.suggest_idx = Some(match app.suggest_idx {
                                None | Some(0) => len - 1,
                                Some(i) => i - 1,
                            });
                        } else if app.input.starts_with('@') && !at_sugg.is_empty() {
                            let len = at_sugg.len();
                            app.suggest_idx = Some(match app.suggest_idx {
                                None | Some(0) => len - 1,
                                Some(i) => i - 1,
                            });
                        } else {
                            app.auto_scroll = false;
                            app.scroll = app.scroll.saturating_sub(3);
                        }
                        continue 'main;
                    }
                    KeyCode::Down if !app.spinning && stream_rx.is_none() => {
                        let slash_sugg = slash_suggestions(&app.input);
                        let at_sugg = if app.input.starts_with('@') && !app.input.contains(' ') {
                            at_suggestions(&app.input[1..], &app.personas)
                        } else {
                            vec![]
                        };
                        if app.input.starts_with('/') && !slash_sugg.is_empty() {
                            let len = slash_sugg.len();
                            app.suggest_idx = Some(match app.suggest_idx {
                                None => 0,
                                Some(i) => (i + 1) % len,
                            });
                        } else if app.input.starts_with('@') && !at_sugg.is_empty() {
                            let len = at_sugg.len();
                            app.suggest_idx = Some(match app.suggest_idx {
                                None => 0,
                                Some(i) => (i + 1) % len,
                            });
                        } else {
                            app.scroll = app.scroll.saturating_add(3);
                            if app.scroll == u16::MAX { app.auto_scroll = true; }
                        }
                        continue 'main;
                    }
                    KeyCode::Up => {
                        app.auto_scroll = false;
                        app.scroll = app.scroll.saturating_sub(3);
                        continue 'main;
                    }
                    KeyCode::Down => {
                        app.scroll = app.scroll.saturating_add(3);
                        if app.scroll == u16::MAX { app.auto_scroll = true; }
                        continue 'main;
                    }
                    KeyCode::PageUp => {
                        app.auto_scroll = false;
                        app.scroll = app.scroll.saturating_sub(10);
                        continue 'main;
                    }
                    KeyCode::PageDown => {
                        app.scroll = app.scroll.saturating_add(10);
                        continue 'main;
                    }
                    _ => {}
                }

                // ── User-input popup ──────────────────────────────────────────
                if app.pending_user_input.is_some() {
                    let ui = app.pending_user_input.as_mut().unwrap();
                    let last = ui.options.len() - 1; // index of the "custom…" entry

                    if ui.custom_mode {
                        // Typing a custom answer
                        match key.code {
                            KeyCode::Char(c) => {
                                ui.custom_text.insert(ui.custom_cursor, c);
                                ui.custom_cursor += c.len_utf8();
                            }
                            KeyCode::Backspace => {
                                if ui.custom_cursor > 0 {
                                    let c = ui.custom_text.remove(ui.custom_cursor - 1);
                                    ui.custom_cursor -= c.len_utf8();
                                }
                            }
                            KeyCode::Enter => {
                                let text = ui.custom_text.trim().to_string();
                                if text.is_empty() {
                                    // Back out of custom mode
                                    ui.custom_mode = false;
                                } else {
                                    let answer = text;
                                    if let Some(p) = app.pending_user_input.take() {
                                        app.push(ChatMsg::Info(format!("  ↳ {answer}")));
                                        let _ = p.respond.send(answer);
                                        app.spinning = true;
                                    }
                                }
                            }
                            KeyCode::Esc => {
                                ui.custom_mode = false;
                                ui.custom_text.clear();
                                ui.custom_cursor = 0;
                            }
                            _ => {}
                        }
                    } else {
                        match key.code {
                            KeyCode::Up => {
                                if ui.cursor > 0 { ui.cursor -= 1; }
                            }
                            KeyCode::Down => {
                                if ui.cursor < last { ui.cursor += 1; }
                            }
                            KeyCode::Char(' ') if ui.mode == InputMode::ChooseMany => {
                                if ui.cursor == last {
                                    // Space on custom → enter custom mode
                                    ui.custom_mode = true;
                                } else {
                                    ui.selected[ui.cursor] = !ui.selected[ui.cursor];
                                }
                            }
                            KeyCode::Enter => {
                                match ui.mode {
                                    InputMode::ChooseOne => {
                                        if ui.cursor == last {
                                            ui.custom_mode = true;
                                        } else {
                                            let answer = ui.options[ui.cursor].clone();
                                            if let Some(p) = app.pending_user_input.take() {
                                                app.push(ChatMsg::Info(format!("  ↳ {answer}")));
                                                let _ = p.respond.send(answer);
                                                app.spinning = true;
                                            }
                                        }
                                    }
                                    InputMode::ChooseMany => {
                                        if ui.cursor == last && !ui.custom_mode {
                                            ui.custom_mode = true;
                                        } else {
                                            // Collect selected options + custom text
                                            let mut parts: Vec<String> = ui
                                                .options[..last]
                                                .iter()
                                                .enumerate()
                                                .filter(|(i, _)| ui.selected[*i])
                                                .map(|(_, o)| o.clone())
                                                .collect();
                                            if !ui.custom_text.trim().is_empty() {
                                                parts.push(ui.custom_text.trim().to_string());
                                            }
                                            if parts.is_empty() {
                                                // Nothing selected — require at least one
                                            } else {
                                                let answer = parts.join(", ");
                                                if let Some(p) = app.pending_user_input.take() {
                                                    app.push(ChatMsg::Info(format!("  ↳ {answer}")));
                                                    let _ = p.respond.send(answer);
                                                    app.spinning = true;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            KeyCode::Esc => {
                                // Cancel: send back an empty string and let the agent handle it
                                if let Some(p) = app.pending_user_input.take() {
                                    app.push(ChatMsg::Info("  ↳ (cancelled)".into()));
                                    let _ = p.respond.send(String::new());
                                    app.spinning = true;
                                }
                            }
                            _ => {}
                        }
                    }
                    continue 'main;
                }

                // ── Permission prompt: intercept y / a / n ────────────────────
                if app.pending_permission.is_some() {
                    match key.code {
                        // Allow once
                        KeyCode::Char('y') | KeyCode::Enter => {
                            if let Some(p) = app.pending_permission.take() {
                                app.push(ChatMsg::Info(format!("  ✓ allowed: {}", p.tool_name)));
                                let _ = p.respond.send(true);
                                app.spinning = true;
                            }
                        }
                        // Allow always (add to approved set)
                        KeyCode::Char('a') => {
                            if let Some(p) = app.pending_permission.take() {
                                app.approved_tools.insert(p.tool_name.clone());
                                app.push(ChatMsg::Info(format!(
                                    "  ✓ always allow: {}",
                                    p.tool_name
                                )));
                                let _ = p.respond.send(true);
                                app.spinning = true;
                            }
                        }
                        // Deny
                        KeyCode::Char('n') | KeyCode::Esc => {
                            if let Some(p) = app.pending_permission.take() {
                                app.push(ChatMsg::Info(format!("  ✗ denied: {}", p.tool_name)));
                                let _ = p.respond.send(false);
                                app.spinning = true;
                            }
                        }
                        _ => {}
                    }
                    continue 'main;
                }

                // Ignore editing while busy
                if app.spinning || stream_rx.is_some() {
                    continue 'main;
                }

                match key.code {
                    // Tab: autocomplete selected suggestion
                    KeyCode::Tab => {
                        if app.input.starts_with('@') && !app.input.contains(' ') {
                            let at_sugg = at_suggestions(&app.input[1..], &app.personas);
                            if !at_sugg.is_empty() {
                                let idx = app.suggest_idx.unwrap_or(0);
                                app.input = format!("@{}", at_sugg[idx].0);
                                app.cursor = app.input.len();
                                app.suggest_idx = None;
                            }
                        } else {
                            let suggestions = slash_suggestions(&app.input);
                            if !suggestions.is_empty() {
                                let idx = app.suggest_idx.unwrap_or(0);
                                app.input = suggestions[idx].0.to_string();
                                app.cursor = app.input.len();
                                app.suggest_idx = None;
                            }
                        }
                        continue 'main;
                    }
                    // Escape: dismiss suggestion popup
                    KeyCode::Esc => {
                        app.suggest_idx = None;
                        continue 'main;
                    }
                    KeyCode::Left  => { app.suggest_idx = None; app.cursor_left(); }
                    KeyCode::Right => { app.suggest_idx = None; app.cursor_right(); }
                    KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.cursor = 0;
                    }
                    KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.cursor = app.input.len();
                    }
                    KeyCode::Backspace => { app.suggest_idx = None; app.backspace(); }

                    // History: Ctrl+P / Ctrl+N
                    KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if !app.history.is_empty() {
                            let idx = app.history_idx
                                .map(|i| i.saturating_sub(1))
                                .unwrap_or(app.history.len() - 1);
                            app.history_idx = Some(idx);
                            app.input = app.history[idx].clone();
                            app.cursor = app.input.len();
                        }
                    }
                    KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Some(idx) = app.history_idx {
                            if idx + 1 < app.history.len() {
                                app.history_idx = Some(idx + 1);
                                app.input = app.history[idx + 1].clone();
                            } else {
                                app.history_idx = None;
                                app.input.clear();
                            }
                            app.cursor = app.input.len();
                        }
                    }

                    KeyCode::Enter => {
                        // If a slash suggestion is selected, complete it instead of submitting
                        let slash_sugg = slash_suggestions(&app.input);
                        if !slash_sugg.is_empty() && app.suggest_idx.is_some() {
                            let idx = app.suggest_idx.unwrap();
                            app.input = slash_sugg[idx].0.to_string();
                            app.cursor = app.input.len();
                            app.suggest_idx = None;
                            continue 'main;
                        }
                        // If an @<name> suggestion is selected, complete it
                        if app.input.starts_with('@') && !app.input.contains(' ') {
                            let at_sugg = at_suggestions(&app.input[1..], &app.personas);
                            if !at_sugg.is_empty() && app.suggest_idx.is_some() {
                                let idx = app.suggest_idx.unwrap();
                                app.input = format!("@{}", at_sugg[idx].0);
                                app.cursor = app.input.len();
                                app.suggest_idx = None;
                                continue 'main;
                            }
                        }
                        app.suggest_idx = None;
                        let input = app.input.trim().to_string();
                        if input.is_empty() { continue 'main; }
                        app.history.push(input.clone());
                        app.history_idx = None;
                        app.input.clear();
                        app.cursor = 0;
                        app.auto_scroll = true;
                        app.scroll = u16::MAX;

                        // @<name> alone — activate persona
                        if input.starts_with('@') && !input.contains(' ') {
                            let name = input[1..].trim();
                            // Rediscover if personas not loaded
                            if app.personas.is_empty() {
                                app.personas = AgentPersona::discover();
                            }
                            if let Some(pos) = app.personas.iter().position(|p| p.name == name) {
                                let persona = app.personas.remove(pos);
                                // Optionally switch provider
                                if persona.model.is_some() || persona.provider.is_some() {
                                    let new_model = persona.model.as_deref().unwrap_or(&creds.model);
                                    let new_prov = persona.provider.as_deref().unwrap_or(&creds.provider);
                                    let new_creds = Credentials {
                                        provider: new_prov.to_string(),
                                        model: new_model.to_string(),
                                        ..creds.clone()
                                    };
                                    provider = Arc::from(new_creds.build_provider());
                                    app.push(ChatMsg::Info(format!(
                                        "switched model to {} / {}",
                                        new_prov, new_model
                                    )));
                                }
                                app.push(ChatMsg::Info(format!(
                                    "switched to persona '@{}'",
                                    persona.name
                                )));
                                app.personas.insert(pos, persona);
                                // Activate — re-borrow by index
                                let persona_name = app.personas[pos].name.clone();
                                app.active_persona = Some(AgentPersona {
                                    name: app.personas[pos].name.clone(),
                                    description: app.personas[pos].description.clone(),
                                    model: app.personas[pos].model.clone(),
                                    provider: app.personas[pos].provider.clone(),
                                    system_prompt: app.personas[pos].system_prompt.clone(),
                                    path: app.personas[pos].path.clone(),
                                });
                                app.persona_text = app.personas[pos].system_prompt.clone();
                                let _ = persona_name; // used above
                            } else {
                                app.push(ChatMsg::Error(format!(
                                    "persona '@{}' not found — use /agents list to see available personas",
                                    name
                                )));
                            }
                            continue 'main;
                        }

                        match input.as_str() {
                            "/quit" => break 'main,
                            "/clear" => {
                                app.chat.clear();
                                messages.clear();
                                active_resume_id = None;
                                app.total_input = 0;
                                app.total_output = 0;
                            }
                            s if s.starts_with("/resume ") => {
                                let sid = s.strip_prefix("/resume ").unwrap_or("").trim();
                                if sid.is_empty() {
                                    app.push(ChatMsg::Error("usage: /resume <session-id>".into()));
                                } else {
                                    let (history, display_msgs) =
                                        load_resume_history(&krabs_config, sid).await;
                                    if history.is_empty() {
                                        app.push(ChatMsg::Error(format!(
                                            "Session {sid} not found or empty"
                                        )));
                                    } else {
                                        app.chat.clear();
                                        messages.clear();
                                        app.total_input = 0;
                                        app.total_output = 0;
                                        for dm in display_msgs {
                                            app.chat.push(dm);
                                        }
                                        messages = history;
                                        active_resume_id = Some(sid.to_string());
                                        app.push(ChatMsg::Info(format!(
                                            "Resumed session {sid}"
                                        )));
                                    }
                                }
                            }
                            "/tools"  => cmd_tools(&mut app, &registry),
                            "/skills" => cmd_skills(&mut app, &krabs_config.skills),
                            s if s == "/mcp" || s.starts_with("/mcp ") => {
                                let mcp_args = s.strip_prefix("/mcp").unwrap_or("").trim();
                                cmd_mcp(&mut app, mcp_args).await;
                            }
                            "/usage"  => cmd_usage(&mut app, max_ctx, &krabs_config.skills),
                            s if s == "/agents" || s.starts_with("/agents ") => {
                                let args = s.strip_prefix("/agents").unwrap_or("").trim();
                                cmd_agents(&mut app, args);
                            }
                            s if s == "/hooks" || s.starts_with("/hooks ") => {
                                let args = s.strip_prefix("/hooks").unwrap_or("").trim();
                                cmd_hooks(&mut app, args);
                            }
                            s if s == "/models" || s.starts_with("/models ") => {
                                let args = s.strip_prefix("/models").unwrap_or("").trim();
                                cmd_models(
                                    &mut app, args, &mut creds,
                                    &mut provider, &mut info, &mut max_ctx,
                                    &krabs_config.custom_models,
                                );
                            }
                            _ => {
                                app.push(ChatMsg::User(input.clone()));

                                // Build effective messages: prepend system prompt if persona active
                                let mut turn_messages = messages.clone();
                                if let Some(ref persona) = app.active_persona {
                                    let base_prompt = format!(
                                        "You are Krabs, an agentic assistant.\n\n---\n\n{}",
                                        persona.system_prompt
                                    );
                                    // Only prepend if no system message exists yet
                                    let has_system = turn_messages
                                        .first()
                                        .map(|m| matches!(m.role, Role::System))
                                        .unwrap_or(false);
                                    if !has_system {
                                        turn_messages.insert(0, Message::system(&base_prompt));
                                    }
                                }
                                turn_messages.push(Message::user(&input));
                                messages.push(Message::user(&input));
                                app.spinning = true;

                                // Capture context breakdown estimates (once per turn)
                                const BASE_SYSTEM_PROMPT: &str = "You are Krabs, an agentic assistant.";
                                app.system_prompt_text = BASE_SYSTEM_PROMPT.to_string();
                                app.tools_text = serde_json::to_string(&registry.tool_defs())
                                    .unwrap_or_default();

                                let (tx, rx) = mpsc::channel::<DisplayEvent>(64);
                                stream_rx = Some(rx);

                                let agent = build_agent(
                                    &krabs_config,
                                    Arc::clone(&provider),
                                    Arc::clone(&registry),
                                    String::new(), // system prompt injected by KrabsAgent
                                    tx.clone(),
                                    active_resume_id.take(),
                                )
                                .await;
                                turn_handle = Some(tokio::spawn(run_agent_turn(
                                    agent,
                                    turn_messages,
                                    tx,
                                )));
                            }
                        }
                    }

                    KeyCode::Char(c) => { app.suggest_idx = None; app.insert_char(c); }
                    _ => {}
                }
            }

            // ── stream events ──
            ev = recv_event(&mut stream_rx) => {
                match ev {
                    None => {
                        if app.spinning {
                            app.push(ChatMsg::Error("stream closed unexpectedly".into()));
                        }
                        app.spinning = false;
                        stream_rx = None;
                    }
                    Some(DisplayEvent::Token(t)) => {
                        app.spinning = false;
                        match app.chat.last_mut() {
                            Some(ChatMsg::Assistant(s)) => s.push_str(&t),
                            _ => app.chat.push(ChatMsg::Assistant(t)),
                        }
                        if app.auto_scroll { app.scroll = u16::MAX; }
                    }
                    Some(DisplayEvent::PermissionRequest { tool_name, args, respond }) => {
                        app.spinning = false;
                        // Auto-allow tools the user already approved as "always allow"
                        if app.approved_tools.contains(&tool_name) {
                            let _ = respond.send(true);
                        } else {
                            app.pending_permission = Some(PendingPermission {
                                tool_name,
                                args,
                                respond,
                            });
                        }
                    }
                    Some(DisplayEvent::UserInput(req)) => {
                        app.spinning = false;
                        // Build the options list: user options + "custom…" sentinel
                        let mut options = req.options.clone();
                        options.push("custom…".into());
                        let n = options.len();
                        app.pending_user_input = Some(PendingUserInput {
                            mode: req.mode,
                            question: req.question,
                            options,
                            selected: vec![false; n],
                            cursor: 0,
                            custom_mode: false,
                            custom_text: String::new(),
                            custom_cursor: 0,
                            respond: req.respond,
                        });
                    }
                    Some(DisplayEvent::ToolCallStart(call)) => {
                        app.spinning = false;
                        app.push(ChatMsg::ToolCall(format!("{} {}", call.name, call.args)));
                    }
                    Some(DisplayEvent::ToolResultEnd(content)) => {
                        app.push(ChatMsg::ToolResult(content));
                        app.spinning = true; // next LLM turn starting
                    }
                    Some(DisplayEvent::TurnUsage(u)) => {
                        app.total_input += u.input_tokens;
                        app.total_output += u.output_tokens;
                        app.push(ChatMsg::Usage(u.input_tokens, u.output_tokens));
                    }
                    Some(DisplayEvent::Done(final_msgs)) => {
                        messages = final_msgs;
                        app.spinning = false;
                        stream_rx = None;
                        turn_handle = None;
                    }
                    Some(DisplayEvent::Error(msg)) => {
                        app.spinning = false;
                        stream_rx = None;
                        turn_handle = None;
                        app.push(ChatMsg::Error(msg));
                    }
                }
            }

            // ── spinner tick ──
            _ = tokio::time::sleep(Duration::from_millis(80)) => {
                if app.spinning { app.spin_i += 1; }
            }
        }
    }

    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    Ok(())
}
