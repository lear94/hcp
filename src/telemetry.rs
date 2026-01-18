use std::time::Duration;

#[derive(Debug, Clone)]
pub struct MissionTelemetry {
    pub dns_handshake_ttfb: Duration,
    pub transfer: Duration,
    pub total: Duration,
    pub size_bytes: u64,
    pub status: u16,
}

impl MissionTelemetry {
    pub fn render_bar(&self, duration: Duration, max_duration: Duration) -> String {
        let max_width = 20;
        let nanos = duration.as_nanos() as f64;
        let max_nanos = max_duration.as_nanos() as f64;

        let ratio = if max_nanos > 0.0 {
            nanos / max_nanos
        } else {
            0.0
        };
        let bars = (ratio * max_width as f64) as usize;
        let bars = bars.min(max_width);

        let filled = "█".repeat(bars);
        let empty = "░".repeat(max_width - bars);
        format!("{}{}", filled, empty)
    }
}
