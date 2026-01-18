use crate::app::HttpMethod;
use crate::telemetry::MissionTelemetry;
use anyhow::{Context, Result};
use futures_util::StreamExt;
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue},
    Client, Method,
};
use std::str::FromStr;
use std::time::Instant;

#[derive(Clone)]
pub struct NetworkEngine {
    client: Client,
}

impl NetworkEngine {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .user_agent("hcp/PRO-1.0")
            .danger_accept_invalid_certs(true)
            .build()?;
        Ok(Self { client })
    }

    pub async fn execute_mission(
        &self,
        method: HttpMethod,
        url: &str,
        body: Option<String>,
        raw_headers: String,
    ) -> Result<(MissionTelemetry, String)> {
        let t0 = Instant::now();

        let mut header_map = HeaderMap::new();
        for line in raw_headers.lines() {
            if let Some((k, v)) = line.split_once(':') {
                let key = k.trim();
                let val = v.trim();
                if !key.is_empty() {
                    if let (Ok(h_name), Ok(h_val)) =
                        (HeaderName::from_str(key), HeaderValue::from_str(val))
                    {
                        header_map.insert(h_name, h_val);
                    }
                }
            }
        }

        let req_method = match method {
            HttpMethod::GET => Method::GET,
            HttpMethod::POST => Method::POST,
            HttpMethod::PUT => Method::PUT,
            HttpMethod::DELETE => Method::DELETE,
        };

        let mut request_builder = self.client.request(req_method, url).headers(header_map);

        if let Some(payload) = body {
            request_builder = request_builder.body(payload);
        }

        let response = request_builder.send().await.context("Failed to connect")?;

        let t_ttfb = t0.elapsed();
        let status = response.status().as_u16();

        let mut size_bytes = 0;
        let mut full_body = Vec::new();
        let mut stream = response.bytes_stream();
        let t_transfer_start = Instant::now();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("Stream error")?;
            size_bytes += chunk.len() as u64;
            full_body.extend_from_slice(&chunk);
        }

        let t_transfer = t_transfer_start.elapsed();
        let total = t0.elapsed();

        let body_string = String::from_utf8_lossy(&full_body).to_string();

        let formatted_body =
            if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(&body_string) {
                serde_json::to_string_pretty(&json_val).unwrap_or(body_string)
            } else {
                body_string
            };

        Ok((
            MissionTelemetry {
                dns_handshake_ttfb: t_ttfb,
                transfer: t_transfer,
                total,
                size_bytes,
                status,
            },
            formatted_body,
        ))
    }
}
