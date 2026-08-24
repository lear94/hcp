//! Terminal clipboard access via OSC 52.
//!
//! This works over SSH and inside multiplexers without linking an X11/Wayland
//! clipboard library, which keeps `hcp` a single static binary.

use std::io::Write;

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Terminals commonly refuse very large OSC 52 payloads, so the copy is capped
/// and the caller is told how much actually made it across.
pub const MAX_COPY_BYTES: usize = 512 * 1024;

pub struct CopyOutcome {
    pub copied_bytes: usize,
    pub truncated: bool,
}

pub fn copy(text: &str) -> std::io::Result<CopyOutcome> {
    let mut bytes = text.as_bytes();
    let mut truncated = false;
    if bytes.len() > MAX_COPY_BYTES {
        // Cut on a char boundary so the clipboard never gets invalid UTF-8.
        let mut end = MAX_COPY_BYTES;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        bytes = &text.as_bytes()[..end];
        truncated = true;
    }

    let payload = base64(bytes);
    let mut out = std::io::stdout();
    // `c` targets the CLIPBOARD selection; BEL terminator has the widest support.
    write!(out, "\x1b]52;c;{payload}\x07")?;
    out.flush()?;

    Ok(CopyOutcome {
        copied_bytes: bytes.len(),
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_handles_high_bytes() {
        assert_eq!(base64(&[0xff, 0xfe, 0xfd]), "//79");
        assert_eq!(base64("é".as_bytes()), "w6k=");
    }
}
