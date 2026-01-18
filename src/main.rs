mod app;
mod engine;
mod telemetry;
mod ui;

use anyhow::Result;
use app::{ActivePane, App, HttpMethod, InputTab};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use engine::NetworkEngine;
use ratatui::{backend::CrosstermBackend, Terminal};
use serde_json::Value;
use std::io;
use telemetry::MissionTelemetry;
use tokio::sync::mpsc;

enum EngineEvent {
    Completed(MissionTelemetry, String),
    Error(String),
}

#[tokio::main]
async fn main() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (tx, mut rx) = mpsc::channel::<EngineEvent>(10);
    let engine = NetworkEngine::new()?;

    let (input_tx, mut input_rx) = mpsc::channel::<Event>(10);

    std::thread::spawn(move || loop {
        if let Ok(event) = event::read() {
            if input_tx.blocking_send(event).is_err() {
                break;
            }
        }
    });

    let mut app = App::new();

    loop {
        terminal.draw(|f| ui::ui(f, &mut app))?;

        tokio::select! {
            Some(event) = rx.recv() => {
                app.is_loading = false;
                match event {
                    EngineEvent::Completed(telemetry, body) => {
                        app.telemetry = Some(telemetry.clone());
                        app.response_status = telemetry.status;
                        app.response_text = body;
                    }
                    EngineEvent::Error(msg) => {
                        app.response_text = format!("❌ CRITICAL FAILURE: {}", msg);
                        app.response_status = 500;
                    }
                }
            }

            Some(event) = input_rx.recv() => {
                if let Event::Key(key) = event {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }

                    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
                        let url = app.url_input.clone();
                        let method = app.method.clone();
                        let raw_body = app.body_editor.lines().join("\n");
                        let raw_headers = app.headers_editor.lines().join("\n");

                        let body_to_send = if (method == HttpMethod::POST || method == HttpMethod::PUT) && !raw_body.trim().is_empty() {
                            match serde_json::from_str::<Value>(&raw_body) {
                                Ok(_) => Some(raw_body),
                                Err(e) => {
                                    app.response_text = format!("❌ JSON ERROR: {}", e);
                                    app.response_status = 400;
                                    continue;
                                }
                            }
                        } else {
                            None
                        };

                        app.is_loading = true;
                        app.response_text = String::new();

                        let tx_clone = tx.clone();
                        let engine_clone = engine.clone();

                        tokio::spawn(async move {
                            match engine_clone.execute_mission(method, &url, body_to_send, raw_headers).await {
                                Ok((tele, body)) => { let _ = tx_clone.send(EngineEvent::Completed(tele, body)).await; },
                                Err(e) => { let _ = tx_clone.send(EngineEvent::Error(e.to_string())).await; }
                            }
                        });
                        continue;
                    }

                    match key.code {
                        KeyCode::Tab => { app.cycle_focus(); continue; },
                        KeyCode::BackTab => { app.cycle_focus(); continue; },
                        KeyCode::Char('1') => {
                            app.input_tab = InputTab::Body;
                            continue;
                        },
                        KeyCode::Char('2') => {
                            app.input_tab = InputTab::Headers;
                            continue;
                        },
                        KeyCode::Esc => {
                             if !app.is_loading {
                                 break;
                             }
                        }
                         KeyCode::Char('q') if app.active_pane != ActivePane::InputArea => {
                            break;
                        },
                        _ => {}
                    }

                    match app.active_pane {
                        ActivePane::InputArea => {
                            match app.input_tab {
                                InputTab::Body => { app.body_editor.input(key); },
                                InputTab::Headers => { app.headers_editor.input(key); },
                            };
                        },
                        ActivePane::ResponseViewer => {
                             match key.code {
                                KeyCode::Char('j') | KeyCode::Down => app.scroll_response(1),
                                KeyCode::Char('k') | KeyCode::Up => app.scroll_response(-1),
                                KeyCode::PageDown => app.scroll_response(10),
                                KeyCode::PageUp => app.scroll_response(-10),
                                _ => {}
                            }
                        },
                        ActivePane::UrlBar => {
                             match key.code {
                                KeyCode::Backspace => { app.url_input.pop(); },
                                KeyCode::Char(c) => { app.url_input.push(c); },
                                _ => {}
                            }
                        },
                        ActivePane::MethodSelector => {
                             if key.code == KeyCode::Enter || key.code == KeyCode::Char(' ') {
                                    app.method = app.method.next();
                             }
                        },
                    }
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}
