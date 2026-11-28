//! 文の中の URL の、クエリ（「?」から後ろ）とフラグメント（「#」から後ろ）の範囲。
//!
//! 抜粋は、本文から切り出す前に範囲を求める（抜粋の端で URL が切れても、残りの部分を見落とさないように）。
//! 英字の大文字と小文字、全角と半角は区別しない（`HTTPS://`・`ｈｔｔｐｓ：／／` も URL として扱う）。

use super::normalize::fold;

const SCHEMES: [&str; 2] = ["https://", "http://"];

/// URL の終わりとみなす文字（空白・ASCII の二重引用符と山括弧・全角の括弧と鉤括弧）。
/// 日本語などの ASCII でない文字とアポストロフィは、URL のパスやクエリの値にも現れるため、URL の一部として扱う。
/// そのため「https://a.jp/x?k=1を見る」のように文が続くときは、URL とその後ろの文の境界を判別できない。
/// 送るための文字列では、クエリとフラグメントを、その後ろの文ごと保守的に省く（クエリの値を送らないように）。
fn ends_url(c: char) -> bool {
    if matches!(
        c,
        '（' | '）' | '［' | '］' | '｛' | '｝' | '「' | '」' | '『' | '』' | '【' | '】' | '〈' | '〉' | '《' | '》' | '〔' | '〕'
    ) {
        return true;
    }
    let c = fold(c);
    c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>')
}

/// `chars[at..]` が URL の始まりなら、スキームの文字数。
fn scheme_at(chars: &[char], at: usize) -> Option<usize> {
    SCHEMES.iter().find_map(|scheme| {
        let len = scheme.chars().count();
        let head = chars.get(at..at + len)?;
        head.iter().zip(scheme.chars()).all(|(&c, s)| fold(c) == s).then_some(len)
    })
}

/// 送らない範囲（URL のクエリとフラグメント）。文字の位置で、始まりと終わり（終わりは含まない）。
pub fn query_spans(chars: &[char]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let Some(scheme) = scheme_at(chars, i) else {
            i += 1;
            continue;
        };
        let start = i;
        let mut end = start + scheme;
        while end < chars.len() && !ends_url(chars[end]) {
            end += 1;
        }
        if let Some(cut) = (start + scheme..end).find(|&j| matches!(fold(chars[j]), '?' | '#')) {
            spans.push((cut, end));
        }
        i = end.max(start + 1);
    }
    spans
}

/// `chars[from..to]` から、送らない範囲を除いた文字列。
pub fn without_spans(chars: &[char], from: usize, to: usize, spans: &[(usize, usize)]) -> String {
    (from..to).filter(|&i| !spans.iter().any(|&(s, e)| s <= i && i < e)).map(|i| chars[i]).collect()
}

/// 文の中の URL から、クエリとフラグメントを除く。
pub fn strip_queries(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let spans = query_spans(&chars);
    without_spans(&chars, 0, chars.len(), &spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    // [CMP-043]
    #[test]
    fn queries_are_found_regardless_of_case_and_width() {
        assert_eq!(strip_queries("見て https://a.example.com/x?y=1#z と http://b.example.com/p#frag。"), "見て https://a.example.com/x と http://b.example.com/p");
        assert_eq!(strip_queries("HTTPS://A.example.com/x?token=1 と Http://b/c#d"), "HTTPS://A.example.com/x と Http://b/c");
        assert_eq!(strip_queries("ｈｔｔｐｓ：／／ａ．ｊｐ／ｘ？ｔｏｋｅｎ＝１ を見る"), "ｈｔｔｐｓ：／／ａ．ｊｐ／ｘ を見る");
        assert_eq!(strip_queries("URL なし。?や#は残す"), "URL なし。?や#は残す");
        assert_eq!(strip_queries("（https://a.jp/x?k=1）"), "（https://a.jp/x）");
        assert_eq!(strip_queries("「https://a.jp/x#y」を開く"), "「https://a.jp/x」を開く");
        assert_eq!(strip_queries("<https://a.jp/x#y>"), "<https://a.jp/x>");
    }

    /// 日本語のパス・クエリの値、アポストロフィを含む URL も、クエリとフラグメントを残さない。
    /// URL と後ろの文の境界を判別できないときは、後ろの文ごと省く。
    // [CMP-043]
    #[test]
    fn queries_after_non_ascii_or_apostrophes_are_dropped() {
        assert_eq!(strip_queries("https://example.test/資料?token=EXAMPLE"), "https://example.test/資料");
        assert_eq!(strip_queries("https://example.test/x?q=東京&token=EXAMPLE を見る"), "https://example.test/x を見る");
        assert_eq!(strip_queries("https://example.test/x?q=it's&token=EXAMPLE"), "https://example.test/x");
        assert_eq!(strip_queries("https://example.test/x?k=1を見る。次も"), "https://example.test/x");
        assert_eq!(strip_queries("https://example.test/議事録#token=EXAMPLE、以上"), "https://example.test/議事録");
        for text in ["https://example.test/資料?token=EXAMPLE", "x https://example.test/a?q=日本&token=EXAMPLE。", "https://example.test/x?q=it's&token=EXAMPLE"] {
            assert!(!strip_queries(text).contains("EXAMPLE"), "{text}");
        }
    }
}
