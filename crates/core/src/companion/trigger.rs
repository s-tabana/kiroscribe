//! 発言が、資料を求めていそうな候補かどうかを、PC の中だけで決める（AI は呼ばない）。

use crate::search::normalize::normalize;

/// 候補にする発言の長さの下限（文字）。
pub const MIN_CHARS: usize = 5;

/// 文末がこれで終わると「問いや依頼の形」。
const ASK_ENDINGS: &[&str] = &["?", "？", "か", "っけ", "よね"];
/// これを含むと「問いや依頼の形」。
const ASK_WORDS: &[&str] = &["送って", "貼って", "共有して", "教えて", "もらえ", "ありますか", "どこ", "どれ"];
/// 「資料や場所を指す語」。
const MATERIAL_WORDS: &[&str] = &[
    "資料", "マニュアル", "手順", "ドキュメント", "スライド", "議事録", "ページ", "ファイル", "シート", "Wiki", "URL",
    "ユーアールエル", "リンク", "パス", "フォルダ", "場所", "どこ", "どれ", "前回", "この前", "先週", "先月", "決めた",
    "決まった",
];

/// 「問いや依頼の形」か。
pub fn asks(text: &str) -> bool {
    let end = text.trim_end_matches(|c: char| c == '。' || c == '．' || c == '.' || c.is_whitespace());
    ASK_ENDINGS.iter().any(|ending| end.ends_with(ending)) || ASK_WORDS.iter().any(|word| text.contains(word))
}

/// 「資料や場所を指す語」を含むか（英字の大文字と小文字、全角と半角は区別しない）。
pub fn points_to_material(text: &str) -> bool {
    let text = normalize(text);
    MATERIAL_WORDS.iter().any(|word| text.contains(&normalize(word)))
}

/// 判定（AI）に回す候補の発言か。
pub fn is_candidate(text: &str) -> bool {
    let text = text.trim();
    text.chars().count() >= MIN_CHARS && asks(text) && points_to_material(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    // [CMP-003]
    #[test]
    fn candidates_need_both_an_ask_and_a_material_word() {
        assert!(is_candidate("経費精算のマニュアルってどこにありますか"));
        assert!(is_candidate("前回決めたスケジュール、共有してもらえますか。"));
        assert!(!is_candidate("マニュアルを更新しました。"), "問いの形がない");
        assert!(!is_candidate("今日の天気はどうですか？"), "資料や場所を指す語がない");
        assert!(!is_candidate("資料？"), "5 文字未満");
        assert!(is_candidate("その資料は？"));
    }

    // [CMP-004]
    #[test]
    fn ask_forms() {
        for text in ["あれですか", "そうだっけ", "決まってたよね", "これ何?", "これ何？", "ありましたか。", "ありましたか． ", "これですか ."] {
            assert!(asks(text), "{text}");
        }
        for text in ["送ってください", "ここに貼って", "共有してほしい", "教えて", "見せてもらえる", "ありますか", "どこだろう", "どれだったかな"] {
            assert!(asks(text), "{text}");
        }
        for text in ["了解です", "これで決まりです。", "かかります"] {
            assert!(!asks(text), "{text}");
        }
        assert!(asks("か"), "文末が「か」");
    }

    // [CMP-005]
    #[test]
    fn material_words_ignore_case_and_width() {
        for word in MATERIAL_WORDS {
            assert!(points_to_material(&format!("あの{word}は")), "{word}");
        }
        assert!(points_to_material("社内のｗｉｋｉ"), "全角の小文字");
        assert!(points_to_material("WIKI"), "大文字");
        assert!(points_to_material("url"));
        assert!(points_to_material("ﾌｧｲﾙ"), "半角カタカナ");
        assert!(!points_to_material("天気の話"));
    }
}
