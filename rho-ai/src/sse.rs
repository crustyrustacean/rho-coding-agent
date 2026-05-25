//! Server-Sent Events (SSE) parser.
//!
//! Converts a raw byte stream from `reqwest` into a stream of `data:` payloads.
//! Used by all three provider modules to consume SSE responses.

/// A parsed SSE event containing the data payload.
#[derive(Debug, Clone)]
pub struct SseEvent {
    /// The raw data field (everything after `data: ` and before `\n`).
    pub data: String,
}

/// State machine for accumulating partial SSE bytes into complete events.
#[derive(Debug, Default)]
pub(crate) struct SseParser {
    /// Accumulated bytes for the current line.
    buffer: String,
}

impl SseParser {
    /// Create a new parser.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Feed bytes into the parser, yielding complete SSE events.
    ///
    /// Handles the `data:` field and `[DONE]` sentinel.
    /// Skips comment lines (`: keepalive`) and other SSE fields.
    pub(crate) fn feed(&mut self, chunk: &str) -> Vec<SseEvent> {
        let mut events = Vec::new();
        self.buffer.push_str(chunk);

        while let Some(newline_pos) = self.buffer.find('\n') {
            let line = self.buffer[..newline_pos]
                .trim_end_matches('\r')
                .to_string();
            self.buffer = self.buffer[newline_pos + 1..].to_string();

            if line.is_empty() {
                continue;
            }

            if line.starts_with(':') {
                continue;
            }

            if let Some(data) = line.strip_prefix("data: ") {
                events.push(SseEvent {
                    data: data.to_string(),
                });
            } else if let Some(data) = line.strip_prefix("data:") {
                events.push(SseEvent {
                    data: data.trim_start().to_string(),
                });
            }
        }

        events
    }
}

/// Parse a chunk of SSE text into events (for testing / non-streaming use).
#[cfg(test)]
pub fn parse_sse_chunk(text: &str) -> Vec<SseEvent> {
    let mut parser = SseParser::new();
    parser.feed(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_event() {
        let events = parse_sse_chunk("data: hello\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn multiple_events() {
        let events = parse_sse_chunk("data: first\n\ndata: second\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "first");
        assert_eq!(events[1].data, "second");
    }

    #[test]
    fn data_without_space() {
        let events = parse_sse_chunk("data:hello\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn data_without_space_strips_leading_space() {
        let events = parse_sse_chunk("data:  hello\n\n");
        assert_eq!(events.len(), 1);
        // `data:` without space strips only the first space (colon-space convention)
        assert_eq!(events[0].data, " hello");
    }

    #[test]
    fn comment_lines_ignored() {
        let events = parse_sse_chunk(": keepalive\ndata: hello\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn split_chunks() {
        let mut parser = SseParser::new();
        let e1 = parser.feed("data: hel");
        assert!(e1.is_empty());
        let e2 = parser.feed("lo world\n\n");
        assert_eq!(e2.len(), 1);
        assert_eq!(e2[0].data, "hello world");
    }

    #[test]
    fn done_sentinel() {
        let events = parse_sse_chunk("data: [DONE]\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "[DONE]");
    }

    #[test]
    fn crlf_line_endings() {
        let events = parse_sse_chunk("data: hello\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn empty_data_field() {
        let events = parse_sse_chunk("data: \n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "");
    }

    #[test]
    fn json_payload() {
        let input = "data: {\"id\":\"1\",\"text\":\"hi\"}\n\n";
        let events = parse_sse_chunk(input);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, r#"{"id":"1","text":"hi"}"#);
    }
}
