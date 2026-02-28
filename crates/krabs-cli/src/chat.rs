use anyhow::Result;
use crossterm::{
    event::DisableMouseCapture,
    event::EnableMouseCapture,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use krabs_core::{
    skills::loader::SkillLoader, AgentPersona, BashTool, Credentials, GlobTool, GrepTool,
    HookConfig, HookEntry, KrabsConfig, LlmProvider, McpRegistry, McpServer, Message, ReadTool,
    Role, SkillsConfig, StreamChunk, TokenUsage, ToolCall, ToolDef, ToolRegistry, WriteTool,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use std::{io, sync::Arc, time::Duration};
use tokio::sync::mpsc;

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
    ("/usage", "show context window usage"),
    ("/clear", "clear screen and conversation"),
    ("/quit", "exit Krabs"),
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

fn build_registry() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(Arc::new(BashTool));
    r.register(Arc::new(GlobTool));
    r.register(Arc::new(GrepTool));
    r.register(Arc::new(ReadTool));
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
    ToolCallStart(ToolCall),
    ToolResultEnd(String),
    TurnUsage(TokenUsage),
    Done(Vec<Message>),
    Error(String),
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
    personas: Vec<AgentPersona>,
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

// ── background agentic task ──────────────────────────────────────────────────

async fn run_turn(
    mut messages: Vec<Message>,
    provider: Arc<dyn LlmProvider>,
    tool_defs: Vec<ToolDef>,
    registry: Arc<ToolRegistry>,
    tx: mpsc::Sender<DisplayEvent>,
) {
    let mut iterations = 0usize;
    loop {
        iterations += 1;
        if iterations > 10 {
            let _ = tx
                .send(DisplayEvent::Error(
                    "agentic loop exceeded 10 iterations — stopping".into(),
                ))
                .await;
            return;
        }
        let (inner_tx, mut inner_rx) = mpsc::channel::<StreamChunk>(64);
        let mut text = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut usage = TokenUsage {
            input_tokens: 0,
            output_tokens: 0,
        };
        let mut got_done = false;

        let p2 = Arc::clone(&provider);
        let m2 = messages.clone();
        let d2 = tool_defs.clone();
        let tx2 = tx.clone();
        let handle = tokio::spawn(async move {
            if let Err(e) = p2.stream_complete(&m2, &d2, inner_tx).await {
                let msg = extract_api_error(&e.to_string());
                let _ = tx2.send(DisplayEvent::Error(msg)).await;
            }
        });

        while let Some(chunk) = inner_rx.recv().await {
            match chunk {
                StreamChunk::Delta { text: t } => {
                    text.push_str(&t);
                    if tx.send(DisplayEvent::Token(t)).await.is_err() {
                        return;
                    }
                }
                StreamChunk::ToolCallReady { call } => {
                    calls.push(call.clone());
                    if tx.send(DisplayEvent::ToolCallStart(call)).await.is_err() {
                        return;
                    }
                }
                StreamChunk::Done { usage: u } => {
                    usage = u;
                    got_done = true;
                }
            }
        }

        // If provider errored it sends DisplayEvent::Error and closes inner_tx without Done
        let _ = handle.await;
        if !got_done {
            return;
        }

        let _ = tx.send(DisplayEvent::TurnUsage(usage)).await;

        // push assistant turn to conversation history
        if calls.is_empty() {
            messages.push(Message::assistant(&text));
        } else {
            messages.push(Message::assistant_tool_calls(calls.clone()));
        }

        if calls.is_empty() {
            let _ = tx.send(DisplayEvent::Done(messages)).await;
            return;
        }

        // execute tool calls
        for call in calls {
            let result = match registry.get(&call.name) {
                Some(tool) => match tool.call(call.args.clone()).await {
                    Ok(r) => r,
                    Err(e) => krabs_core::ToolResult::err(e.to_string()),
                },
                None => krabs_core::ToolResult::err(format!("tool '{}' not found", call.name)),
            };
            let content = result.content.clone();
            if tx
                .send(DisplayEvent::ToolResultEnd(content.clone()))
                .await
                .is_err()
            {
                return;
            }
            messages.push(Message::tool_result(&content, &call.id));
        }
        // loop → next LLM turn
    }
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
            let personas = AgentPersona::discover();
            app.personas = personas;
            if app.personas.is_empty() {
                app.push(ChatMsg::Info(
                    "no agent personas found — add markdown files to ./krabs/agents/".into(),
                ));
            } else {
                let lines: Vec<String> = {
                    let mut v = vec![format!("{} persona(s):", app.personas.len())];
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
    let filled = (pct / 5.0).round() as usize;
    let ctx_bar = format!(
        "[{}{}] {:.1}%",
        "█".repeat(filled),
        "░".repeat(20 - filled),
        pct
    );
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
        Line::from(vec![
            Span::styled("  ctx     ", Style::default().fg(Color::DarkGray)),
            Span::styled(ctx_bar, Style::default().fg(Color::Yellow)),
        ]),
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

fn cmd_usage(app: &mut App, max_ctx: u32) {
    let used = app.total_input + app.total_output;
    let pct = (used as f32 / max_ctx as f32 * 100.0).min(100.0);
    let filled = (pct / 5.0).round() as usize;
    let bar = format!("[{}{}]", "█".repeat(filled), "░".repeat(20 - filled));
    app.push(ChatMsg::Info(format!("context  {bar}  {pct:.1}%")));
    app.push(ChatMsg::Info(format!(
        "input    {} tokens",
        app.total_input
    )));
    app.push(ChatMsg::Info(format!(
        "output   {} tokens",
        app.total_output
    )));
    app.push(ChatMsg::Info(format!(
        "total    {} / {} tokens",
        used, max_ctx
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

pub async fn run(creds: Credentials) -> Result<()> {
    let krabs_config = KrabsConfig::load().unwrap_or_default();
    let mut provider: Arc<dyn LlmProvider> = Arc::from(creds.build_provider());
    let registry = Arc::new(build_registry());
    let tool_defs = registry.tool_defs();
    let max_ctx = context_limit(&creds.model);
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".into());
    let info = InfoBar {
        provider: creds.provider.clone(),
        model: creds.model.clone(),
        cwd,
        tools: registry.names().join(", "),
    };

    // Terminal setup
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
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
    let mut stream_rx: Option<mpsc::Receiver<DisplayEvent>> = None;
    let mut turn_handle: Option<tokio::task::JoinHandle<()>> = None;

    'main: loop {
        terminal.draw(|f| render(&mut app, max_ctx, &info, f))?;

        tokio::select! {
            // ── keyboard ──
            key = key_rx.recv() => {
                let Some(ev) = key else { break };

                // Mouse scroll
                if let Event::Mouse(m) = ev {
                    match m.kind {
                        MouseEventKind::ScrollUp => {
                            app.auto_scroll = false;
                            app.scroll = app.scroll.saturating_sub(3);
                        }
                        MouseEventKind::ScrollDown => {
                            app.scroll = app.scroll.saturating_add(3);
                            if app.scroll == u16::MAX { app.auto_scroll = true; }
                        }
                        _ => {}
                    }
                    continue 'main;
                }

                let Event::Key(key) = ev else { continue 'main };
                if key.kind != KeyEventKind::Press { continue 'main; }

                // Ctrl+C: cancel turn if running, quit if idle
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    if app.spinning || stream_rx.is_some() {
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
                            // history navigation
                            if !app.history.is_empty() {
                                let idx = app.history_idx
                                    .map(|i| i.saturating_sub(1))
                                    .unwrap_or(app.history.len() - 1);
                                app.history_idx = Some(idx);
                                app.input = app.history[idx].clone();
                                app.cursor = app.input.len();
                            }
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
                            // history navigation forward
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
                        continue 'main;
                    }
                    KeyCode::Up => {
                        app.auto_scroll = false;
                        app.scroll = app.scroll.saturating_sub(1);
                        continue 'main;
                    }
                    KeyCode::Down => {
                        app.scroll = app.scroll.saturating_add(1);
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
                                app.total_input = 0;
                                app.total_output = 0;
                            }
                            "/tools"  => cmd_tools(&mut app, &registry),
                            "/skills" => cmd_skills(&mut app, &krabs_config.skills),
                            s if s == "/mcp" || s.starts_with("/mcp ") => {
                                let mcp_args = s.strip_prefix("/mcp").unwrap_or("").trim();
                                cmd_mcp(&mut app, mcp_args).await;
                            }
                            "/usage"  => cmd_usage(&mut app, max_ctx),
                            s if s == "/agents" || s.starts_with("/agents ") => {
                                let args = s.strip_prefix("/agents").unwrap_or("").trim();
                                cmd_agents(&mut app, args);
                            }
                            s if s == "/hooks" || s.starts_with("/hooks ") => {
                                let args = s.strip_prefix("/hooks").unwrap_or("").trim();
                                cmd_hooks(&mut app, args);
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

                                let (tx, rx) = mpsc::channel::<DisplayEvent>(64);
                                stream_rx = Some(rx);

                                turn_handle = Some(tokio::spawn(run_turn(
                                    turn_messages,
                                    Arc::clone(&provider),
                                    tool_defs.clone(),
                                    Arc::clone(&registry),
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

    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}
