use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use std::time::Duration;
use tokio::sync::mpsc;
use crossterm::event::{Event, KeyEventKind};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use anyhow::Result;

use super::app::App;
use super::commands::{at_suggestions, slash_suggestions};
use super::types::{estimate_tokens, InfoBar};

pub(super) const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub(super) const LOGO: &[&str] = &[
    "██╗  ██╗██████╗  █████╗ ██████╗ ███████╗",
    "██║ ██╔╝██╔══██╗██╔══██╗██╔══██╗██╔════╝",
    "█████╔╝ ██████╔╝███████║██████╔╝███████╗",
    "██╔═██╗ ██╔══██╗██╔══██║██╔══██╗╚════██║",
    "██║  ██╗██║  ██║██║  ██║██████╔╝███████║",
    "╚═╝  ╚═╝╚═╝  ╚═╝╚═╝  ╚═╝╚═════╝ ╚══════╝",
];

pub(super) const MR_KRABS_ORANGE: Color = Color::Rgb(255, 128, 0);

pub(super) fn render(app: &mut App, max_ctx: u32, info: &InfoBar, frame: &mut Frame) {
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
        use krabs_core::InputMode;
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

pub(super) async fn show_splash(
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
