//! Key, mouse and paste routing.
//!
//! Routing returns an [`Action`] instead of performing I/O, which keeps every
//! shortcut decision testable without a terminal or a network. This is where
//! the cockpit decides whether a character is a command or just text — the
//! distinction that makes typing `{"id": 1}` or a `?q=` URL possible.

use crate::app::{ActivePane, App, InputTab, Overlay, ResponseTab, ToastKind};
use crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

/// Work that only the shell around the state machine can carry out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Send,
    Cancel,
    Copy,
    SaveResponse,
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> Action {
    if app.overlay != Overlay::None {
        return handle_overlay_key(app, key);
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let typing = app.active_pane.is_text_input();

    // ---- shortcuts that work everywhere, typing or not ----
    // Only keys the text editor does not need may live here.
    if ctrl {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('c') => return Action::Quit,
            KeyCode::Char('s') => return Action::Send,
            KeyCode::Char('l') => {
                app.clear_response();
                return Action::None;
            }
            _ => {}
        }
    }

    // These would shadow the editor's own bindings — Ctrl+R redo, Ctrl+U undo,
    // Ctrl+W delete-word, Ctrl+Y paste, Ctrl+D delete-forward — so they are
    // only live when no text field has focus. The F-keys reach them always.
    if ctrl && !typing {
        match key.code {
            KeyCode::Char('r') => {
                app.open_overlay(Overlay::History);
                return Action::None;
            }
            KeyCode::Char('y') => return Action::Copy,
            KeyCode::Char('w') => {
                toggle_wrap(app);
                return Action::None;
            }
            KeyCode::Char('d') => {
                let rows = app.viewport_rows.max(1);
                app.viewer.scroll_by(rows as isize / 2, rows);
                return Action::None;
            }
            KeyCode::Char('u') => {
                let rows = app.viewport_rows.max(1);
                app.viewer.scroll_by(-(rows as isize) / 2, rows);
                return Action::None;
            }
            _ => {}
        }
    }

    // Alt+digit reaches the request tabs even while a text field has focus.
    if alt {
        match key.code {
            KeyCode::Char('1') => {
                app.input_tab = InputTab::Body;
                return Action::None;
            }
            KeyCode::Char('2') => {
                app.input_tab = InputTab::Headers;
                return Action::None;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::F(1) => {
            app.open_overlay(Overlay::Help);
            return Action::None;
        }
        KeyCode::F(2) => {
            app.open_overlay(Overlay::SavePrompt);
            return Action::None;
        }
        KeyCode::F(3) => {
            app.open_overlay(Overlay::Collection);
            return Action::None;
        }
        KeyCode::F(4) => {
            app.open_overlay(Overlay::History);
            return Action::None;
        }
        KeyCode::F(5) => return Action::Send,
        KeyCode::F(6) => return Action::Copy,
        KeyCode::F(7) => return Action::SaveResponse,
        KeyCode::Tab => {
            app.cycle_focus(true);
            return Action::None;
        }
        KeyCode::BackTab => {
            app.cycle_focus(false);
            return Action::None;
        }
        KeyCode::Esc => {
            return if app.is_loading {
                Action::Cancel
            } else {
                Action::None
            };
        }
        _ => {}
    }

    // ---- shortcuts that must never steal a character from a text field ----
    if !typing {
        match key.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('?') => {
                app.open_overlay(Overlay::Help);
                return Action::None;
            }
            KeyCode::Char('1') => {
                app.input_tab = InputTab::Body;
                app.active_pane = ActivePane::InputArea;
                return Action::None;
            }
            KeyCode::Char('2') => {
                app.input_tab = InputTab::Headers;
                app.active_pane = ActivePane::InputArea;
                return Action::None;
            }
            _ => {}
        }
    }

    match app.active_pane {
        ActivePane::MethodSelector => {
            match key.code {
                KeyCode::Right | KeyCode::Down | KeyCode::Char(' ') | KeyCode::Enter => {
                    app.method = app.method.next()
                }
                KeyCode::Left | KeyCode::Up => app.method = app.method.prev(),
                _ => {}
            }
            Action::None
        }

        ActivePane::UrlBar => match key.code {
            // A single-line field: Enter launches instead of inserting a break.
            KeyCode::Enter => Action::Send,
            _ => {
                if let Some(key) = for_single_line(key) {
                    app.url_editor.input(key);
                }
                Action::None
            }
        },

        ActivePane::InputArea => {
            let key = as_newline_if_line_feed(key);
            let editor = match app.input_tab {
                InputTab::Body => &mut app.body_editor,
                InputTab::Headers => &mut app.headers_editor,
            };
            editor.input(key);
            Action::None
        }

        ActivePane::ResponseViewer => handle_response_key(app, key),
    }
}

fn handle_response_key(app: &mut App, key: KeyEvent) -> Action {
    let rows = app.viewport_rows.max(1);
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => app.viewer.scroll_by(1, rows),
        KeyCode::Char('k') | KeyCode::Up => app.viewer.scroll_by(-1, rows),
        KeyCode::PageDown | KeyCode::Char(' ') => app.viewer.scroll_by(rows as isize, rows),
        KeyCode::PageUp => app.viewer.scroll_by(-(rows as isize), rows),
        KeyCode::Char('g') | KeyCode::Home => app.viewer.scroll_to_top(),
        KeyCode::Char('G') | KeyCode::End => app.viewer.scroll_to_bottom(rows),
        KeyCode::Char('h') => app.viewer.scroll_h(-4),
        KeyCode::Char('l') => app.viewer.scroll_h(4),
        KeyCode::Left => {
            let idx = ResponseTab::ALL
                .iter()
                .position(|t| *t == app.response_tab)
                .unwrap_or(0);
            let prev =
                ResponseTab::ALL[(idx + ResponseTab::ALL.len() - 1) % ResponseTab::ALL.len()];
            app.set_response_tab(prev);
        }
        KeyCode::Right | KeyCode::Char('t') => {
            let next = app.response_tab.next();
            app.set_response_tab(next);
        }
        KeyCode::Char('w') => toggle_wrap(app),
        KeyCode::Char('y') => return Action::Copy,
        KeyCode::Char('s') => return Action::SaveResponse,
        KeyCode::Char('/') => app.open_overlay(Overlay::Search),
        KeyCode::Char('n') => jump_match(app, true),
        KeyCode::Char('N') => jump_match(app, false),
        _ => {}
    }
    Action::None
}

fn handle_overlay_key(app: &mut App, key: KeyEvent) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    if ctrl && matches!(key.code, KeyCode::Char('q') | KeyCode::Char('c')) {
        return Action::Quit;
    }
    if key.code == KeyCode::Esc {
        app.close_overlay();
        return Action::None;
    }

    match app.overlay {
        // The manual is a reference, not a mode: any key dismisses it.
        Overlay::Help => app.close_overlay(),

        Overlay::History => match key.code {
            KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
            KeyCode::Enter => {
                if let Some(entry) = app.history.get(app.list_index) {
                    let snap = entry.request.clone();
                    app.load_snapshot(&snap);
                    app.close_overlay();
                    app.toast("Request loaded from history", ToastKind::Success);
                }
            }
            KeyCode::Char('d') if ctrl => app.clear_history(),
            KeyCode::Char('q') => app.close_overlay(),
            _ => {}
        },

        Overlay::Collection => match key.code {
            KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
            KeyCode::Enter => {
                if let Some(saved) = app.collection.get(app.list_index) {
                    let snap = saved.request.clone();
                    let name = saved.name.clone();
                    app.load_snapshot(&snap);
                    app.close_overlay();
                    app.toast(format!("Loaded '{name}'"), ToastKind::Success);
                }
            }
            KeyCode::Char('d') => app.delete_selected_saved(),
            KeyCode::Char('q') => app.close_overlay(),
            _ => {}
        },

        Overlay::SavePrompt => match key.code {
            KeyCode::Enter => {
                let name = app.name_editor.lines().join(" ");
                app.save_current_request(name);
                app.close_overlay();
            }
            KeyCode::Tab => {}
            _ => {
                if let Some(key) = for_single_line(key) {
                    app.name_editor.input(key);
                }
            }
        },

        Overlay::Search => match key.code {
            KeyCode::Enter => {
                let query = app.search_editor.lines().join("");
                app.viewer.set_query(query);
                let rows = app.viewport_rows;
                if let Some(row) = app.viewer.first_match_row() {
                    app.viewer.center_on(row, rows);
                    let msg = format!("{} matches", app.viewer.matches.len());
                    app.toast(msg, ToastKind::Success);
                } else if !app.viewer.query.is_empty() {
                    app.toast("No matches", ToastKind::Warn);
                }
                app.close_overlay();
                app.active_pane = ActivePane::ResponseViewer;
            }
            KeyCode::Tab => {}
            _ => {
                if let Some(key) = for_single_line(key) {
                    app.search_editor.input(key);
                }
                // Live feedback: the hit counter updates as you type.
                let query = app.search_editor.lines().join("");
                app.viewer.set_query(query);
            }
        },

        Overlay::None => {}
    }

    Action::None
}

/// A bare line feed (0x0A) arrives as Ctrl+J, which the editor treats as
/// "delete to start of line" — silently destroying a line whenever a terminal
/// delivers LF instead of CR. In a multi-line field it means what it says: a
/// new line.
fn as_newline_if_line_feed(key: KeyEvent) -> KeyEvent {
    if key.code == KeyCode::Char('j') && key.modifiers == KeyModifiers::CONTROL {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    } else {
        key
    }
}

/// Single-line fields drop anything that would insert a break.
fn for_single_line(key: KeyEvent) -> Option<KeyEvent> {
    if key.code == KeyCode::Char('j') && key.modifiers == KeyModifiers::CONTROL {
        return None;
    }
    Some(key)
}

pub fn handle_mouse(app: &mut App, m: MouseEvent) {
    if app.overlay != Overlay::None {
        return;
    }
    match m.kind {
        MouseEventKind::ScrollDown => {
            app.active_pane = ActivePane::ResponseViewer;
            app.viewer.scroll_by(3, app.viewport_rows);
        }
        MouseEventKind::ScrollUp => {
            app.active_pane = ActivePane::ResponseViewer;
            app.viewer.scroll_by(-3, app.viewport_rows);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(pane) = app.layout.pane_at(m.column, m.row) {
                app.active_pane = pane;
            }
        }
        _ => {}
    }
}

pub fn handle_paste(app: &mut App, text: String) {
    // Pasted newlines would silently corrupt single-line fields.
    let single_line = text.replace(['\n', '\r'], " ");
    match app.overlay {
        Overlay::SavePrompt => {
            app.name_editor.insert_str(single_line);
            return;
        }
        Overlay::Search => {
            app.search_editor.insert_str(single_line);
            let q = app.search_editor.lines().join("");
            app.viewer.set_query(q);
            return;
        }
        Overlay::None => {}
        _ => return,
    }

    match app.active_pane {
        ActivePane::UrlBar => {
            app.url_editor.insert_str(single_line.trim());
        }
        ActivePane::InputArea => match app.input_tab {
            InputTab::Body => {
                app.body_editor.insert_str(text);
            }
            InputTab::Headers => {
                app.headers_editor.insert_str(text);
            }
        },
        _ => {}
    }
}

pub fn jump_match(app: &mut App, forward: bool) {
    if app.viewer.matches.is_empty() {
        app.toast("No matches — press / to search", ToastKind::Info);
        return;
    }
    let rows = app.viewport_rows;
    if let Some(row) = app.viewer.jump_match(forward) {
        app.viewer.center_on(row, rows);
    }
    let msg = format!(
        "Match {} of {}",
        app.viewer.current_match + 1,
        app.viewer.matches.len()
    );
    app.toast(msg, ToastKind::Info);
}

pub fn toggle_wrap(app: &mut App) {
    app.set_wrap(!app.settings.wrap_response);
    let state = if app.settings.wrap_response { "on" } else { "off" };
    app.toast(format!("Line wrap {state}"), ToastKind::Info);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineConfig, HttpMethod};
    use crate::store::Settings;

    fn app() -> App<'static> {
        App::new(Settings::default(), EngineConfig::default())
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn code(k: KeyCode) -> KeyEvent {
        KeyEvent::new(k, KeyModifiers::NONE)
    }

    fn type_str(a: &mut App, text: &str) {
        for c in text.chars() {
            handle_key(a, key(c));
        }
    }

    // --- the bugs that made the original build unusable ------------------

    #[test]
    fn digits_are_typable_in_the_url_bar() {
        let mut a = app();
        a.active_pane = ActivePane::UrlBar;
        type_str(&mut a, "http://h/v1/2");
        assert_eq!(a.url(), "http://h/v1/2");
    }

    #[test]
    fn q_is_typable_in_the_url_bar() {
        let mut a = app();
        a.active_pane = ActivePane::UrlBar;
        type_str(&mut a, "api.dev?q=search");
        assert_eq!(a.url(), "api.dev?q=search");
    }

    #[test]
    fn digits_are_typable_in_the_json_body() {
        let mut a = app();
        a.active_pane = ActivePane::InputArea;
        a.input_tab = InputTab::Body;
        type_str(&mut a, "{\"id\": 12}");
        assert_eq!(a.body(), "{\"id\": 12}");
        assert_eq!(a.input_tab, InputTab::Body, "digits must not switch tabs");
    }

    #[test]
    fn q_quits_only_outside_text_fields() {
        let mut a = app();
        a.active_pane = ActivePane::ResponseViewer;
        assert_eq!(handle_key(&mut a, key('q')), Action::Quit);

        let mut b = app();
        b.active_pane = ActivePane::InputArea;
        assert_eq!(handle_key(&mut b, key('q')), Action::None);
        assert_eq!(b.body(), "q", "'q' must be text inside an editor");
    }

    #[test]
    fn shift_tab_cycles_backwards() {
        let mut a = app();
        a.active_pane = ActivePane::UrlBar;
        handle_key(&mut a, code(KeyCode::BackTab));
        assert_eq!(a.active_pane, ActivePane::MethodSelector);
        handle_key(&mut a, code(KeyCode::Tab));
        assert_eq!(a.active_pane, ActivePane::UrlBar);
    }

    #[test]
    fn digit_shortcuts_still_work_outside_text_fields() {
        let mut a = app();
        a.active_pane = ActivePane::ResponseViewer;
        handle_key(&mut a, key('2'));
        assert_eq!(a.input_tab, InputTab::Headers);
        assert_eq!(a.active_pane, ActivePane::InputArea);
    }

    #[test]
    fn alt_digits_switch_tabs_even_while_typing() {
        let mut a = app();
        a.active_pane = ActivePane::InputArea;
        handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Char('2'), KeyModifiers::ALT),
        );
        assert_eq!(a.input_tab, InputTab::Headers);
        assert_eq!(a.body(), "", "the digit must not land in the body");
    }

    #[test]
    fn editor_shortcuts_are_not_stolen_while_typing() {
        // Ctrl+W is delete-word, Ctrl+U undo, Ctrl+R redo, Ctrl+Y paste in the
        // editor. Intercepting them would make the body pane unusable.
        let mut a = app();
        a.active_pane = ActivePane::InputArea;
        type_str(&mut a, "hello world");
        handle_key(&mut a, ctrl('w'));
        assert_eq!(a.body(), "hello ", "Ctrl+W must delete a word, not toggle wrap");

        let wrap_before = a.settings.wrap_response;
        assert_eq!(a.settings.wrap_response, wrap_before);

        handle_key(&mut a, ctrl('u'));
        assert_eq!(a.body(), "hello world", "Ctrl+U must undo");
        handle_key(&mut a, ctrl('r'));
        assert_eq!(a.body(), "hello ", "Ctrl+R must redo");
        assert_eq!(a.overlay, Overlay::None, "Ctrl+R must not open history here");
    }

    #[test]
    fn those_same_shortcuts_work_outside_text_fields() {
        let mut a = app();
        a.active_pane = ActivePane::ResponseViewer;
        handle_key(&mut a, ctrl('r'));
        assert_eq!(a.overlay, Overlay::History);
        a.close_overlay();
        let before = a.settings.wrap_response;
        handle_key(&mut a, ctrl('w'));
        assert_ne!(a.settings.wrap_response, before);
        assert_eq!(handle_key(&mut a, ctrl('y')), Action::Copy);
    }

    #[test]
    fn f6_copies_from_anywhere_including_a_text_field() {
        let mut a = app();
        a.active_pane = ActivePane::InputArea;
        assert_eq!(handle_key(&mut a, code(KeyCode::F(6))), Action::Copy);
    }

    #[test]
    fn a_line_feed_inserts_a_newline_instead_of_deleting_the_line() {
        // Regression: a raw 0x0A arrives as Ctrl+J, which the editor maps to
        // "delete to start of line" — silently eating pasted JSON.
        let mut a = app();
        a.active_pane = ActivePane::InputArea;
        type_str(&mut a, "{");
        handle_key(&mut a, ctrl('j'));
        type_str(&mut a, "}");
        assert_eq!(a.body(), "{\n}");
    }

    #[test]
    fn a_line_feed_in_a_single_line_field_is_ignored() {
        let mut a = app();
        a.active_pane = ActivePane::UrlBar;
        type_str(&mut a, "a.dev");
        handle_key(&mut a, ctrl('j'));
        type_str(&mut a, "/x");
        assert_eq!(a.url(), "a.dev/x");
        assert_eq!(a.url_editor.lines().len(), 1);
    }

    // --- mission control ---------------------------------------------------

    #[test]
    fn send_and_quit_shortcuts_report_the_right_action() {
        let mut a = app();
        assert_eq!(handle_key(&mut a, ctrl('s')), Action::Send);
        assert_eq!(handle_key(&mut a, code(KeyCode::F(5))), Action::Send);
        assert_eq!(handle_key(&mut a, ctrl('q')), Action::Quit);
        assert_eq!(handle_key(&mut a, ctrl('c')), Action::Quit);
    }

    #[test]
    fn enter_in_the_url_bar_launches() {
        let mut a = app();
        a.active_pane = ActivePane::UrlBar;
        assert_eq!(handle_key(&mut a, code(KeyCode::Enter)), Action::Send);
        assert_eq!(a.url(), "", "Enter must not insert a line break");
    }

    #[test]
    fn escape_cancels_only_while_a_request_is_in_flight() {
        let mut a = app();
        assert_eq!(handle_key(&mut a, code(KeyCode::Esc)), Action::None);
        a.begin_request();
        assert_eq!(handle_key(&mut a, code(KeyCode::Esc)), Action::Cancel);
    }

    #[test]
    fn escape_never_quits_the_app() {
        let mut a = app();
        handle_key(&mut a, code(KeyCode::Esc));
        assert!(!a.should_quit, "Esc must not destroy an in-progress request");
    }

    #[test]
    fn method_cycles_in_both_directions() {
        let mut a = app();
        a.active_pane = ActivePane::MethodSelector;
        handle_key(&mut a, code(KeyCode::Right));
        assert_eq!(a.method, HttpMethod::Post);
        handle_key(&mut a, code(KeyCode::Left));
        assert_eq!(a.method, HttpMethod::Get);
        handle_key(&mut a, key(' '));
        assert_eq!(a.method, HttpMethod::Post);
    }

    // --- response viewer ---------------------------------------------------

    #[test]
    fn response_scrolling_is_bounded() {
        let mut a = app();
        a.active_pane = ActivePane::ResponseViewer;
        a.viewport_rows = 5;
        a.viewer.set_text("l\n".repeat(10));
        a.viewer.ensure_layout(80, false);
        for _ in 0..100 {
            handle_key(&mut a, key('j'));
        }
        assert_eq!(a.viewer.scroll, a.viewer.max_scroll(5));
        for _ in 0..100 {
            handle_key(&mut a, key('k'));
        }
        assert_eq!(a.viewer.scroll, 0);
    }

    #[test]
    fn arrows_cycle_response_tabs() {
        let mut a = app();
        a.active_pane = ActivePane::ResponseViewer;
        handle_key(&mut a, code(KeyCode::Right));
        assert_eq!(a.response_tab, ResponseTab::Headers);
        handle_key(&mut a, code(KeyCode::Left));
        assert_eq!(a.response_tab, ResponseTab::Body);
    }

    #[test]
    fn y_copies_from_the_response_pane() {
        let mut a = app();
        a.active_pane = ActivePane::ResponseViewer;
        assert_eq!(handle_key(&mut a, key('y')), Action::Copy);
        assert_eq!(handle_key(&mut a, ctrl('y')), Action::Copy);
    }

    #[test]
    fn s_and_f7_save_the_response_to_disk() {
        let mut a = app();
        a.active_pane = ActivePane::ResponseViewer;
        assert_eq!(handle_key(&mut a, key('s')), Action::SaveResponse);
        assert_eq!(handle_key(&mut a, code(KeyCode::F(7))), Action::SaveResponse);
    }

    #[test]
    fn wrap_toggle_flips_the_setting() {
        let mut a = app();
        let before = a.settings.wrap_response;
        a.active_pane = ActivePane::ResponseViewer;
        handle_key(&mut a, key('w'));
        assert_ne!(a.settings.wrap_response, before);
    }

    // --- overlays ----------------------------------------------------------

    #[test]
    fn help_opens_and_any_key_dismisses_it() {
        let mut a = app();
        a.active_pane = ActivePane::ResponseViewer;
        handle_key(&mut a, key('?'));
        assert_eq!(a.overlay, Overlay::Help);
        handle_key(&mut a, key('x'));
        assert_eq!(a.overlay, Overlay::None);
    }

    #[test]
    fn overlay_swallows_shortcuts_that_would_otherwise_fire() {
        let mut a = app();
        a.open_overlay(Overlay::SavePrompt);
        // 'q' must become text, not a quit command, while naming a request.
        assert_eq!(handle_key(&mut a, key('q')), Action::None);
        assert_eq!(a.name_editor.lines().join(""), "q");
    }

    #[test]
    fn escape_closes_an_overlay_instead_of_cancelling_a_request() {
        let mut a = app();
        a.begin_request();
        a.open_overlay(Overlay::History);
        assert_eq!(handle_key(&mut a, code(KeyCode::Esc)), Action::None);
        assert_eq!(a.overlay, Overlay::None);
        assert!(a.is_loading, "the flight must survive closing an overlay");
    }

    #[test]
    fn search_overlay_finds_and_navigates() {
        let mut a = app();
        a.viewport_rows = 4;
        a.viewer.set_text("alpha\nbeta\nalpha\n".to_string());
        a.viewer.ensure_layout(80, false);
        a.active_pane = ActivePane::ResponseViewer;
        handle_key(&mut a, key('/'));
        assert_eq!(a.overlay, Overlay::Search);
        type_str(&mut a, "alpha");
        assert_eq!(a.viewer.matches.len(), 2);
        handle_key(&mut a, code(KeyCode::Enter));
        assert_eq!(a.overlay, Overlay::None);
        handle_key(&mut a, key('n'));
        assert_eq!(a.viewer.current_match, 1);
    }

    #[test]
    fn history_entry_loads_into_the_request() {
        let mut a = app();
        a.history = vec![crate::store::HistoryEntry {
            request: crate::store::RequestSnapshot {
                method: "DELETE".into(),
                url: "https://loaded.dev".into(),
                headers: "X: 1".into(),
                body: String::new(),
            },
            status: 204,
            duration_ms: 5,
            at: 1,
        }];
        a.open_overlay(Overlay::History);
        handle_key(&mut a, code(KeyCode::Enter));
        assert_eq!(a.method, HttpMethod::Delete);
        assert_eq!(a.url(), "https://loaded.dev");
        assert_eq!(a.overlay, Overlay::None);
    }

    // --- mouse & paste -----------------------------------------------------

    #[test]
    fn wheel_scrolls_the_response() {
        let mut a = app();
        a.viewport_rows = 3;
        a.viewer.set_text("x\n".repeat(50));
        a.viewer.ensure_layout(80, false);
        handle_mouse(
            &mut a,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 1,
                row: 1,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(a.viewer.scroll, 3);
        assert_eq!(a.active_pane, ActivePane::ResponseViewer);
    }

    #[test]
    fn pasting_into_the_url_strips_line_breaks() {
        let mut a = app();
        a.active_pane = ActivePane::UrlBar;
        handle_paste(&mut a, "https://a.dev/x\n".to_string());
        assert_eq!(a.url(), "https://a.dev/x");
    }

    #[test]
    fn pasting_into_the_body_keeps_line_breaks() {
        let mut a = app();
        a.active_pane = ActivePane::InputArea;
        handle_paste(&mut a, "{\n  \"a\": 1\n}".to_string());
        assert_eq!(a.body(), "{\n  \"a\": 1\n}");
    }
}
