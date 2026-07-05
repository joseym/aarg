//! Byte-buffer line draining shared by the two local stream parsers.
//!
//! Both local providers stream line-oriented bodies — the OpenAI dialect sends
//! `data:` SSE lines, Ollama sends NDJSON — and HTTP hands both back as
//! arbitrary byte chunks that can split a line anywhere. Each client
//! accumulates chunks in a buffer and pulls complete lines off the front; the
//! draining itself is identical, so it lives here once.

/// Pull every complete line off the front of `buffer`, returning them without
/// their trailing newline. A partial final line stays in the buffer until more
/// bytes arrive; `\r\n` endings are tolerated.
pub fn drain_lines(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut lines = Vec::new();
    while let Some(pos) = buffer.iter().position(|&byte| byte == b'\n') {
        let line: Vec<u8> = buffer.drain(..=pos).collect();
        let text = String::from_utf8_lossy(&line);
        lines.push(
            text.trim_end_matches('\n')
                .trim_end_matches('\r')
                .to_string(),
        );
    }
    lines
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn drain_lines_handles_split_and_crlf_lines() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(b"data: {\"a\"");
        assert!(drain_lines(&mut buffer).is_empty());
        buffer.extend_from_slice(b": 1}\n\r\ndata: {\"b\": 2}\r\n");
        let lines = drain_lines(&mut buffer);
        assert_eq!(lines, vec![r#"data: {"a": 1}"#, "", r#"data: {"b": 2}"#]);
        assert!(buffer.is_empty());
    }

    #[test]
    fn a_trailing_partial_line_stays_buffered() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(b"{\"done\": true}\n{\"par");
        assert_eq!(drain_lines(&mut buffer), vec![r#"{"done": true}"#]);
        assert_eq!(buffer, b"{\"par");
    }
}
