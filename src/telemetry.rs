use std::time::Duration;

/// Forensic breakdown of a single request's lifecycle.
///
/// Every phase is measured from a monotonic clock. Phases that did not happen
/// for this request (for example DNS + connect on a pooled/reused connection)
/// are `None` rather than zero, so the UI can tell "instant" apart from
/// "did not occur".
#[derive(Debug, Clone, Default)]
pub struct MissionTelemetry {
    /// Name resolution time. `None` when the connection was reused.
    pub dns: Option<Duration>,
    /// TCP handshake + TLS negotiation. `None` when the connection was reused.
    pub connect: Option<Duration>,
    /// Time the server spent before the first response byte reached us.
    pub server: Duration,
    /// Time spent streaming the response body.
    pub transfer: Duration,
    /// Wall-clock time for the whole exchange.
    pub total: Duration,
    pub size_bytes: u64,
    pub status: u16,
    /// True when the request rode an already-established connection.
    pub reused_connection: bool,
    /// Negotiated HTTP version, e.g. "HTTP/2.0".
    pub http_version: String,
    /// Remote socket address, when the transport exposed one.
    pub remote_addr: Option<String>,
}

/// A single row of the telemetry waterfall. A phase that did not run carries
/// the reason, because "0 ms" and "never happened" mean very different things
/// when you are diagnosing latency.
pub struct Phase {
    pub label: &'static str,
    pub value: Option<Duration>,
    pub note: &'static str,
}

impl MissionTelemetry {
    /// The phases in chronological order, ready to be drawn as a waterfall.
    pub fn phases(&self) -> [Phase; 4] {
        let dns_note = if self.reused_connection {
            "connection reused"
        } else {
            // A fresh connection with no lookup means the host was already an
            // address, so the resolver was never consulted.
            "literal IP — no lookup"
        };
        [
            Phase { label: "DNS", value: self.dns, note: dns_note },
            Phase { label: "CONNECT", value: self.connect, note: "connection reused" },
            Phase { label: "SERVER", value: Some(self.server), note: "" },
            Phase { label: "TRANSFER", value: Some(self.transfer), note: "" },
        ]
    }

    /// Number of filled cells for `duration` on a `width`-wide bar, scaled
    /// against `max_duration`. Saturates instead of overflowing and never
    /// divides by zero.
    pub fn bar_cells(duration: Duration, max_duration: Duration, width: usize) -> usize {
        let max_nanos = max_duration.as_nanos();
        if max_nanos == 0 || width == 0 {
            return 0;
        }
        let ratio = duration.as_nanos() as f64 / max_nanos as f64;
        let cells = (ratio * width as f64).round();
        if !cells.is_finite() || cells <= 0.0 {
            // A non-zero phase should still show a sliver rather than vanish.
            usize::from(duration.as_nanos() > 0)
        } else {
            (cells as usize).min(width)
        }
    }

    pub fn render_bar(&self, duration: Duration, max_duration: Duration, width: usize) -> String {
        let filled = Self::bar_cells(duration, max_duration, width);
        let mut s = String::with_capacity(width * 3);
        for _ in 0..filled {
            s.push('█');
        }
        for _ in 0..width.saturating_sub(filled) {
            s.push('░');
        }
        s
    }
}

/// Human-readable duration that keeps precision for fast responses.
pub fn fmt_duration(d: Duration) -> String {
    let micros = d.as_micros();
    if micros < 1_000 {
        format!("{micros}µs")
    } else if micros < 1_000_000 {
        format!("{:.1}ms", micros as f64 / 1_000.0)
    } else {
        format!("{:.2}s", micros as f64 / 1_000_000.0)
    }
}

/// Human-readable byte size.
pub fn fmt_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < MB {
        format!("{:.1} KB", b / KB)
    } else if b < GB {
        format!("{:.2} MB", b / MB)
    } else {
        format!("{:.2} GB", b / GB)
    }
}

/// Throughput over the transfer window, e.g. "4.2 MB/s".
pub fn fmt_throughput(bytes: u64, over: Duration) -> Option<String> {
    let secs = over.as_secs_f64();
    if bytes == 0 || secs <= 0.0 {
        return None;
    }
    Some(format!("{}/s", fmt_bytes((bytes as f64 / secs) as u64)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_never_exceeds_width_or_underflows() {
        // duration > max must saturate, not panic or overflow the empty half.
        let bar = MissionTelemetry::default()
            .render_bar(Duration::from_secs(10), Duration::from_secs(1), 20);
        assert_eq!(bar.chars().count(), 20);
        assert!(bar.chars().all(|c| c == '█'));
    }

    #[test]
    fn zero_max_duration_is_safe() {
        assert_eq!(MissionTelemetry::bar_cells(Duration::ZERO, Duration::ZERO, 20), 0);
        let bar = MissionTelemetry::default().render_bar(Duration::ZERO, Duration::ZERO, 20);
        assert_eq!(bar.chars().count(), 20);
    }

    #[test]
    fn tiny_phase_still_visible() {
        let cells =
            MissionTelemetry::bar_cells(Duration::from_micros(1), Duration::from_secs(5), 20);
        assert_eq!(cells, 1, "a non-zero phase must not render as empty");
    }

    #[test]
    fn duration_formatting_keeps_precision() {
        assert_eq!(fmt_duration(Duration::from_micros(250)), "250µs");
        assert_eq!(fmt_duration(Duration::from_micros(1_500)), "1.5ms");
        assert_eq!(fmt_duration(Duration::from_millis(2_500)), "2.50s");
    }

    #[test]
    fn skipped_phases_explain_themselves() {
        let reused = MissionTelemetry {
            reused_connection: true,
            ..Default::default()
        };
        assert_eq!(reused.phases()[0].note, "connection reused");

        let literal_ip = MissionTelemetry {
            connect: Some(Duration::from_millis(1)),
            reused_connection: false,
            ..Default::default()
        };
        assert!(literal_ip.phases()[0].note.contains("literal IP"));
    }

    #[test]
    fn byte_formatting() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(2048), "2.0 KB");
        assert_eq!(fmt_bytes(5 * 1024 * 1024), "5.00 MB");
    }
}
