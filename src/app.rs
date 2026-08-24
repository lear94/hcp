use crate::engine::{EngineConfig, HttpMethod, MissionResult};
use crate::store::{self, HistoryEntry, RequestSnapshot, SavedRequest, Settings};
use crate::syntax;
use crate::telemetry::MissionTelemetry;
use crate::viewer::TextViewer;
use ratatui::layout::Rect;
use std::sync::Arc;
use ratatui::widgets::{Block, Borders};
use std::time::{Duration, Instant};
use tui_textarea::TextArea;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePane {
    MethodSelector,
    UrlBar,
    InputArea,
    ResponseViewer,
}

impl ActivePane {
    const ORDER: [ActivePane; 4] = [
        ActivePane::MethodSelector,
        ActivePane::UrlBar,
        ActivePane::InputArea,
        ActivePane::ResponseViewer,
    ];

    fn shift(self, delta: isize) -> Self {
        let idx = Self::ORDER.iter().position(|p| *p == self).unwrap_or(0) as isize;
        let len = Self::ORDER.len() as isize;
        Self::ORDER[((idx + delta).rem_euclid(len)) as usize]
    }

    /// Panes where typed characters are text, not shortcuts.
    pub fn is_text_input(self) -> bool {
        matches!(self, ActivePane::InputArea | ActivePane::UrlBar)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputTab {
    Body,
    Headers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseTab {
    Body,
    Headers,
    Raw,
}

impl ResponseTab {
    pub const ALL: [ResponseTab; 3] = [ResponseTab::Body, ResponseTab::Headers, ResponseTab::Raw];

    pub fn title(self) -> &'static str {
        match self {
            ResponseTab::Body => " BODY ",
            ResponseTab::Headers => " HEADERS ",
            ResponseTab::Raw => " RAW ",
        }
    }

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

/// Where each pane was last drawn, so mouse clicks can focus the right one.
#[derive(Debug, Clone, Copy, Default)]
pub struct LayoutRects {
    pub method: Rect,
    pub url: Rect,
    pub request: Rect,
    pub response: Rect,
}

impl LayoutRects {
    pub fn pane_at(&self, x: u16, y: u16) -> Option<ActivePane> {
        let hit = |r: Rect| {
            r.width > 0
                && r.height > 0
                && x >= r.x
                && x < r.x + r.width
                && y >= r.y
                && y < r.y + r.height
        };
        if hit(self.method) {
            Some(ActivePane::MethodSelector)
        } else if hit(self.url) {
            Some(ActivePane::UrlBar)
        } else if hit(self.request) {
            Some(ActivePane::InputArea)
        } else if hit(self.response) {
            Some(ActivePane::ResponseViewer)
        } else {
            None
        }
    }
}

/// Which modal is on top of the cockpit, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help,
    History,
    Collection,
    SavePrompt,
    Search,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warn,
    Error,
}

pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub born: Instant,
}

impl Toast {
    const TTL: Duration = Duration::from_secs(4);
    pub fn expired(&self) -> bool {
        self.born.elapsed() > Self::TTL
    }
}

/// A response the cockpit has fully received and prepared for display.
pub struct ResponseData {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub content_type: Option<String>,
    pub final_url: String,
    pub size_bytes: u64,
    pub truncated: bool,
    pub is_binary: bool,
    pub is_json: bool,
    /// Pretty-printed JSON, when the payload is JSON and small enough to format.
    /// Shared with the viewer rather than copied into it.
    pretty: Option<Arc<str>>,
    /// Text as received (or a hex dump when the payload is binary).
    raw_text: Arc<str>,
    /// The payload exactly as it arrived. Kept so the pilot can write a binary
    /// download back to disk byte for byte — a hex preview cannot do that.
    bytes: Vec<u8>,
}

/// Formatting a gigantic payload costs more than it helps; above this the
/// response is shown verbatim and the pilot is told why.
const PRETTY_LIMIT_BYTES: usize = 8 * 1024 * 1024;
/// How much of a binary payload to render as a hex dump.
const HEX_PREVIEW_BYTES: usize = 64 * 1024;

impl ResponseData {
    pub fn from_result(result: MissionResult, pretty_enabled: bool) -> (Self, Vec<String>) {
        let MissionResult {
            telemetry,
            headers,
            body,
            content_type,
            final_url,
            truncated,
            mut warnings,
        } = result;

        let is_binary = looks_binary(&body);
        let raw_text: Arc<str> = if is_binary {
            Arc::from(hex_dump(&body, HEX_PREVIEW_BYTES))
        } else {
            Arc::from(String::from_utf8_lossy(&body).into_owned())
        };

        let is_json = !is_binary && syntax::looks_like_json(content_type.as_deref(), &raw_text);
        let byte_len = body.len();
        let pretty = if is_json && pretty_enabled {
            if byte_len > PRETTY_LIMIT_BYTES {
                warnings.push(format!(
                    "Payload is {} — shown unformatted to keep the UI responsive.",
                    crate::telemetry::fmt_bytes(byte_len as u64)
                ));
                None
            } else {
                serde_json::from_str::<serde_json::Value>(&raw_text)
                    .ok()
                    .and_then(|v| serde_json::to_string_pretty(&v).ok())
                    .map(Arc::from)
            }
        } else {
            None
        };

        (
            Self {
                status: telemetry.status,
                headers,
                content_type,
                final_url,
                size_bytes: telemetry.size_bytes,
                truncated,
                is_binary,
                is_json,
                pretty,
                raw_text,
                bytes: body,
            },
            warnings,
        )
    }

    /// The text to show for a given response tab. Body and raw hand back a
    /// shared handle, so switching tabs on a 30 MB payload copies nothing.
    pub fn text_for(&self, tab: ResponseTab) -> Arc<str> {
        match tab {
            ResponseTab::Body => self
                .pretty
                .clone()
                .unwrap_or_else(|| self.raw_text.clone()),
            ResponseTab::Raw => self.raw_text.clone(),
            ResponseTab::Headers => {
                let mut out = String::with_capacity(self.headers.len() * 48 + 64);
                out.push_str(&format!("{} {}\n\n", "URL:", self.final_url));
                let width = self
                    .headers
                    .iter()
                    .map(|(k, _)| k.len())
                    .max()
                    .unwrap_or(0)
                    .min(32);
                for (k, v) in &self.headers {
                    out.push_str(&format!("{k:<width$}  {v}\n", width = width));
                }
                if self.headers.is_empty() {
                    out.push_str("(no response headers)\n");
                }
                Arc::from(out)
            }
        }
    }

    /// The payload exactly as received.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Whether the current tab's content should be JSON-highlighted.
    pub fn highlight_json(&self, tab: ResponseTab) -> bool {
        self.is_json && matches!(tab, ResponseTab::Body | ResponseTab::Raw)
    }
}

fn looks_binary(bytes: &[u8]) -> bool {
    let probe = &bytes[..bytes.len().min(8192)];
    if probe.is_empty() {
        return false;
    }
    // A NUL byte is the classic tell; otherwise flag heavy control-byte density.
    if probe.contains(&0) {
        return true;
    }
    let control = probe
        .iter()
        .filter(|b| **b < 0x09 || (**b > 0x0d && **b < 0x20))
        .count();
    control * 100 / probe.len() > 5
}

fn hex_dump(bytes: &[u8], limit: usize) -> String {
    let slice = &bytes[..bytes.len().min(limit)];
    let mut out = String::with_capacity(slice.len() / 16 * 78 + 128);
    for (i, chunk) in slice.chunks(16).enumerate() {
        out.push_str(&format!("{:08x}  ", i * 16));
        for (j, b) in chunk.iter().enumerate() {
            out.push_str(&format!("{b:02x} "));
            if j == 7 {
                out.push(' ');
            }
        }
        for j in chunk.len()..16 {
            out.push_str("   ");
            if j == 7 {
                out.push(' ');
            }
        }
        out.push_str(" |");
        for b in chunk {
            out.push(if (0x20..0x7f).contains(b) {
                *b as char
            } else {
                '.'
            });
        }
        out.push_str("|\n");
    }
    if bytes.len() > limit {
        out.push_str(&format!(
            "\n… {} more bytes not shown (binary payload).\n",
            crate::telemetry::fmt_bytes((bytes.len() - limit) as u64)
        ));
    }
    out
}

pub struct App<'a> {
    pub url_editor: TextArea<'a>,
    pub method: HttpMethod,
    pub body_editor: TextArea<'a>,
    pub headers_editor: TextArea<'a>,
    pub active_pane: ActivePane,
    pub input_tab: InputTab,
    pub response_tab: ResponseTab,

    pub viewer: TextViewer,
    pub response: Option<ResponseData>,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub telemetry: Option<MissionTelemetry>,

    pub is_loading: bool,
    pub request_started: Option<Instant>,
    pub spinner: usize,
    /// Monotonic id so a response from a cancelled flight is never shown.
    pub request_id: u64,

    /// Settings in force for this session, including one-off command-line
    /// overrides.
    pub settings: Settings,
    /// The on-disk copy. Command-line overrides must never leak into it — a
    /// single `-k` should not disable certificate checking forever.
    saved_settings: Settings,
    pub engine_config: EngineConfig,
    pub history: Vec<HistoryEntry>,
    pub collection: Vec<SavedRequest>,

    pub overlay: Overlay,
    pub list_index: usize,
    pub name_editor: TextArea<'a>,
    pub search_editor: TextArea<'a>,

    pub toast: Option<Toast>,
    pub should_quit: bool,
    /// Rows available in the response viewport, refreshed by the renderer.
    pub viewport_rows: usize,
    pub layout: LayoutRects,
}

impl<'a> App<'a> {
    /// `settings` is the effective configuration (disk + command line);
    /// `on_disk` is what should be written back when a preference changes.
    pub fn new(settings: Settings, engine_config: EngineConfig) -> Self {
        let on_disk = settings.clone();
        Self::with_persisted(settings, on_disk, engine_config)
    }

    pub fn with_persisted(
        settings: Settings,
        on_disk: Settings,
        engine_config: EngineConfig,
    ) -> Self {
        let mut body_ta = TextArea::default();
        body_ta.set_placeholder_text("Request body — JSON is validated before launch");

        let mut headers_ta = TextArea::new(vec!["Content-Type: application/json".to_string()]);
        headers_ta.set_placeholder_text("Key: Value (one per line, # for comments)");
        headers_ta.move_cursor(tui_textarea::CursorMove::End);

        let mut url_ta = TextArea::default();
        url_ta.set_placeholder_text("https://api.example.com/v1/status");
        url_ta.set_block(Block::default().borders(Borders::NONE));

        let mut name_ta = TextArea::default();
        name_ta.set_placeholder_text("Name this request");

        let mut search_ta = TextArea::default();
        search_ta.set_placeholder_text("Search response…");

        Self {
            url_editor: url_ta,
            method: HttpMethod::Get,
            body_editor: body_ta,
            headers_editor: headers_ta,
            active_pane: ActivePane::UrlBar,
            input_tab: InputTab::Body,
            response_tab: ResponseTab::Body,
            viewer: TextViewer::default(),
            response: None,
            error: None,
            warnings: Vec::new(),
            telemetry: None,
            is_loading: false,
            request_started: None,
            spinner: 0,
            request_id: 0,
            settings,
            saved_settings: on_disk,
            engine_config,
            history: store::load_history(),
            collection: store::load_collection(),
            overlay: Overlay::None,
            list_index: 0,
            name_editor: name_ta,
            search_editor: search_ta,
            toast: None,
            should_quit: false,
            viewport_rows: 20,
            layout: LayoutRects::default(),
        }
    }

    // ---- request state -------------------------------------------------

    pub fn url(&self) -> String {
        self.url_editor.lines().first().cloned().unwrap_or_default()
    }

    pub fn set_url(&mut self, url: &str) {
        let mut ta = TextArea::new(vec![url.to_string()]);
        ta.set_placeholder_text("https://api.example.com/v1/status");
        ta.set_block(Block::default().borders(Borders::NONE));
        ta.move_cursor(tui_textarea::CursorMove::End);
        self.url_editor = ta;
    }

    pub fn body(&self) -> String {
        self.body_editor.lines().join("\n")
    }

    pub fn headers(&self) -> String {
        self.headers_editor.lines().join("\n")
    }

    pub fn snapshot(&self) -> RequestSnapshot {
        RequestSnapshot {
            method: self.method.as_str().to_string(),
            url: self.url(),
            headers: self.headers(),
            body: self.body(),
        }
    }

    pub fn load_snapshot(&mut self, snap: &RequestSnapshot) {
        self.method = snap.method_enum();
        self.set_url(&snap.url);
        self.body_editor = editor_from(&snap.body, "Request body — JSON is validated before launch");
        self.headers_editor = editor_from(&snap.headers, "Key: Value (one per line, # for comments)");
    }

    /// Validates the body before it leaves the terminal. Returns the payload to
    /// send, or an error message describing exactly what is malformed.
    pub fn prepare_body(&self) -> Result<Option<String>, String> {
        let raw = self.body();
        if raw.trim().is_empty() || !self.method.allows_body() {
            return Ok(None);
        }
        let declared_json = self
            .headers()
            .lines()
            .filter_map(|l| l.split_once(':'))
            .any(|(k, v)| k.trim().eq_ignore_ascii_case("content-type") && v.contains("json"));
        let shaped_json = {
            let t = raw.trim_start();
            t.starts_with('{') || t.starts_with('[')
        };

        if declared_json || shaped_json {
            if let Err(e) = serde_json::from_str::<serde_json::Value>(&raw) {
                return Err(json_error_report(&raw, &e));
            }
        }
        Ok(Some(raw))
    }

    // ---- focus & navigation --------------------------------------------

    pub fn cycle_focus(&mut self, forward: bool) {
        self.active_pane = self.active_pane.shift(if forward { 1 } else { -1 });
    }

    pub fn set_response_tab(&mut self, tab: ResponseTab) {
        if self.response_tab != tab {
            self.response_tab = tab;
            self.refresh_viewer();
        }
    }

    /// Rebuilds the viewer's buffer for the current tab, preserving the search.
    pub fn refresh_viewer(&mut self) {
        let query = self.viewer.query.clone();
        let text: Arc<str> = match (&self.response, &self.error) {
            (_, Some(err)) => Arc::from(err.as_str()),
            (Some(resp), None) => resp.text_for(self.response_tab),
            (None, None) => Arc::from(""),
        };
        self.viewer.set_text(text);
        if !query.is_empty() {
            self.viewer.set_query(query);
        }
    }

    // ---- flight lifecycle ------------------------------------------------

    pub fn begin_request(&mut self) -> u64 {
        self.request_id += 1;
        self.is_loading = true;
        self.request_started = Some(Instant::now());
        self.error = None;
        self.warnings.clear();
        self.request_id
    }

    pub fn finish_success(&mut self, result: MissionResult) {
        let telemetry = result.telemetry.clone();
        let (data, warnings) = ResponseData::from_result(result, self.settings.pretty_json);

        let snapshot = self.snapshot();
        store::push_history(
            &mut self.history,
            HistoryEntry {
                request: snapshot,
                status: data.status,
                duration_ms: telemetry.total.as_millis() as u64,
                at: store::now_epoch(),
            },
        );
        if let Err(e) = store::save_history(&self.history) {
            self.warnings.push(format!("Could not save history: {e}"));
        }

        self.telemetry = Some(telemetry);
        self.response = Some(data);
        self.error = None;
        self.warnings.extend(warnings);
        self.is_loading = false;
        self.request_started = None;
        self.response_tab = ResponseTab::Body;
        self.refresh_viewer();
    }

    pub fn finish_error(&mut self, message: String) {
        self.show_error("REQUEST FAILED", message, "Request failed");
    }

    /// A request rejected before it left the terminal — the pilot needs to know
    /// nothing was sent, which "request failed" would not convey.
    pub fn reject_preflight(&mut self, message: String) {
        self.show_error("NOT SENT — PRE-FLIGHT CHECK FAILED", message, "Not sent — fix the body");
    }

    fn show_error(&mut self, heading: &str, message: String, toast: &str) {
        self.is_loading = false;
        self.request_started = None;
        self.telemetry = None;
        self.response = None;
        self.error = Some(format!("{heading}\n\n{message}"));
        self.refresh_viewer();
        self.toast(toast, ToastKind::Error);
    }

    pub fn cancel_request(&mut self) {
        if !self.is_loading {
            return;
        }
        // Bumping the id orphans any reply still in flight.
        self.request_id += 1;
        self.is_loading = false;
        self.request_started = None;
        self.toast("Request cancelled", ToastKind::Warn);
    }

    pub fn clear_response(&mut self) {
        self.response = None;
        self.error = None;
        self.telemetry = None;
        self.warnings.clear();
        self.viewer.clear();
        self.toast("Response cleared", ToastKind::Info);
    }

    // ---- overlays & feedback --------------------------------------------

    pub fn toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.toast = Some(Toast {
            text: text.into(),
            kind,
            born: Instant::now(),
        });
    }

    pub fn tick(&mut self) {
        if self.is_loading {
            self.spinner = self.spinner.wrapping_add(1);
        }
        if self.toast.as_ref().is_some_and(|t| t.expired()) {
            self.toast = None;
        }
    }

    /// Records an in-app preference change without persisting any command-line
    /// override that happens to be active.
    pub fn set_wrap(&mut self, wrap: bool) {
        self.settings.wrap_response = wrap;
        self.saved_settings.wrap_response = wrap;
        if let Err(e) = store::save_settings(&self.saved_settings) {
            self.toast(format!("Could not save settings: {e}"), ToastKind::Warn);
        }
    }

    #[cfg(test)]
    pub fn persisted(&self) -> &Settings {
        &self.saved_settings
    }

    pub fn open_overlay(&mut self, overlay: Overlay) {
        self.overlay = overlay;
        self.list_index = 0;
        if overlay == Overlay::SavePrompt {
            self.name_editor = editor_from("", "Name this request");
        }
        if overlay == Overlay::Search {
            self.search_editor = editor_from(&self.viewer.query, "Search response…");
            self.search_editor.move_cursor(tui_textarea::CursorMove::End);
        }
    }

    pub fn close_overlay(&mut self) {
        self.overlay = Overlay::None;
    }

    pub fn overlay_len(&self) -> usize {
        match self.overlay {
            Overlay::History => self.history.len(),
            Overlay::Collection => self.collection.len(),
            _ => 0,
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        let len = self.overlay_len();
        if len == 0 {
            self.list_index = 0;
            return;
        }
        let next = (self.list_index as isize + delta).rem_euclid(len as isize);
        self.list_index = next as usize;
    }

    pub fn save_current_request(&mut self, name: String) {
        let name = name.trim().to_string();
        if name.is_empty() {
            self.toast("Name cannot be empty", ToastKind::Warn);
            return;
        }
        let entry = SavedRequest {
            name: name.clone(),
            request: self.snapshot(),
        };
        match self.collection.iter_mut().find(|s| s.name == name) {
            Some(existing) => *existing = entry,
            None => self.collection.push(entry),
        }
        match store::save_collection(&self.collection) {
            Ok(()) => self.toast(format!("Saved '{name}'"), ToastKind::Success),
            Err(e) => self.toast(format!("Could not save: {e}"), ToastKind::Error),
        }
    }

    pub fn delete_selected_saved(&mut self) {
        if self.list_index >= self.collection.len() {
            return;
        }
        let removed = self.collection.remove(self.list_index);
        self.list_index = self.list_index.min(self.collection.len().saturating_sub(1));
        match store::save_collection(&self.collection) {
            Ok(()) => self.toast(format!("Deleted '{}'", removed.name), ToastKind::Info),
            Err(e) => self.toast(format!("Could not save: {e}"), ToastKind::Error),
        }
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
        self.list_index = 0;
        match store::save_history(&self.history) {
            Ok(()) => self.toast("History cleared", ToastKind::Info),
            Err(e) => self.toast(format!("Could not save: {e}"), ToastKind::Error),
        }
    }

    /// Writes the payload, byte for byte, next to wherever `hcp` was launched.
    /// Returns the path so the pilot can find it.
    pub fn save_response_to_file(&self) -> Result<std::path::PathBuf, String> {
        let Some(resp) = &self.response else {
            return Err("No response to save yet".to_string());
        };
        let name = suggested_filename(&resp.final_url, resp.content_type.as_deref());
        let mut path = std::path::PathBuf::from(&name);
        // Never clobber an earlier download.
        let mut n = 1;
        while path.exists() {
            let stem = std::path::Path::new(&name)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "response".into());
            let ext = std::path::Path::new(&name)
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            path = std::path::PathBuf::from(format!("{stem}-{n}{ext}"));
            n += 1;
        }
        std::fs::write(&path, resp.bytes())
            .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
        Ok(path)
    }

    /// Text handed to the clipboard for the current view.
    pub fn copy_target(&self) -> Option<String> {
        if self.viewer.is_empty() {
            None
        } else {
            Some(self.viewer.text().to_string())
        }
    }
}

/// Points at the exact character serde rejected, the way a compiler would.
/// `serde_json`'s message already carries the position, so it is not repeated.
fn json_error_report(source: &str, err: &serde_json::Error) -> String {
    let mut out = format!("Invalid JSON body — {err}");
    let (line, col) = (err.line(), err.column());
    if line == 0 {
        return out;
    }
    if let Some(text) = source.lines().nth(line - 1) {
        // Long lines are windowed so the caret stays on screen.
        let (shown, shift) = if text.chars().count() > 76 && col > 40 {
            let skip = col - 40;
            let start = text
                .char_indices()
                .nth(skip)
                .map(|(i, _)| i)
                .unwrap_or(0);
            (format!("…{}", &text[start..]), skip - 1)
        } else {
            (text.to_string(), 0)
        };
        let caret_col = col.saturating_sub(1).saturating_sub(shift);
        let gutter = format!("{line}");
        out.push_str("\n\n");
        out.push_str(&format!("  {gutter} │ {}\n", shown.trim_end()));
        out.push_str(&format!(
            "  {} │ {}^\n",
            " ".repeat(gutter.len()),
            " ".repeat(caret_col)
        ));
    }
    out
}

/// Derives a safe local filename from the request URL and content type.
fn suggested_filename(url: &str, content_type: Option<&str>) -> String {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    // Drop scheme and authority first, or the host would be mistaken for a
    // filename whenever the URL has no path.
    let after_authority = match without_query.split_once("://") {
        Some((_, rest)) => rest.split_once('/').map(|(_, p)| p).unwrap_or(""),
        None => without_query,
    };
    let last = after_authority.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let mut base: String = last
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    if base.is_empty() || base == "." || base == ".." {
        base = "response".to_string();
    }
    if base.len() > 64 {
        base.truncate(64);
    }
    if !base.contains('.') {
        base.push('.');
        base.push_str(extension_for(content_type));
    }
    base
}

fn extension_for(content_type: Option<&str>) -> &'static str {
    let ct = content_type.unwrap_or("").to_ascii_lowercase();
    if ct.contains("json") {
        "json"
    } else if ct.contains("html") {
        "html"
    } else if ct.contains("xml") {
        "xml"
    } else if ct.contains("csv") {
        "csv"
    } else if ct.contains("javascript") {
        "js"
    } else if ct.contains("css") {
        "css"
    } else if ct.starts_with("text/") {
        "txt"
    } else if ct.contains("png") {
        "png"
    } else if ct.contains("jpeg") {
        "jpg"
    } else if ct.contains("pdf") {
        "pdf"
    } else {
        "bin"
    }
}

fn editor_from<'a>(content: &str, placeholder: &'a str) -> TextArea<'a> {
    let lines: Vec<String> = if content.is_empty() {
        vec![String::new()]
    } else {
        content.lines().map(|l| l.to_string()).collect()
    };
    let mut ta = TextArea::new(lines);
    ta.set_placeholder_text(placeholder);
    ta
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App<'static> {
        App::new(Settings::default(), EngineConfig::default())
    }

    #[test]
    fn focus_cycles_forward_and_backward() {
        let mut a = app();
        a.active_pane = ActivePane::MethodSelector;
        a.cycle_focus(true);
        assert_eq!(a.active_pane, ActivePane::UrlBar);
        a.cycle_focus(false);
        assert_eq!(a.active_pane, ActivePane::MethodSelector);
        // Shift+Tab from the first pane must wrap to the last, not stall.
        a.cycle_focus(false);
        assert_eq!(a.active_pane, ActivePane::ResponseViewer);
    }

    #[test]
    fn text_panes_are_recognised() {
        assert!(ActivePane::UrlBar.is_text_input());
        assert!(ActivePane::InputArea.is_text_input());
        assert!(!ActivePane::ResponseViewer.is_text_input());
    }

    #[test]
    fn body_is_only_sent_for_verbs_that_allow_it() {
        let mut a = app();
        a.body_editor = editor_from("{\"a\":1}", "");
        a.method = HttpMethod::Get;
        assert_eq!(a.prepare_body().unwrap(), None);
        a.method = HttpMethod::Post;
        assert!(a.prepare_body().unwrap().is_some());
    }

    #[test]
    fn malformed_json_is_caught_and_pointed_at() {
        let mut a = app();
        a.method = HttpMethod::Post;
        a.body_editor = editor_from("{\n  \"a\": 1,\n}", "");
        let err = a.prepare_body().unwrap_err();
        assert!(err.starts_with("Invalid JSON body — "), "{err}");
        assert!(err.contains('^'), "the offending column must be marked: {err}");
        assert_eq!(
            err.matches("line 3").count(),
            1,
            "the position must not be repeated: {err}"
        );
    }

    #[test]
    fn json_error_report_survives_a_very_long_line() {
        let long = format!("{{\"k\": \"{}\" bad}}", "x".repeat(500));
        let err = serde_json::from_str::<serde_json::Value>(&long).unwrap_err();
        let report = json_error_report(&long, &err);
        assert!(report.contains('^'));
        for line in report.lines() {
            assert!(line.chars().count() < 120, "line too wide: {line}");
        }
    }

    #[test]
    fn non_json_body_passes_through_untouched() {
        let mut a = app();
        a.method = HttpMethod::Post;
        a.headers_editor = editor_from("Content-Type: text/plain", "");
        a.body_editor = editor_from("just some text", "");
        assert_eq!(
            a.prepare_body().unwrap().as_deref(),
            Some("just some text")
        );
    }

    #[test]
    fn command_line_overrides_never_reach_the_config_file() {
        // Session settings carry `-k`; the on-disk copy must not.
        let session = Settings {
            insecure: true,
            timeout_secs: 3,
            ..Settings::default()
        };
        let on_disk = Settings::default();
        let mut a = App::with_persisted(session, on_disk, EngineConfig::default());

        assert!(a.settings.insecure);
        a.set_wrap(!a.settings.wrap_response);

        assert!(
            !a.persisted().insecure,
            "a one-off --insecure must not be written to the config file"
        );
        assert_eq!(a.persisted().timeout_secs, 30);
        assert_eq!(a.persisted().wrap_response, a.settings.wrap_response);
    }

    #[test]
    fn cancelling_orphans_the_in_flight_reply() {
        let mut a = app();
        let id = a.begin_request();
        a.cancel_request();
        assert!(!a.is_loading);
        assert_ne!(id, a.request_id, "a late reply must no longer match");
    }

    #[test]
    fn url_round_trips_through_the_editor() {
        let mut a = app();
        a.set_url("https://example.com/x?q=1&n=2");
        assert_eq!(a.url(), "https://example.com/x?q=1&n=2");
    }

    #[test]
    fn snapshots_restore_the_whole_request() {
        let mut a = app();
        a.method = HttpMethod::Patch;
        a.set_url("https://a.dev");
        a.body_editor = editor_from("{\"k\":1}", "");
        a.headers_editor = editor_from("X: 1\nY: 2", "");
        let snap = a.snapshot();

        let mut b = app();
        b.load_snapshot(&snap);
        assert_eq!(b.method, HttpMethod::Patch);
        assert_eq!(b.url(), "https://a.dev");
        assert_eq!(b.body(), "{\"k\":1}");
        assert_eq!(b.headers(), "X: 1\nY: 2");
    }

    #[test]
    fn binary_detection_and_hex_dump() {
        assert!(looks_binary(&[0x00, 0x01, 0x02]));
        assert!(!looks_binary(b"plain text\nwith newlines\n"));
        let dump = hex_dump(b"AB", 64);
        assert!(dump.contains("41 42"), "{dump}");
        assert!(dump.contains("|AB|"), "{dump}");
    }

    #[test]
    fn hex_dump_reports_what_it_omitted() {
        let dump = hex_dump(&[0u8; 100], 16);
        assert!(dump.contains("more bytes not shown"), "{dump}");
    }

    #[test]
    fn download_names_are_derived_safely() {
        assert_eq!(
            suggested_filename("https://a.dev/v1/report.csv?x=1", Some("text/csv")),
            "report.csv"
        );
        assert_eq!(
            suggested_filename("https://a.dev/v1/users", Some("application/json")),
            "users.json"
        );
        assert_eq!(suggested_filename("https://a.dev/", None), "response.bin");
        assert_eq!(suggested_filename("https://a.dev", None), "response.bin");
        assert_eq!(
            suggested_filename("https://a.dev/deep/path/", Some("text/plain")),
            "path.txt"
        );
        // Path traversal and shell metacharacters must never reach the disk.
        assert_eq!(
            suggested_filename("https://a.dev/../../etc/passwd", Some("text/plain")),
            "passwd.txt"
        );
        // Spaces, semicolons and quotes are stripped; the result is always a
        // plain relative name that no shell can reinterpret.
        assert_eq!(
            suggested_filename("https://a.dev/a;rm -rf b", Some("text/plain")),
            "arm-rfb.txt"
        );
        for url in [
            "https://a.dev/../../etc/passwd",
            "https://a.dev/a;rm -rf b",
            "https://a.dev/$(whoami)",
            "https://a.dev/a\\b",
        ] {
            let name = suggested_filename(url, None);
            assert!(!name.contains('/'), "{name}");
            assert!(!name.contains(std::path::MAIN_SEPARATOR), "{name}");
            assert!(!name.starts_with('-'), "{name}");
            assert!(
                name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)),
                "{name}"
            );
        }
    }

    #[test]
    fn very_long_url_segments_do_not_produce_absurd_filenames() {
        let name = suggested_filename(&format!("https://a.dev/{}", "n".repeat(500)), None);
        assert!(name.len() <= 70, "{name}");
    }

    #[test]
    fn selection_wraps_within_overlay_bounds() {
        let mut a = app();
        a.collection = vec![
            SavedRequest { name: "a".into(), request: a.snapshot() },
            SavedRequest { name: "b".into(), request: a.snapshot() },
        ];
        a.overlay = Overlay::Collection;
        a.move_selection(-1);
        assert_eq!(a.list_index, 1, "moving up from the top wraps to the end");
        a.move_selection(1);
        assert_eq!(a.list_index, 0);
    }

    #[test]
    fn clicks_map_to_the_pane_under_the_cursor() {
        let rects = LayoutRects {
            method: Rect::new(0, 0, 11, 3),
            url: Rect::new(11, 0, 60, 3),
            request: Rect::new(0, 3, 30, 20),
            response: Rect::new(30, 3, 41, 20),
        };
        assert_eq!(rects.pane_at(5, 1), Some(ActivePane::MethodSelector));
        assert_eq!(rects.pane_at(20, 1), Some(ActivePane::UrlBar));
        assert_eq!(rects.pane_at(5, 10), Some(ActivePane::InputArea));
        assert_eq!(rects.pane_at(40, 10), Some(ActivePane::ResponseViewer));
        assert_eq!(rects.pane_at(200, 200), None);
    }

    #[test]
    fn zero_sized_panes_never_swallow_clicks() {
        let rects = LayoutRects::default();
        assert_eq!(rects.pane_at(0, 0), None);
    }

    #[test]
    fn selection_on_empty_overlay_is_safe() {
        let mut a = app();
        a.overlay = Overlay::History;
        a.history.clear();
        a.move_selection(1);
        assert_eq!(a.list_index, 0);
    }
}
