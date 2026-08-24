use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use hcp::app::{ActivePane, App, InputTab, ToastKind};
use hcp::engine::{EngineConfig, MissionResult, NetworkEngine};
use hcp::input::{self, Action};
use hcp::{cli, clipboard, store, telemetry, ui};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::{self, Write};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// A finished flight, tagged with the request that produced it so a reply from
/// a cancelled or superseded request can be discarded instead of displayed.
struct EngineEvent {
    id: u64,
    result: Result<MissionResult>,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = match cli::parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("hcp: {e}");
            std::process::exit(2);
        }
    };
    let options = match parsed {
        cli::Action::PrintAndExit(text) => {
            println!("{text}");
            return Ok(());
        }
        cli::Action::Run(cli) => cli,
    };

    // The runtime is built by hand so `--help` never pays for spawning threads.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")?;

    runtime.block_on(run(*options))
}

/// Restores the terminal no matter how the app exits — clean quit, `?` on an
/// I/O error, or a panic. Leaving a user in raw mode is unforgivable.
struct TerminalGuard {
    mouse: bool,
}

impl TerminalGuard {
    fn enter(mouse: bool) -> Result<Self> {
        enable_raw_mode().context("could not switch the terminal to raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
        if mouse {
            execute!(stdout, EnableMouseCapture)?;
        }
        Ok(Self { mouse })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        if self.mouse {
            let _ = execute!(stdout, DisableMouseCapture);
        }
        let _ = execute!(stdout, DisableBracketedPaste, LeaveAlternateScreen);
        let _ = disable_raw_mode();
        let _ = stdout.flush();
    }
}

fn install_panic_hook(mouse: bool) {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut stdout = io::stdout();
        if mouse {
            let _ = execute!(stdout, DisableMouseCapture);
        }
        let _ = execute!(stdout, DisableBracketedPaste, LeaveAlternateScreen);
        let _ = disable_raw_mode();
        let _ = stdout.flush();
        original(info);
    }));
}

async fn run(options: cli::Cli) -> Result<()> {
    let on_disk = store::load_settings();
    let mut settings = on_disk.clone();
    // Command-line flags win for this session only; `on_disk` is what gets
    // written back if a preference is changed inside the cockpit.
    if let Some(t) = options.timeout {
        settings.timeout_secs = t;
    }
    if let Some(t) = options.connect_timeout {
        settings.connect_timeout_secs = t;
    }
    if let Some(mb) = options.max_body_mb {
        settings.max_body_mb = mb;
    }
    if options.insecure {
        settings.insecure = true;
    }
    if options.no_redirects {
        settings.follow_redirects = false;
    }
    if options.no_wrap {
        settings.wrap_response = false;
    }
    if options.no_pretty {
        settings.pretty_json = false;
    }
    let mouse = settings.mouse && !options.no_mouse;

    let engine_config = EngineConfig {
        timeout: Duration::from_secs(settings.timeout_secs.max(1)),
        connect_timeout: Duration::from_secs(settings.connect_timeout_secs.max(1)),
        insecure: settings.insecure,
        follow_redirects: settings.follow_redirects,
        max_redirects: 10,
        max_body_bytes: settings.max_body_mb.max(1) * 1024 * 1024,
    };

    let engine = NetworkEngine::new(engine_config.clone())?;
    let mut app = App::with_persisted(settings, on_disk, engine.config().clone());

    if let Some(url) = &options.url {
        app.set_url(url);
        app.active_pane = ActivePane::ResponseViewer;
    }
    if let Some(method) = options.method {
        app.method = method;
    }
    if !options.headers.is_empty() {
        let joined = options.headers.join("\n");
        app.load_snapshot(&store::RequestSnapshot {
            method: app.method.as_str().to_string(),
            url: app.url(),
            headers: joined,
            body: options.body.clone().unwrap_or_default(),
        });
    } else if let Some(body) = &options.body {
        let snap = store::RequestSnapshot {
            method: app.method.as_str().to_string(),
            url: app.url(),
            headers: app.headers(),
            body: body.clone(),
        };
        app.load_snapshot(&snap);
    }

    install_panic_hook(mouse);
    let _guard = TerminalGuard::enter(mouse)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .context("could not initialise the terminal backend")?;
    terminal.clear()?;

    let (tx, mut rx) = mpsc::channel::<EngineEvent>(8);
    let (input_tx, mut input_rx) = mpsc::channel::<Event>(64);

    // Terminal reads block, so they live on their own OS thread; the async
    // runtime is then free to service the network without ever stalling input.
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if input_tx.blocking_send(ev).is_err() {
                break; // The cockpit has shut down.
            }
        }
    });

    let mut inflight: Option<JoinHandle<()>> = None;
    let mut ticker = tokio::time::interval(Duration::from_millis(110));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut needs_redraw = true;

    if options.send_now {
        launch(&mut app, &engine, &tx, &mut inflight);
    }

    while !app.should_quit {
        if needs_redraw {
            terminal.draw(|f| ui::ui(f, &mut app))?;
            needs_redraw = false;
        }

        tokio::select! {
            _ = ticker.tick() => {
                let animating = app.is_loading || app.toast.is_some();
                app.tick();
                if animating {
                    needs_redraw = true;
                }
            }

            Some(event) = rx.recv() => {
                needs_redraw = true;
                if event.id != app.request_id {
                    continue; // Reply from a cancelled or superseded flight.
                }
                inflight = None;
                match event.result {
                    Ok(result) => app.finish_success(result),
                    Err(e) => app.finish_error(format!("{e:#}")),
                }
            }

            // Matching the Option rather than `Some(..)` matters: if the
            // terminal goes away the channel closes, and a `Some(..)` pattern
            // would silently disable this branch and spin forever.
            maybe_event = input_rx.recv() => {
                let Some(event) = maybe_event else {
                    app.should_quit = true;
                    continue;
                };
                needs_redraw = true;
                match event {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        match input::handle_key(&mut app, key) {
                            Action::None => {}
                            Action::Quit => app.should_quit = true,
                            Action::Send => launch(&mut app, &engine, &tx, &mut inflight),
                            Action::Cancel => {
                                if let Some(handle) = inflight.take() {
                                    handle.abort();
                                }
                                app.cancel_request();
                            }
                            Action::Copy => copy_view(&mut app),
                            Action::SaveResponse => save_response(&mut app),
                        }
                    }
                    Event::Mouse(m) => input::handle_mouse(&mut app, m),
                    Event::Paste(text) => input::handle_paste(&mut app, text),
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }
        }
    }

    if let Some(handle) = inflight.take() {
        handle.abort();
    }
    Ok(())
}

fn launch(
    app: &mut App,
    engine: &NetworkEngine,
    tx: &mpsc::Sender<EngineEvent>,
    inflight: &mut Option<JoinHandle<()>>,
) {
    if app.url().trim().is_empty() {
        app.toast("Set a URL before launching", ToastKind::Warn);
        app.active_pane = ActivePane::UrlBar;
        return;
    }
    let body = match app.prepare_body() {
        Ok(b) => b,
        Err(message) => {
            app.reject_preflight(message);
            app.input_tab = InputTab::Body;
            app.active_pane = ActivePane::InputArea;
            return;
        }
    };

    // A second launch supersedes the first rather than racing it.
    if let Some(handle) = inflight.take() {
        handle.abort();
    }

    let id = app.begin_request();
    let method = app.method;
    let url = app.url();
    let headers = app.headers();
    let engine = engine.clone();
    let tx = tx.clone();

    *inflight = Some(tokio::spawn(async move {
        let result = engine
            .execute_mission(method, &url, body, &headers)
            .await;
        let _ = tx.send(EngineEvent { id, result }).await;
    }));
}

/// Hands the current view to the terminal's clipboard.
fn copy_view(app: &mut App) {
    let Some(text) = app.copy_target() else {
        app.toast("Nothing to copy yet", ToastKind::Warn);
        return;
    };
    match clipboard::copy(&text) {
        Ok(outcome) if outcome.truncated => app.toast(
            format!(
                "Copied first {} (clipboard limit)",
                telemetry::fmt_bytes(outcome.copied_bytes as u64)
            ),
            ToastKind::Warn,
        ),
        Ok(outcome) => app.toast(
            format!("Copied {}", telemetry::fmt_bytes(outcome.copied_bytes as u64)),
            ToastKind::Success,
        ),
        Err(e) => app.toast(format!("Clipboard failed: {e}"), ToastKind::Error),
    }
}

/// Writes the received payload to a file next to the working directory.
fn save_response(app: &mut App) {
    match app.save_response_to_file() {
        Ok(path) => app.toast(format!("Saved to {}", path.display()), ToastKind::Success),
        Err(e) => app.toast(e, ToastKind::Warn),
    }
}
