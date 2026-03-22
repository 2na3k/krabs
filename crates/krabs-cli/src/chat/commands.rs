use std::sync::Arc;

use krabs_core::{
    skills::loader::SkillLoader, AgentPersona, BaseAgent, Credentials, CustomModelEntry,
    HookConfig, HookEntry, KrabsConfig, LlmProvider, McpRegistry, McpServer, Message, SkillsConfig,
    ToolRegistry,
};

use super::app::App;
use super::types::{ChatMsg, InfoBar, ModelEntry, ModelPicker};

// ── constants ────────────────────────────────────────────────────────────────

pub(super) const SLASH_COMMANDS: &[(&str, &str)] = &[
    (
        "/tools",
        "list available tools  usage: /tools [allow <name>|deny <name>]",
    ),
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
    ("/models", "open model picker"),
    ("/usage", "show context window usage"),
    ("/clear", "clear screen and conversation"),
    ("/resume", "resume a session  usage: /resume <session-id>"),
    ("/quit", "exit Krabs"),
];

/// Well-known models grouped by provider. Used by `/models` for display and tab-completion.
pub(super) const KNOWN_MODELS: &[(&str, &[&str])] = &[
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
            "gemini-3-flash-preview",
        ],
    ),
    (
        "
        ",
        &["llama3.2", "mistral", "codestral", "qwen2.5-coder"],
    ),
];

pub(super) fn context_limit(model: &str) -> u32 {
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

pub(super) fn slash_suggestions(prefix: &str) -> Vec<(&'static str, &'static str)> {
    SLASH_COMMANDS
        .iter()
        .filter(|(cmd, _)| cmd.starts_with(prefix))
        .copied()
        .collect()
}

/// Return persona names whose names start with `prefix` (after stripping `@`).
pub(super) fn at_suggestions<'a>(
    prefix: &str,
    personas: &'a [AgentPersona],
) -> Vec<(&'a str, &'a str)> {
    personas
        .iter()
        .filter(|p| p.name.starts_with(prefix))
        .map(|p| (p.name.as_str(), p.description.as_deref().unwrap_or("")))
        .collect()
}

pub(super) fn cmd_agents(app: &mut App, args: &str) {
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

/// Build the full list of selectable model entries for the picker.
pub(super) fn build_model_entries(
    creds: &Credentials,
    custom_models: &[CustomModelEntry],
) -> (Vec<ModelEntry>, usize) {
    let mut entries: Vec<ModelEntry> = Vec::new();

    // Built-in known models
    for (prov, models) in KNOWN_MODELS {
        for m in *models {
            entries.push(ModelEntry {
                group: prov.trim().to_string(),
                label: m.to_string(),
                provider: prov.trim().to_string(),
                model: m.to_string(),
                base_url: None,
                api_key: None,
            });
        }
    }

    // Custom model entries from config
    for entry in custom_models {
        entries.push(ModelEntry {
            group: "custom".into(),
            label: format!("{} ({})", entry.name, entry.model),
            provider: entry.provider.clone(),
            model: entry.model.clone(),
            base_url: Some(entry.base_url.clone()),
            api_key: if entry.api_key.is_empty() {
                None
            } else {
                Some(entry.api_key.clone())
            },
        });
    }

    // If the current active model isn't in any group, inject it at the top
    // so it's always visible and selectable (e.g. a local llama.cpp model
    // configured via base_url in .krabs.json).
    // An entry counts as "present" only if the model name matches AND either
    // the entry is a standard known model (no explicit base_url) with the same
    // name, OR it's a custom entry with an exact base_url match.
    let already_present = entries.iter().any(|e| {
        e.model == creds.model
            && (e.base_url.is_none() || e.base_url.as_deref() == Some(creds.base_url.as_str()))
    });
    if !already_present {
        entries.insert(
            0,
            ModelEntry {
                group: "active".into(),
                label: creds.model.clone(),
                provider: creds.provider.clone(),
                model: creds.model.clone(),
                base_url: Some(creds.base_url.clone()),
                api_key: None,
            },
        );
    }

    // Find index of active model
    let active = entries
        .iter()
        .position(|e| {
            e.model == creds.model
                && (e.base_url.is_none() || e.base_url.as_deref() == Some(creds.base_url.as_str()))
        })
        .unwrap_or(0);

    (entries, active)
}

/// Apply a selected ModelEntry to the live credentials + provider.
pub(super) fn apply_model_entry(
    entry: &ModelEntry,
    creds: &mut Credentials,
    provider: &mut Arc<dyn LlmProvider>,
    info: &mut InfoBar,
    max_ctx: &mut u32,
) {
    creds.provider = entry.provider.clone();
    creds.model = entry.model.clone();
    if let Some(url) = &entry.base_url {
        creds.base_url = url.clone();
    }
    if let Some(key) = &entry.api_key {
        creds.api_key = key.clone();
    }
    *provider = Arc::from(creds.build_provider());
    *max_ctx = context_limit(&creds.model);
    info.provider = creds.provider.clone();
    info.model = creds.model.clone();
}

/// /models — always opens the interactive model picker popup.
pub(super) fn cmd_models(
    app: &mut App,
    _args: &str,
    creds: &Credentials,
    _provider: &mut Arc<dyn LlmProvider>,
    _info: &mut InfoBar,
    _max_ctx: &mut u32,
    custom_models: &[CustomModelEntry],
) {
    let (entries, active) = build_model_entries(creds, custom_models);
    let scroll = active.saturating_sub(4);
    app.model_picker = Some(ModelPicker {
        entries,
        cursor: active,
        scroll,
    });
}

pub(super) fn cmd_tools(app: &mut App, registry: &ToolRegistry) {
    app.push(ChatMsg::Info("available tools:".into()));
    for d in registry.tool_defs() {
        let approved = if app.approved_tools.contains(&d.name) {
            " [auto-approved]"
        } else {
            ""
        };
        app.push(ChatMsg::Info(format!(
            "  {:15}  {}{}",
            d.name, d.description, approved
        )));
    }
    app.push(ChatMsg::Info(
        "  /tools allow <name>  pre-approve a tool (no popup)".into(),
    ));
    app.push(ChatMsg::Info(
        "  /tools deny <name>   remove a pre-approval".into(),
    ));
}

pub(super) fn cmd_tools_allow(app: &mut App, name: &str) {
    if name.is_empty() {
        app.push(ChatMsg::Error("usage: /tools allow <tool-name>".into()));
        return;
    }
    app.approved_tools.insert(name.to_string());
    app.push(ChatMsg::Info(format!(
        "  ✓ '{name}' pre-approved — permission popup disabled"
    )));
}

pub(super) fn cmd_tools_deny(app: &mut App, name: &str) {
    if name.is_empty() {
        app.push(ChatMsg::Error("usage: /tools deny <tool-name>".into()));
        return;
    }
    if app.approved_tools.remove(name) {
        app.push(ChatMsg::Info(format!(
            "  ✗ '{name}' approval removed — popup will appear again"
        )));
    } else {
        app.push(ChatMsg::Info(format!("  '{name}' was not pre-approved")));
    }
}

pub(super) fn cmd_skills(app: &mut App, skills_config: &SkillsConfig) {
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
pub(super) async fn cmd_mcp(app: &mut App, args: &str) {
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
pub(super) fn cmd_hooks(app: &mut App, args: &str) {
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
pub(super) fn parse_hook_rest(rest: &[&str]) -> (Option<String>, String, Option<String>) {
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

pub(super) async fn cmd_usage(app: &mut App, max_ctx: u32, skills_config: &SkillsConfig) {
    use super::types::{estimate_tokens, fmt_k};
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
        (seg(t_system), 'S', ratatui::style::Color::Green),
        (seg(t_persona), 'P', ratatui::style::Color::Rgb(255, 128, 0)),
        (seg(t_tools), 'T', ratatui::style::Color::Magenta),
        (seg(t_skills), 'K', ratatui::style::Color::LightGreen),
        (seg(t_memory), 'M', ratatui::style::Color::Blue),
        (seg(t_messages), 'C', ratatui::style::Color::Cyan),
        (seg(t_free), 'F', ratatui::style::Color::DarkGray),
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

/// Load a persisted session's history and convert it to display messages.
/// Returns `(messages_for_agent, display_messages_for_tui, subturn_resume)`.
pub(super) async fn load_resume_history(
    config: &KrabsConfig,
    session_id: &str,
) -> (
    Vec<Message>,
    Vec<ChatMsg>,
    Option<krabs_core::SubturnResume>,
) {
    use krabs_core::{session::session::Session as KrabsSession, SessionStore};

    let store = match SessionStore::open(&config.db_path).await {
        Ok(s) => s,
        Err(_) => return (Vec::new(), Vec::new(), None),
    };
    let session = match store.load_session(session_id).await {
        Ok(s) => s,
        Err(_) => return (Vec::new(), Vec::new(), None),
    };

    let (stored, subturn_resume) = match session.latest_checkpoint().await {
        Ok(Some(cp)) => {
            let _ = session.rollback_to(cp.last_msg_id).await;
            let msgs = session
                .messages_up_to(cp.last_msg_id)
                .await
                .unwrap_or_default();
            let sr = cp.subturn_tool_idx.map(|idx| krabs_core::SubturnResume {
                turn: cp.turn,
                completed_tool_count: idx + 1,
                last_call_id: cp.subturn_call_id.unwrap_or_default(),
            });
            (msgs, sr)
        }
        _ => (session.messages().await.unwrap_or_default(), None),
    };

    let mut messages = Vec::new();
    let mut display: Vec<ChatMsg> = Vec::new();

    for s in &stored {
        if let Ok(msg) = KrabsSession::stored_to_message(s) {
            let dm = match s.role.as_str() {
                "user" => ChatMsg::User(s.content.clone()),
                "assistant" if s.tool_args.is_none() => ChatMsg::Assistant(s.content.clone()),
                _ => ChatMsg::Info(format!("[{}] {}", s.role, s.content)),
            };
            display.push(dm);
            messages.push(msg);
        }
    }

    (messages, display, subturn_resume)
}

pub(super) fn build_registry() -> ToolRegistry {
    ToolRegistry::with_defaults()
}
