use crate::app::{ActivePane, App, HttpMethod, InputTab};
use ratatui::{prelude::*, widgets::*};

pub fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(8),
            Constraint::Length(1),
        ])
        .split(f.area());

    let top_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(12), Constraint::Min(1)])
        .split(chunks[0]);

    let method_color = match app.method {
        HttpMethod::GET => Color::Green,
        HttpMethod::POST => Color::Yellow,
        HttpMethod::PUT => Color::Blue,
        HttpMethod::DELETE => Color::Red,
    };
    let method_style = if app.active_pane == ActivePane::MethodSelector {
        Style::default()
            .fg(Color::Black)
            .bg(method_color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(method_color)
    };

    f.render_widget(
        Paragraph::new(format!("{:?}", app.method))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" METHOD ")
                    .border_style(if app.active_pane == ActivePane::MethodSelector {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default()
                    }),
            )
            .style(method_style),
        top_chunks[0],
    );

    let url_style = if app.active_pane == ActivePane::UrlBar {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::White)
    };
    f.render_widget(
        Paragraph::new(app.url_input.as_str()).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" ENDPOINT URL ")
                .border_style(url_style),
        ),
        top_chunks[1],
    );

    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(chunks[1]);

    let input_area = main_chunks[0];

    let tabs = Tabs::new(vec![" [1] BODY ", " [2] HEADERS "])
        .block(Block::default().borders(Borders::BOTTOM))
        .select(match app.input_tab {
            InputTab::Body => 0,
            InputTab::Headers => 1,
        })
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );

    let input_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(input_area);

    f.render_widget(tabs, input_chunks[0]);

    let border_style = if app.active_pane == ActivePane::InputArea {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    match app.input_tab {
        InputTab::Body => {
            app.body_editor.set_block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style),
            );
            f.render_widget(&app.body_editor, input_chunks[1]);
        }
        InputTab::Headers => {
            app.headers_editor.set_block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style),
            );
            f.render_widget(&app.headers_editor, input_chunks[1]);
        }
    }

    let resp_border = if app.active_pane == ActivePane::ResponseViewer {
        Color::Cyan
    } else if app.response_status >= 200 && app.response_status < 300 {
        Color::Green
    } else if app.response_status > 0 {
        Color::Red
    } else {
        Color::DarkGray
    };
    let resp_text = if app.is_loading {
        "🚀 TRANSMITTING...".to_string()
    } else {
        app.response_text.clone()
    };
    f.render_widget(
        Paragraph::new(resp_text)
            .wrap(Wrap { trim: false })
            .scroll((app.response_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" RESPONSE ")
                    .border_style(Style::default().fg(resp_border)),
            ),
        main_chunks[1],
    );

    if let Some(tele) = &app.telemetry {
        let t_dns = tele.render_bar(tele.dns_handshake_ttfb, tele.total);
        let t_transfer = tele.render_bar(tele.transfer, tele.total);
        let status_color = if tele.status >= 200 && tele.status < 300 {
            Color::Green
        } else {
            Color::Red
        };

        let telemetry_text = vec![
            Line::from(vec![
                Span::raw("STATUS: "),
                Span::styled(
                    format!(" {} ", tele.status),
                    Style::default()
                        .bg(status_color)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  SIZE: {:.2} KB", tele.size_bytes as f64 / 1024.0)),
            ]),
            Line::from(vec![
                Span::raw(format!(
                    "{:<15} {:>6.0}ms ",
                    "LATENCY:",
                    tele.dns_handshake_ttfb.as_millis()
                )),
                Span::styled(t_dns, Style::default().fg(Color::Cyan)),
            ]),
            Line::from(vec![
                Span::raw(format!(
                    "{:<15} {:>6.0}ms ",
                    "TRANSFER:",
                    tele.transfer.as_millis()
                )),
                Span::styled(t_transfer, Style::default().fg(Color::Magenta)),
            ]),
            Line::from(vec![Span::styled(
                format!("TOTAL:       {:>6.0}ms", tele.total.as_millis()),
                Style::default().add_modifier(Modifier::BOLD),
            )]),
        ];
        f.render_widget(
            Paragraph::new(telemetry_text)
                .block(Block::default().borders(Borders::ALL).title(" TELEMETRY ")),
            chunks[2],
        );
    } else {
        f.render_widget(
            Block::default().borders(Borders::ALL).title(" TELEMETRY "),
            chunks[2],
        );
    }

    f.render_widget(
        Paragraph::new(
            " [TAB] Focus | [1/2] Switch Tab | [J/K] Scroll Resp | [CTRL+S] Send | [Q] Quit ",
        )
        .style(Style::default().bg(Color::DarkGray)),
        chunks[3],
    );
}
