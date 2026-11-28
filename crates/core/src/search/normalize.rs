//! 検索のための文字の正規化（英字の大文字と小文字、全角と半角を区別しない）。

/// 半角カタカナ（U+FF66〜U+FF9D）に対応する全角カタカナ。
const HALFWIDTH_KATAKANA: &str = "ヲァィゥェォャュョッーアイウエオカキクケコサシスセソタチツテトナニヌネノハヒフヘホマミムメモヤユヨラリルレロワン";

fn halfwidth_katakana(c: char) -> Option<char> {
    let code = c as u32;
    (0xFF66..=0xFF9D).contains(&code).then(|| HALFWIDTH_KATAKANA.chars().nth((code - 0xFF66) as usize).unwrap())
}

/// 濁点・半濁点を付けた文字（なければ None）。
fn with_mark(base: char, mark: char) -> Option<char> {
    let voiced = mark == 'ﾞ';
    let code = base as u32;
    match base {
        'ウ' if voiced => Some('ヴ'),
        // カ〜ト（濁点）: カ キ ク ケ コ サ シ ス セ ソ タ チ ツ テ ト は、次の文字が濁音
        'カ' | 'キ' | 'ク' | 'ケ' | 'コ' | 'サ' | 'シ' | 'ス' | 'セ' | 'ソ' | 'タ' | 'チ' | 'ツ' | 'テ' | 'ト' if voiced => {
            char::from_u32(code + 1)
        }
        'ハ' | 'ヒ' | 'フ' | 'ヘ' | 'ホ' => char::from_u32(if voiced { code + 1 } else { code + 2 }),
        _ => None,
    }
}

/// 1 文字の正規化（全角英数・記号は半角に、英大文字は小文字に、全角の空白は半角に、半角カタカナは全角に）。
pub(crate) fn fold(c: char) -> char {
    let c = match c as u32 {
        0xFF01..=0xFF5E => char::from_u32(c as u32 - 0xFEE0).unwrap(),
        0x3000 => ' ',
        _ => halfwidth_katakana(c).unwrap_or(c),
    };
    if c.is_ascii_uppercase() {
        c.to_ascii_lowercase()
    } else {
        c
    }
}

/// 正規化した文字列と、その各文字が元の文字列の何文字目から来たか。
pub fn normalize_with_map(text: &str) -> (String, Vec<usize>) {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut map = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let folded = fold(chars[i]);
        // 半角の濁点・半濁点は、前の文字と合わせて 1 文字にする（ｶﾞ → ガ）
        if halfwidth_katakana(chars[i]).is_some() {
            if let Some(&mark) = chars.get(i + 1) {
                if mark == 'ﾞ' || mark == 'ﾟ' {
                    if let Some(joined) = with_mark(folded, mark) {
                        out.push(joined);
                        map.push(i);
                        i += 2;
                        continue;
                    }
                }
            }
        }
        out.push(match folded {
            'ﾞ' => '゛',
            'ﾟ' => '゜',
            other => other,
        });
        map.push(i);
        i += 1;
    }
    (out, map)
}

/// 検索のための正規化。
pub fn normalize(text: &str) -> String {
    normalize_with_map(text).0
}

#[cfg(test)]
mod tests {
    use super::*;

    // [IDX-013]
    #[test]
    fn case_and_width_are_folded() {
        assert_eq!(normalize("ＡＢＣ　Wiki"), "abc wiki");
        assert_eq!(normalize("ｹｲﾋｾｲｻﾝ"), "ケイヒセイサン");
        assert_eq!(normalize("ｶﾞｲﾄﾞ ﾊﾟｽ"), "ガイド パス");
        assert_eq!(normalize("経費精算のマニュアル"), "経費精算のマニュアル");
        let (text, map) = normalize_with_map("aｶﾞb");
        assert_eq!(text, "aガb");
        assert_eq!(map, vec![0, 1, 3]);
    }
}
