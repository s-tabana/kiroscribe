//! Server-Sent Events のパーサ。
//!
//! 受信データはバイト列のまま貯めてからイベント単位で UTF-8 に変換する
//! （日本語などの多バイト文字がチャンクの境目で分かれても壊れないように）。

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Default)]
pub struct SseParser {
    buffer: Vec<u8>,
}

fn find_boundary(buf: &[u8]) -> Option<(usize, usize)> {
    // イベントの区切りは空行（\n\n または \r\n\r\n）
    for i in 0..buf.len() {
        if buf[i] == b'\n' {
            if buf.get(i + 1) == Some(&b'\n') {
                return Some((i, 2));
            }
            if buf.get(i + 1) == Some(&b'\r') && buf.get(i + 2) == Some(&b'\n') {
                return Some((i, 3));
            }
        }
    }
    None
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some((end, sep)) = find_boundary(&self.buffer) {
            let block: Vec<u8> = self.buffer.drain(..end + sep).take(end).collect();
            if let Some(event) = parse_block(&String::from_utf8_lossy(&block)) {
                events.push(event);
            }
        }
        events
    }
}

fn parse_block(block: &str) -> Option<SseEvent> {
    let mut event = None;
    let mut data = Vec::new();
    for line in block.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => event = Some(value.to_string()),
            "data" => data.push(value.to_string()),
            _ => {}
        }
    }
    if event.is_none() && data.is_empty() {
        return None;
    }
    Some(SseEvent {
        event,
        data: data.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_split_across_chunks() {
        let raw = "event: message_start\ndata: {\"a\":1}\n\nevent: content_block_delta\ndata: {\"text\":\"こんにちは\"}\n\n";
        let bytes = raw.as_bytes();
        let mut parser = SseParser::default();
        let mut events = Vec::new();
        // 1 バイトずつ入れても、多バイト文字が壊れない
        for b in bytes {
            events.extend(parser.push(std::slice::from_ref(b)));
        }
        assert_eq!(
            events,
            vec![
                SseEvent {
                    event: Some("message_start".into()),
                    data: "{\"a\":1}".into()
                },
                SseEvent {
                    event: Some("content_block_delta".into()),
                    data: "{\"text\":\"こんにちは\"}".into()
                },
            ]
        );
    }

    #[test]
    fn handles_crlf_comments_and_multiline_data() {
        let mut parser = SseParser::default();
        let events = parser.push(b": keep-alive\r\n\r\nevent: x\r\ndata: a\r\ndata: b\r\n\r\npartial");
        assert_eq!(
            events,
            vec![SseEvent {
                event: Some("x".into()),
                data: "a\nb".into()
            }]
        );
        // 区切りが来るまでは返さない
        assert!(parser.push(b" event").is_empty());
    }
}
