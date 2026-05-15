//! Incremental line decoder for streamed HTTP bodies.
//!
//! HTTP body chunks do not arrive aligned to line boundaries. A
//! single TCP segment may carry the start of one SSE line and the
//! middle of another, or split a line in two. To extract complete
//! lines, the consumer must buffer incoming bytes and emit lines
//! only when a newline appears.
//!
//! [`LineDecoder`] is a small stateful buffer that takes byte chunks
//! through [`LineDecoder::push`] and yields complete lines through
//! [`LineDecoder::next_line`]. It is pure: no I/O, no async.
//! Everything testable about line framing is testable here without a
//! network.

/// A stateful buffer that turns arbitrarily-chunked bytes into
/// complete lines.
///
/// Bytes are appended with [`push`](Self::push). Complete lines —
/// the portions ending in `\n` — are returned one at a time by
/// [`next_line`](Self::next_line). A trailing partial line (bytes
/// pushed after the last `\n`) remains buffered until either more
/// bytes complete it or the caller signals end-of-stream with
/// [`finish`](Self::finish).
#[derive(Debug, Default)]
pub struct LineDecoder {
    buf: Vec<u8>,
}

impl LineDecoder {
    /// Creates an empty decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends bytes to the internal buffer. Does not parse.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Returns the next complete line, if any, as a `String`.
    ///
    /// A complete line is the bytes up to (but not including) the
    /// next `\n` in the buffer. The newline itself and the bytes
    /// before it are removed from the buffer. A trailing `\r`, if
    /// present (CRLF endings), is trimmed from the returned line.
    ///
    /// Returns `Ok(None)` when no complete line is buffered yet.
    /// Returns `Err` if the buffered bytes are not valid UTF-8 up
    /// to the next newline — an SSE line that is not UTF-8 is a
    /// protocol error.
    pub fn next_line(&mut self) -> Result<Option<String>, std::str::Utf8Error> {
        let newline_pos = match self.buf.iter().position(|&b| b == b'\n') {
            Some(pos) => pos,
            None => return Ok(None),
        };

        let line_bytes: Vec<u8> = self.buf.drain(..=newline_pos).collect();
        // Strip the trailing newline and any preceding CR.
        let line_slice = &line_bytes[..line_bytes.len() - 1];
        let line_slice = line_slice.strip_suffix(b"\r").unwrap_or(line_slice);

        let s = std::str::from_utf8(line_slice)?;
        Ok(Some(s.to_string()))
    }

    /// Returns the final partial line, if any.
    ///
    /// Called once when the stream has ended. If bytes remain in
    /// the buffer after the last newline (or if no newline ever
    /// arrived), those bytes are returned as one last line.
    /// Returns `Ok(None)` if the buffer is empty.
    pub fn finish(&mut self) -> Result<Option<String>, std::str::Utf8Error> {
        if self.buf.is_empty() {
            return Ok(None);
        }
        let remaining: Vec<u8> = std::mem::take(&mut self.buf);
        let slice = remaining.strip_suffix(b"\r").unwrap_or(&remaining);
        let s = std::str::from_utf8(slice)?;
        Ok(Some(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_decoder_yields_no_line() {
        let mut d = LineDecoder::new();
        assert_eq!(d.next_line().unwrap(), None);
    }

    #[test]
    fn one_complete_line_is_yielded() {
        let mut d = LineDecoder::new();
        d.push(b"hello\n");
        assert_eq!(d.next_line().unwrap(), Some("hello".to_string()));
        assert_eq!(d.next_line().unwrap(), None);
    }

    #[test]
    fn two_lines_in_one_push_are_yielded_in_order() {
        let mut d = LineDecoder::new();
        d.push(b"first\nsecond\n");
        assert_eq!(d.next_line().unwrap(), Some("first".to_string()));
        assert_eq!(d.next_line().unwrap(), Some("second".to_string()));
        assert_eq!(d.next_line().unwrap(), None);
    }

    #[test]
    fn a_line_split_across_pushes_is_reassembled() {
        let mut d = LineDecoder::new();
        d.push(b"hel");
        assert_eq!(d.next_line().unwrap(), None);
        d.push(b"lo\n");
        assert_eq!(d.next_line().unwrap(), Some("hello".to_string()));
    }

    #[test]
    fn pathological_byte_at_a_time_still_reassembles() {
        let mut d = LineDecoder::new();
        for byte in b"streamed\n" {
            d.push(std::slice::from_ref(byte));
        }
        assert_eq!(d.next_line().unwrap(), Some("streamed".to_string()));
        assert_eq!(d.next_line().unwrap(), None);
    }

    #[test]
    fn crlf_line_endings_are_handled() {
        let mut d = LineDecoder::new();
        d.push(b"hello\r\nworld\r\n");
        assert_eq!(d.next_line().unwrap(), Some("hello".to_string()));
        assert_eq!(d.next_line().unwrap(), Some("world".to_string()));
    }

    #[test]
    fn blank_line_is_yielded_as_empty_string() {
        // SSE uses blank lines as event separators; the decoder
        // must surface them, not swallow them. classify_sse_line
        // is responsible for ignoring them at the next layer.
        let mut d = LineDecoder::new();
        d.push(b"\n");
        assert_eq!(d.next_line().unwrap(), Some(String::new()));
    }

    #[test]
    fn finish_returns_trailing_partial_line() {
        let mut d = LineDecoder::new();
        d.push(b"data: [DONE]");
        assert_eq!(d.next_line().unwrap(), None);
        assert_eq!(d.finish().unwrap(), Some("data: [DONE]".to_string()));
    }

    #[test]
    fn finish_on_empty_buffer_returns_none() {
        let mut d = LineDecoder::new();
        assert_eq!(d.finish().unwrap(), None);
    }

    #[test]
    fn finish_after_complete_lines_returns_none() {
        let mut d = LineDecoder::new();
        d.push(b"one\ntwo\n");
        let _ = d.next_line().unwrap();
        let _ = d.next_line().unwrap();
        assert_eq!(d.finish().unwrap(), None);
    }

    #[test]
    fn invalid_utf8_yields_an_error() {
        let mut d = LineDecoder::new();
        d.push(&[0xff, 0xfe, b'\n']);
        assert!(d.next_line().is_err());
    }
}
