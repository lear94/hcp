use ratatui::widgets::{Block, Borders};
use tui_textarea::TextArea;

#[derive(Debug, Clone, PartialEq)]
pub enum HttpMethod {
    GET,
    POST,
    PUT,
    DELETE,
}

impl HttpMethod {
    pub fn next(&self) -> Self {
        match self {
            HttpMethod::GET => HttpMethod::POST,
            HttpMethod::POST => HttpMethod::PUT,
            HttpMethod::PUT => HttpMethod::DELETE,
            HttpMethod::DELETE => HttpMethod::GET,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActivePane {
    UrlBar,
    MethodSelector,
    InputArea,
    ResponseViewer,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputTab {
    Body,
    Headers,
}

pub struct App<'a> {
    pub url_input: String,
    pub method: HttpMethod,
    pub body_editor: TextArea<'a>,
    pub headers_editor: TextArea<'a>,
    pub active_pane: ActivePane,
    pub input_tab: InputTab,
    pub response_text: String,
    pub response_status: u16,
    pub response_scroll: u16,
    pub telemetry: Option<crate::telemetry::MissionTelemetry>,
    pub is_loading: bool,
}

impl<'a> App<'a> {
    pub fn new() -> Self {
        let mut body_ta = TextArea::default();
        body_ta.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" BODY (JSON) "),
        );

        let mut headers_ta = TextArea::default();
        headers_ta.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" HEADERS (Key: Value) "),
        );
        headers_ta.insert_str("Content-Type: application/json");

        Self {
            url_input: "https://httpbin.org/bearer".to_string(),
            method: HttpMethod::GET,
            body_editor: body_ta,
            headers_editor: headers_ta,
            active_pane: ActivePane::UrlBar,
            input_tab: InputTab::Body,
            response_text: String::new(),
            response_status: 0,
            response_scroll: 0,
            telemetry: None,
            is_loading: false,
        }
    }

    pub fn cycle_focus(&mut self) {
        self.active_pane = match self.active_pane {
            ActivePane::UrlBar => ActivePane::MethodSelector,
            ActivePane::MethodSelector => ActivePane::InputArea,
            ActivePane::InputArea => ActivePane::ResponseViewer,
            ActivePane::ResponseViewer => ActivePane::UrlBar,
        };
    }

    pub fn scroll_response(&mut self, amount: i16) {
        let new_pos = self.response_scroll as i16 + amount;
        if new_pos < 0 {
            self.response_scroll = 0;
        } else {
            self.response_scroll = new_pos as u16;
        }
    }
}
