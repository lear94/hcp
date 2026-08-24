//! Hand-rolled argument parsing.
//!
//! Deliberately dependency-free: the surface is small, and a TUI binary that
//! people `cargo install` benefits more from a fast build than from a parser
//! framework.

use crate::engine::HttpMethod;
use anyhow::{anyhow, bail, Result};

pub const HELP: &str = concat!(
    "hcp ",
    env!("CARGO_PKG_VERSION"),
    " — http-cockpit, mission control for your APIs.

USAGE:
    hcp [OPTIONS] [URL]

ARGS:
    <URL>    Endpoint to preload into the cockpit. A missing scheme is filled
             in automatically (https://, or http:// for localhost).

OPTIONS:
    -X, --method <METHOD>       GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS
    -H, --header <'K: V'>       Add a header. Repeatable.
    -d, --data <BODY>           Request body. Use @path to read from a file,
                                or @- to read from stdin.
    -t, --timeout <SECS>        Total request timeout        [default: 30]
        --connect-timeout <S>   Connection timeout           [default: 10]
        --max-body <MB>         Response bytes to keep       [default: 32]
    -k, --insecure              Skip TLS certificate verification
        --no-redirects          Do not follow 3xx redirects
        --no-mouse              Disable mouse capture (frees terminal selection)
        --no-wrap               Start with response wrapping off
        --no-pretty             Do not pretty-print JSON responses
    -s, --send                  Fire the request as soon as the cockpit opens
        --print-config-path     Show where settings and history are stored
    -h, --help                  Print this help
    -V, --version               Print version

KEYS (inside the cockpit):
    Tab / Shift+Tab   Move focus          Ctrl+S / F5   Send request
    1 / 2             Body / Headers      Esc           Cancel in-flight request
    j k g G / PgUp    Scroll response     /  n  N       Search response
    Ctrl+R            History             F2 / F3       Save / open request
    Ctrl+Y            Copy response       ?  or  F1     Full key reference
    Ctrl+Q            Quit
"
);

#[derive(Debug, Default)]
pub struct Cli {
    pub url: Option<String>,
    pub method: Option<HttpMethod>,
    pub headers: Vec<String>,
    pub body: Option<String>,
    pub timeout: Option<u64>,
    pub connect_timeout: Option<u64>,
    pub max_body_mb: Option<u64>,
    pub insecure: bool,
    pub no_redirects: bool,
    pub no_mouse: bool,
    pub no_wrap: bool,
    pub no_pretty: bool,
    pub send_now: bool,
}

/// What `main` should do once the arguments are understood.
pub enum Action {
    Run(Box<Cli>),
    PrintAndExit(String),
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Action> {
    let mut cli = Cli::default();
    let mut it = args.into_iter().peekable();

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Action::PrintAndExit(HELP.to_string())),
            "-V" | "--version" => {
                return Ok(Action::PrintAndExit(format!(
                    "hcp {}",
                    env!("CARGO_PKG_VERSION")
                )))
            }
            "--print-config-path" => {
                let dir = crate::store::data_dir()
                    .map(|d| d.display().to_string())
                    .unwrap_or_else(|| "<unavailable on this platform>".to_string());
                return Ok(Action::PrintAndExit(dir));
            }
            "-k" | "--insecure" => cli.insecure = true,
            "--no-redirects" => cli.no_redirects = true,
            "--no-mouse" => cli.no_mouse = true,
            "--no-wrap" => cli.no_wrap = true,
            "--no-pretty" => cli.no_pretty = true,
            "-s" | "--send" => cli.send_now = true,
            "-X" | "--method" => {
                let raw = next_value(&mut it, &arg)?;
                cli.method = Some(
                    HttpMethod::parse(&raw)
                        .ok_or_else(|| anyhow!("unknown HTTP method '{raw}'"))?,
                );
            }
            "-H" | "--header" => cli.headers.push(next_value(&mut it, &arg)?),
            "-d" | "--data" => {
                let raw = next_value(&mut it, &arg)?;
                cli.body = Some(read_data(&raw)?);
            }
            "-t" | "--timeout" => cli.timeout = Some(parse_secs(&next_value(&mut it, &arg)?, &arg)?),
            "--connect-timeout" => {
                cli.connect_timeout = Some(parse_secs(&next_value(&mut it, &arg)?, &arg)?)
            }
            "--max-body" => {
                let n = parse_secs(&next_value(&mut it, &arg)?, &arg)?;
                if n == 0 {
                    bail!("--max-body must be at least 1 MB");
                }
                cli.max_body_mb = Some(n);
            }
            other if other.starts_with('-') && other.len() > 1 => {
                bail!("unknown option '{other}'\n\nRun `hcp --help` for usage.");
            }
            positional => {
                if cli.url.is_some() {
                    bail!("unexpected extra argument '{positional}' — only one URL is accepted.");
                }
                cli.url = Some(positional.to_string());
            }
        }
    }

    Ok(Action::Run(Box::new(cli)))
}

fn next_value<I: Iterator<Item = String>>(
    it: &mut std::iter::Peekable<I>,
    flag: &str,
) -> Result<String> {
    it.next()
        .ok_or_else(|| anyhow!("option '{flag}' needs a value"))
}

fn parse_secs(raw: &str, flag: &str) -> Result<u64> {
    raw.trim()
        .parse::<u64>()
        .map_err(|_| anyhow!("option '{flag}' expects a whole number, got '{raw}'"))
}

/// `@path` reads a file, `@-` reads stdin, anything else is a literal body.
fn read_data(raw: &str) -> Result<String> {
    if let Some(path) = raw.strip_prefix('@') {
        if path == "-" {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| anyhow!("could not read body from stdin: {e}"))?;
            return Ok(buf);
        }
        return std::fs::read_to_string(path)
            .map_err(|e| anyhow!("could not read body file '{path}': {e}"));
    }
    Ok(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Cli> {
        match parse(args.iter().map(|s| s.to_string()))? {
            Action::Run(c) => Ok(*c),
            Action::PrintAndExit(_) => panic!("expected Run"),
        }
    }

    #[test]
    fn parses_a_full_command_line() {
        let c = parse_args(&[
            "-X", "post", "-H", "A: 1", "-H", "B: 2", "-d", "{}", "-t", "5", "-k",
            "https://x.dev",
        ])
        .unwrap();
        assert_eq!(c.method, Some(HttpMethod::Post));
        assert_eq!(c.headers, vec!["A: 1", "B: 2"]);
        assert_eq!(c.body.as_deref(), Some("{}"));
        assert_eq!(c.timeout, Some(5));
        assert!(c.insecure);
        assert_eq!(c.url.as_deref(), Some("https://x.dev"));
    }

    #[test]
    fn url_may_come_first() {
        let c = parse_args(&["example.com", "-X", "HEAD"]).unwrap();
        assert_eq!(c.url.as_deref(), Some("example.com"));
        assert_eq!(c.method, Some(HttpMethod::Head));
    }

    #[test]
    fn rejects_bad_input_with_a_useful_message() {
        assert!(parse_args(&["-X", "TELEPORT"]).is_err());
        assert!(parse_args(&["-t", "soon"]).is_err());
        assert!(parse_args(&["--nope"]).is_err());
        assert!(parse_args(&["a.com", "b.com"]).is_err());
        assert!(parse_args(&["-X"]).is_err(), "missing value must not panic");
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert!(matches!(
            parse(["--help".to_string()]).unwrap(),
            Action::PrintAndExit(_)
        ));
        assert!(matches!(
            parse(["-V".to_string()]).unwrap(),
            Action::PrintAndExit(_)
        ));
    }
}
