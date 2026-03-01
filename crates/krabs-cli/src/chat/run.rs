use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use krabs_core::{AgentPersona, Credentials, KrabsConfig, LlmProvider, Message, Role};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use tokio::sync::mpsc;

use super::agent::{build_agent, run_agent_turn};
use super::app::App;
use super::commands::{
    at_suggestions, build_registry, cmd_agents, cmd_hooks, cmd_mcp, cmd_models, cmd_skills,
    cmd_tools, cmd_usage, context_limit, load_resume_history, slash_suggestions,
};
use super::render::{render, show_splash};
use super::types::{ChatMsg, DisplayEvent, InfoBar, PendingPermission, PendingUserInput};

// ── async helper: recv or park ───────────────────────────────────────────────

async fn recv_event(rx: &mut Option<mpsc::Receiver<DisplayEvent>>) -> Option<DisplayEvent> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
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

                let busy = app.spinning || stream_rx.is_some();

                // Scroll (always available)
                match key.code {
                    KeyCode::Up if !busy => {
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
                    KeyCode::Down if !busy => {
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
                            KeyCode::Char(' ') if ui.mode == krabs_core::InputMode::ChooseMany => {
                                if ui.cursor == last {
                                    // Space on custom → enter custom mode
                                    ui.custom_mode = true;
                                } else {
                                    ui.selected[ui.cursor] = !ui.selected[ui.cursor];
                                }
                            }
                            KeyCode::Enter => {
                                match ui.mode {
                                    krabs_core::InputMode::ChooseOne => {
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
                                    krabs_core::InputMode::ChooseMany => {
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

                        // Queue message if a turn is running; it will be dispatched on Done.
                        if busy {
                            app.push(ChatMsg::User(input.clone()));
                            app.queued_input = Some(input);
                            continue 'main;
                        }

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
                            "/usage"  => cmd_usage(&mut app, max_ctx, &krabs_config.skills).await,
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
                    Some(DisplayEvent::Done { messages: final_msgs, session_id }) => {
                        messages = final_msgs;
                        app.spinning = false;
                        stream_rx = None;
                        turn_handle = None;

                        // Update the resume ID so the next turn continues the same session.
                        if session_id.is_some() {
                            active_resume_id = session_id;
                        }

                        // Flush any message queued while the turn was running.
                        if let Some(queued) = app.queued_input.take() {
                            // ChatMsg::User was already pushed at queue time; just dispatch.
                            let mut turn_messages = messages.clone();
                            turn_messages.push(Message::user(&queued));
                            messages.push(Message::user(&queued));
                            app.spinning = true;
                            let (tx, rx) = mpsc::channel::<DisplayEvent>(64);
                            stream_rx = Some(rx);
                            let agent = build_agent(
                                &krabs_config,
                                Arc::clone(&provider),
                                Arc::clone(&registry),
                                String::new(),
                                tx.clone(),
                                active_resume_id.take(),
                            )
                            .await;
                            turn_handle = Some(tokio::spawn(run_agent_turn(agent, turn_messages, tx)));
                        }
                    }
                    Some(DisplayEvent::Error { message, session_id }) => {
                        app.spinning = false;
                        stream_rx = None;
                        turn_handle = None;
                        app.push(ChatMsg::Error(message));

                        if session_id.is_some() {
                            active_resume_id = session_id;
                        }

                        // If a message was queued while this turn was running, dispatch it
                        // now so it isn't silently lost. The queued ChatMsg::User was already
                        // pushed to the chat at queue time.
                        if let Some(queued) = app.queued_input.take() {
                            let mut turn_messages = messages.clone();
                            turn_messages.push(Message::user(&queued));
                            messages.push(Message::user(&queued));
                            app.spinning = true;
                            let (tx, rx) = mpsc::channel::<DisplayEvent>(64);
                            stream_rx = Some(rx);
                            let agent = build_agent(
                                &krabs_config,
                                Arc::clone(&provider),
                                Arc::clone(&registry),
                                String::new(),
                                tx.clone(),
                                active_resume_id.take(),
                            )
                            .await;
                            turn_handle = Some(tokio::spawn(run_agent_turn(agent, turn_messages, tx)));
                        }
                    }
                    Some(DisplayEvent::Status(text)) => {
                        app.push(ChatMsg::Info(text));
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
