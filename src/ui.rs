use crate::app::{ActivePane, App, InputTab, Overlay, ResponseTab, ToastKind};
use crate::engine::{status_reason, HttpMethod};
use crate::store::relative_time;
use crate::syntax::{self, Token};
use crate::telemetry::{fmt_bytes, fmt_duration, fmt_throughput};
use crate::viewer::column_slice_at;
use ratatui::{prelude::*, widgets::*};

const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

pub fn ui(f: &mut Frame, app: &mut App) {
    let area = f.area();

    // A cockpit crammed into a postage stamp is worse than an honest message.
    if area.width < 40 || area.height < 12 {
        f.render_widget(
            Paragraph::new("hcp needs at least 40x12.\nResize the terminal.")
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(Color::Yellow)),
            area,
        );
        return;
    }

    // The telemetry panel is the first thing to fold away on short terminals.
    let telemetry_height = if area.height >= 24 { 8 } else { 0 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(telemetry_height),
            Constraint::Length(1),
        ])
        .split(area);

    render_top_bar(f, app, chunks[0]);

    // Narrow terminals stack the panes instead of squeezing both.
    let split = if area.width >= 90 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(chunks[1])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(chunks[1])
    };

    app.layout.request = split[0];
    app.layout.response = split[1];
    render_request_pane(f, app, split[0]);
    render_response_pane(f, app, split[1]);

    if telemetry_height > 0 {
        render_telemetry(f, app, chunks[2]);
    }
    render_status_bar(f, app, chunks[3]);

    match app.overlay {
        Overlay::None => {}
        Overlay::Help => render_help(f, area),
        Overlay::History => render_history(f, app, area),
        Overlay::Collection => render_collection(f, app, area),
        Overlay::SavePrompt => render_save_prompt(f, app, area),
        Overlay::Search => render_search_prompt(f, app, area),
    }
}

// ---------------------------------------------------------------- top bar

fn method_color(m: HttpMethod) -> Color {
    match m {
        HttpMethod::Get => Color::Green,
        HttpMethod::Post => Color::Yellow,
        HttpMethod::Put => Color::Blue,
        HttpMethod::Patch => Color::Magenta,
        HttpMethod::Delete => Color::Red,
        HttpMethod::Head | HttpMethod::Options => Color::Cyan,
    }
}

fn focus_style(active: bool) -> Style {
    if active {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn render_top_bar(f: &mut Frame, app: &mut App, area: Rect) {
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(11), Constraint::Min(10)])
        .split(area);

    app.layout.method = top[0];
    app.layout.url = top[1];

    let m_color = method_color(app.method);
    let m_active = app.active_pane == ActivePane::MethodSelector;
    f.render_widget(
        Paragraph::new(app.method.as_str())
            .alignment(Alignment::Center)
            .style(if m_active {
                Style::default()
                    .fg(Color::Black)
                    .bg(m_color)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(m_color).add_modifier(Modifier::BOLD)
            })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" METHOD ")
                    .border_style(focus_style(m_active)),
            ),
        top[0],
    );

    let u_active = app.active_pane == ActivePane::UrlBar;
    let insecure_badge = if app.engine_config.insecure {
        " ⚠ TLS VERIFY OFF "
    } else {
        " ENDPOINT URL "
    };
    let url_block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            insecure_badge,
            if app.engine_config.insecure {
                Style::default().fg(Color::Black).bg(Color::Red)
            } else {
                Style::default()
            },
        ))
        .border_style(focus_style(u_active));

    let inner = url_block.inner(top[1]);
    f.render_widget(url_block, top[1]);
    style_editor(&mut app.url_editor, u_active);
    f.render_widget(&app.url_editor, inner);
}

/// Only the focused editor shows a cursor; otherwise every pane looks active.
fn style_editor(ta: &mut tui_textarea::TextArea, active: bool) {
    if active {
        ta.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
        ta.set_cursor_line_style(Style::default().add_modifier(Modifier::UNDERLINED));
    } else {
        ta.set_cursor_style(Style::default());
        ta.set_cursor_line_style(Style::default());
    }
}

// ---------------------------------------------------------- request pane

fn render_request_pane(f: &mut Frame, app: &mut App, area: Rect) {
    let active = app.active_pane == ActivePane::InputArea;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" REQUEST ")
        .border_style(focus_style(active));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);

    let body_label = if app.method.allows_body() {
        " 1 BODY "
    } else {
        " 1 BODY (unused) "
    };
    let tabs = Tabs::new(vec![body_label, " 2 HEADERS "])
        .select(match app.input_tab {
            InputTab::Body => 0,
            InputTab::Headers => 1,
        })
        .divider("│")
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, rows[0]);

    match app.input_tab {
        InputTab::Body => {
            style_editor(&mut app.body_editor, active);
            f.render_widget(&app.body_editor, rows[1]);
        }
        InputTab::Headers => {
            style_editor(&mut app.headers_editor, active);
            f.render_widget(&app.headers_editor, rows[1]);
        }
    }
}

// --------------------------------------------------------- response pane

fn status_color(status: u16) -> Color {
    match status {
        100..=199 => Color::Cyan,
        200..=299 => Color::Green,
        300..=399 => Color::Blue,
        400..=499 => Color::Yellow,
        500..=599 => Color::Red,
        _ => Color::DarkGray,
    }
}

fn render_response_pane(f: &mut Frame, app: &mut App, area: Rect) {
    let active = app.active_pane == ActivePane::ResponseViewer;
    let border = if active {
        Color::Cyan
    } else if app.error.is_some() {
        Color::Red
    } else if let Some(r) = &app.response {
        status_color(r.status)
    } else {
        Color::DarkGray
    };

    let mut title = vec![Span::raw(" RESPONSE ")];
    if let Some(r) = &app.response {
        title.push(Span::styled(
            format!(" {} {} ", r.status, status_reason(r.status)),
            Style::default()
                .fg(Color::Black)
                .bg(status_color(r.status))
                .add_modifier(Modifier::BOLD),
        ));
        title.push(Span::raw(format!(" {} ", fmt_bytes(r.size_bytes))));
        if let Some(ct) = &r.content_type {
            title.push(Span::styled(
                format!("{} ", short_content_type(ct)),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if r.is_binary {
            title.push(Span::styled(
                " BINARY ",
                Style::default().fg(Color::Black).bg(Color::Magenta),
            ));
        }
        if r.truncated {
            title.push(Span::styled(
                " TRUNCATED ",
                Style::default().fg(Color::Black).bg(Color::Yellow),
            ));
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(title))
        .border_style(Style::default().fg(border));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);

    let tabs = Tabs::new(ResponseTab::ALL.iter().map(|t| t.title()).collect::<Vec<_>>())
        .select(
            ResponseTab::ALL
                .iter()
                .position(|t| *t == app.response_tab)
                .unwrap_or(0),
        )
        .divider("│")
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, rows[0]);

    let view = rows[1];
    if app.is_loading {
        let frame = SPINNER[app.spinner % SPINNER.len()];
        let elapsed = app
            .request_started
            .map(|s| fmt_duration(s.elapsed()))
            .unwrap_or_default();
        f.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("{frame}  TRANSMITTING…  {elapsed}"),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Esc to abort",
                    Style::default().fg(Color::DarkGray),
                )),
            ])
            .alignment(Alignment::Center),
            view,
        );
        return;
    }

    if app.response.is_none() && app.error.is_none() {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "Ready for launch",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    "Ctrl+S to send · ? for keys",
                    Style::default().fg(Color::DarkGray),
                )),
            ])
            .alignment(Alignment::Center),
            view,
        );
        return;
    }

    render_viewer(f, app, view);
}

/// Renders only the rows that are on screen. Cost is independent of payload
/// size, which is what keeps a 32 MB response scrolling at full speed.
fn render_viewer(f: &mut Frame, app: &mut App, area: Rect) {
    let show_scrollbar = area.width > 2;
    let text_width = if show_scrollbar {
        area.width.saturating_sub(1)
    } else {
        area.width
    };
    let text_area = Rect {
        width: text_width,
        ..area
    };

    let wrap = app.settings.wrap_response;
    app.viewport_rows = area.height as usize;
    app.viewer.ensure_layout(text_width, wrap);

    let max_scroll = app.viewer.max_scroll(area.height as usize);
    if app.viewer.scroll > max_scroll {
        app.viewer.scroll = max_scroll;
    }
    if wrap {
        app.viewer.h_scroll = 0;
    } else {
        // Without a clamp, `l` would happily scroll past the end of every line
        // and leave the pilot staring at a blank pane.
        let max_h = app
            .viewer
            .max_row_width()
            .saturating_sub(text_width as usize);
        app.viewer.h_scroll = app.viewer.h_scroll.min(max_h);
    }

    let highlight = app
        .response
        .as_ref()
        .is_some_and(|r| r.highlight_json(app.response_tab));
    let error_style = app.error.is_some();

    let start = app.viewer.scroll;
    let end = (start + area.height as usize).min(app.viewer.total_rows());
    let h_scroll = app.viewer.h_scroll;
    let current_offset = app
        .viewer
        .matches
        .get(app.viewer.current_match)
        .map(|&(start, _)| start)
        .unwrap_or(usize::MAX);

    let mut lines: Vec<Line> = Vec::with_capacity(end.saturating_sub(start));
    for i in start..end {
        let Some(row) = app.viewer.row(i) else { break };
        let row_start = app.viewer.row_start(i);
        lines.push(build_line(
            row,
            row_start,
            &app.viewer.matches,
            current_offset,
            highlight,
            error_style,
            h_scroll,
            text_width as usize,
        ));
    }

    f.render_widget(Paragraph::new(lines), text_area);

    if show_scrollbar && app.viewer.total_rows() > area.height as usize {
        let mut state = ScrollbarState::new(app.viewer.total_rows().saturating_sub(1))
            .position(app.viewer.scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("│"))
                .thumb_symbol("█")
                .style(Style::default().fg(Color::DarkGray)),
            area,
            &mut state,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn build_line<'a>(
    row: &'a str,
    row_start: usize,
    matches: &[(usize, usize)],
    current_offset: usize,
    highlight_json: bool,
    error_style: bool,
    h_scroll: usize,
    width: usize,
) -> Line<'a> {
    let base = if error_style {
        Style::default().fg(Color::Red)
    } else {
        Style::default()
    };

    // Clip to the visible columns before doing any work: a minified payload can
    // put megabytes on one row, and none of it off-screen deserves styling.
    let (vis, vis_offset) = column_slice_at(row, h_scroll, width);
    if vis.is_empty() {
        return Line::from(Span::raw(""));
    }
    let vis_start = row_start + vis_offset;
    let vis_end = vis_start + vis.len();

    // Per-byte styling over one visible row is cheap and makes overlapping
    // syntax and search highlights trivial to compose.
    let mut styles = vec![base; vis.len()];

    if highlight_json {
        for (a, b, kind) in syntax::tokenize_json_line(vis) {
            let style = match kind {
                Token::Key => Style::default().fg(Color::Cyan),
                Token::Str => Style::default().fg(Color::Green),
                Token::Number => Style::default().fg(Color::Yellow),
                Token::Bool => Style::default().fg(Color::Magenta),
                Token::Null => Style::default().fg(Color::DarkGray),
                Token::Punct => Style::default().fg(Color::DarkGray),
                Token::Plain => base,
            };
            for s in styles.iter_mut().take(b.min(vis.len())).skip(a) {
                *s = style;
            }
        }
    }

    if !matches.is_empty() {
        // Hits are sorted by start, and their ends are non-decreasing, so a
        // binary search lands on the first hit that can still touch this row.
        let lo = matches.partition_point(|&(_, end)| end <= vis_start);
        for &(m_start, m_end) in &matches[lo..] {
            if m_start >= vis_end {
                break;
            }
            let style = if m_start == current_offset {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightYellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Black).bg(Color::Yellow)
            };
            // A wrapped or horizontally scrolled row can cut a hit in half, so
            // both ends are clamped into the visible slice before indexing.
            let from = m_start.saturating_sub(vis_start).min(vis.len());
            let to = m_end.min(vis_end).saturating_sub(vis_start).min(vis.len());
            for s in styles.iter_mut().take(to).skip(from) {
                *s = style;
            }
        }
    }

    let mut spans: Vec<Span> = Vec::new();
    let mut run_start = 0usize;
    let mut idx = 0usize;
    while idx < vis.len() {
        let mut next = idx + 1;
        while next < vis.len() && !vis.is_char_boundary(next) {
            next += 1;
        }
        if next >= vis.len() || styles[next] != styles[run_start] {
            spans.push(Span::styled(&vis[run_start..next], styles[run_start]));
            run_start = next;
        }
        idx = next;
    }

    Line::from(spans)
}

// ------------------------------------------------------------- telemetry

fn render_telemetry(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" TELEMETRY ")
        .border_style(Style::default().fg(Color::DarkGray));

    let Some(tele) = &app.telemetry else {
        let hint = if app.is_loading {
            "measuring…"
        } else {
            "No flight data yet — send a request to see the latency waterfall."
        };
        f.render_widget(
            Paragraph::new(Span::styled(hint, Style::default().fg(Color::DarkGray)))
                .block(block)
                .alignment(Alignment::Center),
            area,
        );
        return;
    };

    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    // The bar is whatever is left after the label and the timing column.
    let bar_width = (inner.width as usize).saturating_sub(24).clamp(4, 60);
    let mut lines = Vec::with_capacity(6);

    let mut summary = vec![
        Span::styled(
            format!(" {} ", tele.status),
            Style::default()
                .fg(Color::Black)
                .bg(status_color(tele.status))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {}", status_reason(tele.status))),
        Span::styled("  ·  ", Style::default().fg(Color::DarkGray)),
        Span::raw(tele.http_version.clone()),
        Span::styled("  ·  ", Style::default().fg(Color::DarkGray)),
        Span::raw(fmt_bytes(tele.size_bytes)),
    ];
    if let Some(addr) = &tele.remote_addr {
        summary.push(Span::styled("  ·  ", Style::default().fg(Color::DarkGray)));
        summary.push(Span::styled(
            addr.clone(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if tele.reused_connection {
        summary.push(Span::styled("  ·  ", Style::default().fg(Color::DarkGray)));
        summary.push(Span::styled(
            "keep-alive",
            Style::default().fg(Color::Green),
        ));
    }
    lines.push(Line::from(summary));

    let colors = [Color::Blue, Color::Magenta, Color::Cyan, Color::Green];
    for (phase, color) in tele.phases().into_iter().zip(colors) {
        match phase.value {
            Some(d) => lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<9}", phase.label),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(format!("{:>9}  ", fmt_duration(d))),
                Span::styled(
                    tele.render_bar(d, tele.total, bar_width),
                    Style::default().fg(color),
                ),
            ])),
            None => lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<9}", phase.label),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{:>9}  ", "—"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(phase.note, Style::default().fg(Color::DarkGray)),
            ])),
        }
    }

    let mut total = vec![
        Span::styled("TOTAL    ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(
            format!("{:>9}", fmt_duration(tele.total)),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(rate) = fmt_throughput(tele.size_bytes, tele.transfer) {
        total.push(Span::styled("  ·  ", Style::default().fg(Color::DarkGray)));
        total.push(Span::styled(rate, Style::default().fg(Color::DarkGray)));
    }
    lines.push(Line::from(total));

    f.render_widget(Paragraph::new(lines), inner);
}

// ------------------------------------------------------------ status bar

fn render_status_bar(f: &mut Frame, app: &App, area: Rect) {
    if let Some(toast) = &app.toast {
        let (fg, bg) = match toast.kind {
            ToastKind::Info => (Color::Black, Color::Cyan),
            ToastKind::Success => (Color::Black, Color::Green),
            ToastKind::Warn => (Color::Black, Color::Yellow),
            ToastKind::Error => (Color::White, Color::Red),
        };
        f.render_widget(
            Paragraph::new(format!(" {} ", toast.text)).style(Style::default().fg(fg).bg(bg)),
            area,
        );
        return;
    }

    if let Some(warning) = app.warnings.first() {
        let more = if app.warnings.len() > 1 {
            format!(" (+{} more)", app.warnings.len() - 1)
        } else {
            String::new()
        };
        f.render_widget(
            Paragraph::new(format!(" ⚠ {warning}{more} "))
                .style(Style::default().fg(Color::Black).bg(Color::Yellow)),
            area,
        );
        return;
    }

    let hints = match app.active_pane {
        ActivePane::MethodSelector => "←/→ or Space change method · Tab focus · Ctrl+S send · ? keys",
        ActivePane::UrlBar => "Type the endpoint · Tab focus · Ctrl+S send · Ctrl+R history · ? keys",
        ActivePane::InputArea => {
            "Alt+1/Alt+2 tabs · Ctrl+W del word · Ctrl+U undo · Ctrl+S send · F1 keys"
        }
        ActivePane::ResponseViewer => {
            "j/k scroll · ←/→ tabs · / search · w wrap · y copy · s save · ? keys"
        }
    };
    f.render_widget(
        Paragraph::new(format!(" {hints} "))
            .style(Style::default().fg(Color::Black).bg(Color::Gray)),
        area,
    );
}

// --------------------------------------------------------------- overlays

fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(v[1])[1]
}

const HELP_ROWS: &[(&str, &str)] = &[
    ("MISSION", ""),
    ("Ctrl+S · F5", "Send the request"),
    ("Esc", "Abort the request in flight / close this overlay"),
    ("Ctrl+Q", "Quit  (q also quits outside text fields)"),
    ("", ""),
    ("NAVIGATION", ""),
    ("Tab / Shift+Tab", "Move focus forward / backward"),
    ("1 / 2", "Request body / headers  (Alt+1, Alt+2 while typing)"),
    ("← / → · t", "Cycle response tabs: body / headers / raw"),
    ("←/→ · Space", "Change HTTP method  (method pane)"),
    ("", ""),
    ("RESPONSE", ""),
    ("j k ↑ ↓", "Scroll one line"),
    ("PgUp PgDn", "Scroll one screen"),
    ("Ctrl+U Ctrl+D", "Scroll half a screen"),
    ("g / G", "Jump to top / bottom"),
    ("h / l", "Scroll sideways (when wrapping is off)"),
    ("/ · n · N", "Search · next hit · previous hit"),
    ("w · Ctrl+W", "Toggle line wrapping"),
    ("y · Ctrl+Y · F6", "Copy the current view to the clipboard"),
    ("s · F7", "Save the payload to a file, byte for byte"),
    ("Ctrl+L", "Clear the response"),
    ("", ""),
    ("REQUESTS", ""),
    ("Ctrl+R · F4", "History of sent requests"),
    ("", ""),
    ("WHILE TYPING", ""),
    ("Ctrl+W Ctrl+U", "Delete word / undo — the editor keeps its own keys"),
    ("Alt+1 Alt+2", "Switch request tab without leaving the editor"),
    ("F2–F6", "Every overlay and copy, reachable from any pane"),
    ("F2", "Save the current request"),
    ("F3", "Open a saved request"),
];

fn render_help(f: &mut Frame, area: Rect) {
    let popup = centered(area, 72, 86);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" FLIGHT MANUAL — Esc to close ")
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let lines: Vec<Line> = HELP_ROWS
        .iter()
        .map(|(key, desc)| {
            if desc.is_empty() {
                Line::from(Span::styled(
                    *key,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(vec![
                    Span::styled(
                        format!("  {key:<16}"),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::raw(*desc),
                ])
            }
        })
        .collect();

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_list_overlay(
    f: &mut Frame,
    area: Rect,
    title: &str,
    footer: &str,
    empty: &str,
    items: Vec<Line<'_>>,
    selected: usize,
) {
    let popup = centered(area, 76, 70);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_string())
        .title_bottom(Line::from(Span::styled(
            footer.to_string(),
            Style::default().fg(Color::DarkGray),
        )))
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if items.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(empty, Style::default().fg(Color::DarkGray)))
                .alignment(Alignment::Center),
            inner,
        );
        return;
    }

    let list = List::new(
        items
            .into_iter()
            .map(ListItem::new)
            .collect::<Vec<_>>(),
    )
    .highlight_style(
        Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ");

    let mut state = ListState::default();
    state.select(Some(selected));
    f.render_stateful_widget(list, inner, &mut state);
}

fn render_history(f: &mut Frame, app: &App, area: Rect) {
    let now = crate::store::now_epoch();
    let items: Vec<Line> = app
        .history
        .iter()
        .map(|e| {
            Line::from(vec![
                Span::styled(
                    format!("{:<7}", e.request.method),
                    Style::default()
                        .fg(method_color(e.request.method_enum()))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:<4}", e.status),
                    Style::default().fg(status_color(e.status)),
                ),
                Span::raw(format!("{:<48}", truncate(&e.request.url, 48))),
                Span::styled(
                    format!("{:>10}  {}", format!("{}ms", e.duration_ms), relative_time(e.at, now)),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();

    render_list_overlay(
        f,
        area,
        " HISTORY ",
        " Enter load · Ctrl+D clear all · Esc close ",
        "No requests yet.",
        items,
        app.list_index,
    );
}

fn render_collection(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<Line> = app
        .collection
        .iter()
        .map(|s| {
            Line::from(vec![
                Span::styled(
                    format!("{:<24}", truncate(&s.name, 24)),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:<7}", s.request.method),
                    Style::default().fg(method_color(s.request.method_enum())),
                ),
                Span::styled(
                    truncate(&s.request.url, 44),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();

    render_list_overlay(
        f,
        area,
        " SAVED REQUESTS ",
        " Enter load · d delete · Esc close ",
        "Nothing saved yet — press F2 to save the current request.",
        items,
        app.list_index,
    );
}

fn render_prompt(f: &mut Frame, area: Rect, title: &str, hint: &str, editor: &tui_textarea::TextArea) {
    let popup = centered(area, 60, 18);
    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_string())
        .title_bottom(Line::from(Span::styled(
            hint.to_string(),
            Style::default().fg(Color::DarkGray),
        )))
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    if inner.height > 0 {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(inner);
        f.render_widget(editor, rows[0]);
    }
}

fn render_save_prompt(f: &mut Frame, app: &App, area: Rect) {
    render_prompt(
        f,
        area,
        " SAVE REQUEST ",
        " Enter save · Esc cancel ",
        &app.name_editor,
    );
}

fn render_search_prompt(f: &mut Frame, app: &App, area: Rect) {
    let hint = if app.viewer.matches.is_empty() {
        " Enter search · Esc cancel ".to_string()
    } else {
        format!(
            " {} of {} hits · Enter jump · Esc cancel ",
            app.viewer.current_match + 1,
            app.viewer.matches.len()
        )
    };
    render_prompt(f, area, " SEARCH RESPONSE ", &hint, &app.search_editor);
}

/// "application/json; charset=utf-8" -> "json"; keeps the title readable.
fn short_content_type(ct: &str) -> &str {
    let base = ct.split(';').next().unwrap_or(ct).trim();
    match base.rsplit(['/', '+']).next() {
        Some(sub) if !sub.is_empty() => sub,
        _ => base,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::engine::EngineConfig;
    use crate::store::Settings;
    use ratatui::backend::TestBackend;

    fn test_app() -> App<'static> {
        App::new(Settings::default(), EngineConfig::default())
    }

    /// Draws the whole cockpit into an off-screen buffer and returns its text.
    fn draw(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| ui(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_at_every_plausible_terminal_size() {
        // Layout arithmetic underflowing on a small terminal is a crash, and a
        // crash in raw mode leaves the user with a broken shell.
        let mut app = test_app();
        for (w, h) in [
            (1, 1), (10, 5), (39, 11), (40, 12), (60, 20), (80, 24),
            (120, 40), (200, 60), (300, 100),
        ] {
            let out = draw(&mut app, w, h);
            assert_eq!(out.lines().count(), h as usize);
        }
    }

    #[test]
    fn tiny_terminals_get_an_explanation_not_a_broken_layout() {
        let mut app = test_app();
        assert!(draw(&mut app, 30, 10).contains("Resize"));
        assert!(!draw(&mut app, 80, 24).contains("Resize"));
    }

    #[test]
    fn every_overlay_renders() {
        for overlay in [
            Overlay::Help,
            Overlay::History,
            Overlay::Collection,
            Overlay::SavePrompt,
            Overlay::Search,
        ] {
            let mut app = test_app();
            app.open_overlay(overlay);
            // Small and large, since popups are sized as a percentage.
            draw(&mut app, 42, 13);
            draw(&mut app, 160, 50);
        }
    }

    #[test]
    fn a_huge_payload_renders_only_the_visible_rows() {
        let mut app = test_app();
        let huge = "x".repeat(2_000_000);
        app.viewer.set_text(huge);
        app.error = Some(String::new());
        let start = std::time::Instant::now();
        for _ in 0..20 {
            draw(&mut app, 120, 40);
        }
        // Re-wrapping 2 MB per frame would take orders of magnitude longer;
        // this is a generous bound that still catches a regression.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(3),
            "20 frames took {:?} — rendering is no longer viewport-bounded",
            start.elapsed()
        );
    }

    #[test]
    fn a_minified_payload_on_one_line_still_renders_fast() {
        // Wrap off + a single multi-megabyte line used to style every byte of
        // that line on every frame. Only the visible columns should cost.
        let mut app = test_app();
        app.settings.wrap_response = false;
        app.error = Some(String::new());
        let minified = format!("[{}]", "{\"k\":\"v\"},".repeat(200_000));
        app.viewer.set_text(minified);
        let start = std::time::Instant::now();
        for _ in 0..20 {
            draw(&mut app, 120, 40);
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(3),
            "20 frames took {:?} — rendering is no longer viewport-bounded",
            start.elapsed()
        );
    }

    #[test]
    fn scrolling_a_huge_buffer_does_not_get_slower_with_depth() {
        let mut app = test_app();
        app.error = Some(String::new());
        app.viewer.set_text("line of text\n".repeat(300_000));
        draw(&mut app, 120, 40);

        let top = std::time::Instant::now();
        for _ in 0..10 {
            draw(&mut app, 120, 40);
        }
        let at_top = top.elapsed();

        app.viewer.scroll_to_bottom(app.viewport_rows);
        let bottom = std::time::Instant::now();
        for _ in 0..10 {
            draw(&mut app, 120, 40);
        }
        let at_bottom = bottom.elapsed();

        assert!(
            at_bottom < at_top * 4 + std::time::Duration::from_millis(50),
            "rendering at the end of the buffer ({at_bottom:?}) must not cost \
             materially more than at the start ({at_top:?})"
        );
    }

    #[test]
    fn the_status_line_and_method_are_visible() {
        let mut app = test_app();
        app.method = crate::engine::HttpMethod::Delete;
        let out = draw(&mut app, 100, 30);
        assert!(out.contains("DELETE"));
        assert!(out.contains("TELEMETRY"));
        assert!(out.contains("RESPONSE"));
    }

    #[test]
    fn insecure_mode_is_impossible_to_miss() {
        let mut app = test_app();
        app.engine_config.insecure = true;
        assert!(draw(&mut app, 100, 30).contains("TLS VERIFY OFF"));
    }

    #[test]
    fn the_telemetry_panel_folds_away_on_short_terminals() {
        let mut app = test_app();
        assert!(!draw(&mut app, 100, 18).contains("TELEMETRY"));
        assert!(draw(&mut app, 100, 30).contains("TELEMETRY"));
    }

    #[test]
    fn truncation_is_char_safe() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 8), "hello w…");
        assert_eq!(truncate("日本語のテキスト", 4), "日本語…");
    }

    #[test]
    fn content_types_shorten_to_their_subtype() {
        assert_eq!(short_content_type("application/json; charset=utf-8"), "json");
        assert_eq!(short_content_type("text/html"), "html");
        assert_eq!(short_content_type("application/vnd.api+json"), "json");
        assert_eq!(short_content_type("image/png"), "png");
    }

    #[test]
    fn status_colors_cover_every_class() {
        assert_eq!(status_color(204), Color::Green);
        assert_eq!(status_color(301), Color::Blue);
        assert_eq!(status_color(404), Color::Yellow);
        assert_eq!(status_color(503), Color::Red);
        assert_eq!(status_color(0), Color::DarkGray);
    }

    #[test]
    fn horizontal_scrolling_keeps_syntax_and_search_highlighting() {
        let row = r#"{"alpha": "beta", "gamma": 1}"#;
        let scrolled = build_line(row, 0, &[(11, 15)], 11, true, false, 10, 12);
        let rebuilt: String = scrolled.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(rebuilt, &row[10..22]);
        assert!(
            scrolled.spans.len() > 1,
            "the scrolled window must still be styled, not flattened"
        );
    }

    #[test]
    fn scrolling_past_the_end_of_a_row_renders_nothing() {
        let line = build_line("short", 0, &[], usize::MAX, false, false, 99, 20);
        let rebuilt: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(rebuilt, "");
    }

    #[test]
    fn a_hit_starting_on_an_earlier_wrapped_row_does_not_underflow() {
        // Regression: the highlight used to subtract the row offset from a
        // match that began before the row, panicking on the way.
        let row = "second half";
        let line = build_line(row, 20, &[(10, 24)], 10, false, false, 0, 80);
        let rebuilt: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(rebuilt, row);
    }

    #[test]
    fn a_hit_entirely_before_the_row_is_ignored() {
        let row = "later text";
        let line = build_line(row, 100, &[(0, 5)], 0, false, false, 0, 80);
        let rebuilt: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(rebuilt, row);
    }

    #[test]
    fn spans_reassemble_into_the_original_row() {
        let row = r#"  "key": "valué", 12"#;
        let line = build_line(row, 0, &[], usize::MAX, true, false, 0, 80);
        let rebuilt: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(rebuilt, row, "highlighting must not drop or duplicate bytes");
    }

    #[test]
    fn empty_row_renders_without_panicking() {
        let line = build_line("", 0, &[], usize::MAX, true, false, 0, 80);
        assert_eq!(line.spans.len(), 1);
    }

    #[test]
    fn search_highlight_does_not_corrupt_the_row() {
        let row = "find me here";
        let line = build_line(row, 0, &[(5, 7)], 5, false, false, 0, 80);
        let rebuilt: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(rebuilt, row);
    }
}
