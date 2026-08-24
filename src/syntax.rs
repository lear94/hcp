//! Minimal, allocation-light JSON tokenizer used for response highlighting.
//!
//! It works line-by-line, which is safe because `serde_json` pretty output
//! escapes newlines inside strings — no string token ever spans two lines.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Key,
    Str,
    Number,
    Bool,
    Null,
    Punct,
    Plain,
}

/// Returns `(start, end, kind)` byte ranges covering the whole line in order.
pub fn tokenize_json_line(line: &str) -> Vec<(usize, usize, Token)> {
    let bytes = line.as_bytes();
    let mut out: Vec<(usize, usize, Token)> = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'"' => {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                let end = i.min(bytes.len());
                // A string followed by ':' is an object key.
                let mut j = end;
                while j < bytes.len() && bytes[j] == b' ' {
                    j += 1;
                }
                let kind = if j < bytes.len() && bytes[j] == b':' {
                    Token::Key
                } else {
                    Token::Str
                };
                out.push((start, end, kind));
            }
            b'{' | b'}' | b'[' | b']' | b':' | b',' => {
                out.push((i, i + 1, Token::Punct));
                i += 1;
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                i += 1;
                while i < bytes.len()
                    && (bytes[i].is_ascii_digit()
                        || matches!(bytes[i], b'.' | b'e' | b'E' | b'+' | b'-'))
                {
                    i += 1;
                }
                out.push((start, i, Token::Number));
            }
            b't' | b'f' | b'n' => {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let word = &line[start..i];
                let kind = match word {
                    "true" | "false" => Token::Bool,
                    "null" => Token::Null,
                    _ => Token::Plain,
                };
                out.push((start, i, kind));
            }
            _ => {
                let start = i;
                i += 1;
                while i < bytes.len()
                    && !matches!(
                        bytes[i],
                        b'"' | b'{' | b'}' | b'[' | b']' | b':' | b',' | b'-'
                    )
                    && !bytes[i].is_ascii_digit()
                    && !matches!(bytes[i], b't' | b'f' | b'n')
                {
                    i += 1;
                }
                out.push((start, i, Token::Plain));
            }
        }
    }

    out
}

/// Heuristic used to decide whether highlighting a payload as JSON makes sense.
pub fn looks_like_json(content_type: Option<&str>, body: &str) -> bool {
    if let Some(ct) = content_type {
        let ct = ct.to_ascii_lowercase();
        if ct.contains("json") {
            return true;
        }
        if ct.contains("text/html") || ct.contains("xml") || ct.contains("text/plain") {
            return false;
        }
    }
    let t = body.trim_start();
    t.starts_with('{') || t.starts_with('[')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str) -> Vec<(&str, Token)> {
        tokenize_json_line(line)
            .into_iter()
            .map(|(a, b, k)| (&line[a..b], k))
            .collect()
    }

    #[test]
    fn tokens_cover_the_whole_line_without_gaps() {
        let line = r#"  "name": "value","n": 42"#;
        let toks = tokenize_json_line(line);
        let mut cursor = 0;
        for (a, b, _) in &toks {
            assert_eq!(*a, cursor, "gap or overlap in tokenization");
            cursor = *b;
        }
        assert_eq!(cursor, line.len());
    }

    #[test]
    fn keys_are_distinguished_from_string_values() {
        let k = kinds(r#""a": "b""#);
        assert_eq!(k[0], (r#""a""#, Token::Key));
        assert!(k.iter().any(|&(s, t)| s == r#""b""# && t == Token::Str));
    }

    #[test]
    fn escaped_quotes_do_not_end_the_string() {
        let line = r#""a\"b": 1"#;
        let k = kinds(line);
        assert_eq!(k[0], (r#""a\"b""#, Token::Key));
    }

    #[test]
    fn literals_and_numbers() {
        let k = kinds("true, null, -1.5e3");
        assert!(k.iter().any(|&(s, t)| s == "true" && t == Token::Bool));
        assert!(k.iter().any(|&(s, t)| s == "null" && t == Token::Null));
        assert!(k.iter().any(|&(s, t)| s == "-1.5e3" && t == Token::Number));
    }

    #[test]
    fn unterminated_string_does_not_panic_or_lose_bytes() {
        let line = r#""oops"#;
        let toks = tokenize_json_line(line);
        assert_eq!(toks.last().unwrap().1, line.len());
    }

    #[test]
    fn json_detection_respects_content_type() {
        assert!(looks_like_json(Some("application/json; charset=utf-8"), ""));
        assert!(!looks_like_json(Some("text/html"), "{not really}"));
        assert!(looks_like_json(None, "  [1,2]"));
        assert!(!looks_like_json(None, "hello"));
    }
}
