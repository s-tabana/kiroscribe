//! デバッグ用のログ。不具合（会議を検出しなかった、録音が止まった など）の原因を、後から調べるための記録。
//!
//! - アプリのデータの場所の `logs/` に、日ごとのファイル `kiroscribe-<年月日>.log` を作り、1 行に 1 つの出来事を JSON で書く
//! - [`KEEP_DAYS`] 日より前のファイルと、合計が [`MAX_TOTAL_BYTES`] を超えた分（古いものから）を消す。
//!   1 日のファイルは [`MAX_FILE_BYTES`] までで、それを超える分は書かない
//! - 「詳しい記録」（[`Level::Detail`]）は、利用者がオンにしてから [`VERBOSE_MS`] の間だけ書く
//!
//! 書かないもの: API キー・文字起こし・メモ・議事録・チャットの本文・音声・AI に送った内容と返ってきた内容。
//! 呼び出す側は、これらを値に入れない。念のため、文字列の値は [`MAX_TEXT_CHARS`] 文字で切り、
//! API キーの形の文字列・パスの中の利用者のフォルダの名前・URL のクエリを伏せてから書く（[`sanitize_text`]）。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use chrono::{DateTime, Local, NaiveDate, SecondsFormat, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::redact_api_keys;

/// ログを置くフォルダの名前（アプリのデータの場所の中）。
pub const DIR_NAME: &str = "logs";
/// ログのファイルの名前の先頭。続けて `<年月日>.log`。
pub const FILE_PREFIX: &str = "kiroscribe-";
pub const FILE_SUFFIX: &str = ".log";
/// 残す日数（今日を含む）。
pub const KEEP_DAYS: i64 = 7;
/// ログのファイルの合計の上限。
pub const MAX_TOTAL_BYTES: u64 = 20 * 1024 * 1024;
/// 1 日のファイルの上限。
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// 「詳しい記録」を続ける時間（24 時間）。
pub const VERBOSE_MS: i64 = 24 * 60 * 60 * 1000;
/// 文字列の値の長さの上限（文字数）。
pub const MAX_TEXT_CHARS: usize = 300;
/// 1 つの出来事に入れる値の数の上限（入れ子を含む）。
pub const MAX_VALUES: usize = 64;
/// 1 日のファイルが上限に達したことを書く出来事の名前。
const FULL_EVENT: &str = "log.file.full";

/// 出来事の重さ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// いつも書く出来事
    Info,
    /// 「詳しい記録」がオンのときだけ書く細かい出来事
    Detail,
    /// 失敗（いつも書く）
    Error,
}

/// 出来事が起きた場所。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// 本体（Rust）
    App,
    /// 画面（WebView）
    Ui,
}

/// ログの 1 行。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    /// 時刻（RFC 3339、PC の時刻帯）
    pub at: String,
    pub level: Level,
    pub source: Source,
    /// 出来事の名前（例: `recording.started`）
    pub event: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub fields: Map<String, Value>,
}

/// 日ごとのファイルの名前（例: `kiroscribe-20261127.log`）。
pub fn file_name(day: NaiveDate) -> String {
    format!("{FILE_PREFIX}{}{FILE_SUFFIX}", day.format("%Y%m%d"))
}

/// ログのファイルの名前から日付を読む（ログのファイルでなければ `None`）。
pub fn day_of(name: &str) -> Option<NaiveDate> {
    let digits = name.strip_prefix(FILE_PREFIX)?.strip_suffix(FILE_SUFFIX)?;
    if digits.len() != 8 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    NaiveDate::parse_from_str(digits, "%Y%m%d").ok()
}

/// 出来事の名前に使える形か（英小文字・数字・`.`・`_`・`-` で、64 文字まで）。
pub fn valid_event_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

/// パスの区切り（`\\`・`/`）か。
fn is_separator(b: u8) -> bool {
    matches!(b, b'\\' | b'/')
}

/// 利用者のフォルダの名前の終わり（区切り・引用符・改行）か。
fn ends_user_name(b: u8) -> bool {
    matches!(b, b'\\' | b'/' | b'"' | b'\'' | b'\n' | b'\r' | b'\t')
}

/// `at` の区切りから始まる「区切り + users か home + 区切り」の並びなら、利用者のフォルダの名前が始まる位置を返す。
/// 区切りは、続けて書かれていてもよい（JSON や Rust の `{:?}` で `\\` が `\\\\` になったパスも当てる）。
/// 大文字と小文字は区別しない。
fn user_name_start(bytes: &[u8], at: usize) -> Option<usize> {
    let skip_separators = |mut i: usize| {
        while i < bytes.len() && is_separator(bytes[i]) {
            i += 1;
        }
        i
    };
    if !bytes.get(at).copied().is_some_and(is_separator) {
        return None;
    }
    let root = skip_separators(at);
    let rest = &bytes[root..];
    let root_len = ["users", "home"].iter().find_map(|word| {
        (rest.len() >= word.len() && rest[..word.len()].eq_ignore_ascii_case(word.as_bytes())).then_some(word.len())
    })?;
    let after = root + root_len;
    if !bytes.get(after).copied().is_some_and(is_separator) {
        return None;
    }
    Some(skip_separators(after))
}

/// パスの中の利用者のフォルダの名前（`C:\Users\<名前>\`・`/home/<名前>/`・`/Users/<名前>/`）を `<user>` に置き換える。
/// 名前に日本語などが入っていても、区切りが `\\` と書かれたパス（JSON などの中の文字列）でも伏せる。
fn mask_user_folders(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    // `out` に写し終えた位置。区切り・引用符は ASCII なので、どの位置も文字の境目になる
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        if let Some(start) = user_name_start(bytes, i) {
            let end = bytes[start..]
                .iter()
                .position(|b| ends_user_name(*b))
                .map_or(bytes.len(), |at| start + at);
            if end > start {
                out.push_str(&text[copied..start]);
                out.push_str("<user>");
                copied = end;
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&text[copied..]);
    out
}

/// URL のクエリ（`?` から後ろ。署名つきのダウンロードの URL などに、使い捨ての鍵が入る）を `?<query>` に置き換える。
fn mask_url_queries(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let lower = rest.to_ascii_lowercase();
        let Some(scheme) = [lower.find("https://"), lower.find("http://")].into_iter().flatten().min() else {
            out.push_str(rest);
            return out;
        };
        let url_end = rest[scheme..]
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ')' | '>' | '<'))
            .map_or(rest.len(), |at| scheme + at);
        let url = &rest[scheme..url_end];
        out.push_str(&rest[..scheme]);
        match url.find('?') {
            Some(query) => {
                out.push_str(&url[..query]);
                out.push_str("?<query>");
            }
            None => out.push_str(url),
        }
        rest = &rest[url_end..];
    }
}

/// API キーの形の文字列・URL のクエリ・利用者のフォルダの名前を伏せる（長さは変えない）。
/// URL のクエリは、利用者のフォルダの名前より先に伏せる。名前は空白では終わらないため、
/// パスの後ろに URL が続くと、名前と一緒に URL の `https:` まで置き換わり、クエリを URL として見つけられなくなる。
pub fn mask_text(text: &str) -> String {
    mask_user_folders(&mask_url_queries(&redact_api_keys(text)))
}

/// ログに書く文字列を整える: [`mask_text`] で伏せ、[`MAX_TEXT_CHARS`] 文字で切る。
pub fn sanitize_text(text: &str) -> String {
    let masked = mask_text(text);
    if masked.chars().count() <= MAX_TEXT_CHARS {
        return masked;
    }
    let mut cut: String = masked.chars().take(MAX_TEXT_CHARS).collect();
    cut.push('…');
    cut
}

/// 値の中の文字列をすべて [`sanitize_text`] で整え、値の数を [`MAX_VALUES`] までにする。
/// 上限を超えるときは、浅いところの値から順に残す（深いところの細かい値だけを落とす）。
/// 例えば `meeting.check` では、マイクの使用状況の記録が多くても、判定・理由・段階と、記録を開けたか・件数は残る。
pub fn sanitize_value(value: Value) -> Value {
    let (cut_depth, kept_at_cut) = cutoff(&value, MAX_VALUES);
    let mut seen_at_cut = 0;
    prune(value, 0, cut_depth, kept_at_cut, &mut seen_at_cut).unwrap_or(Value::Null)
}

fn children(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(items) => items.iter().collect(),
        Value::Object(map) => map.values().collect(),
        _ => Vec::new(),
    }
}

/// 浅い順に数えて `budget` に収まる範囲: 途中で収まらなくなる深さと、その深さで残す値の数（左から）。
/// すべて収まるなら、いちばん深い値の次の深さと 0。
fn cutoff(value: &Value, budget: usize) -> (usize, usize) {
    let mut level = vec![value];
    let mut left = budget;
    let mut depth = 0;
    while !level.is_empty() {
        if level.len() > left {
            return (depth, left);
        }
        left -= level.len();
        level = level.into_iter().flat_map(children).collect();
        depth += 1;
    }
    (depth, 0)
}

/// [`cutoff`] で決めた範囲の値だけを残し、文字列を整える。同じ深さの値は、左から（深さ優先でたどった順に）数える。
fn prune(value: Value, depth: usize, cut_depth: usize, kept_at_cut: usize, seen_at_cut: &mut usize) -> Option<Value> {
    if depth > cut_depth {
        return None;
    }
    if depth == cut_depth {
        if *seen_at_cut >= kept_at_cut {
            return None;
        }
        *seen_at_cut += 1;
    }
    let mut child = |value: Value| prune(value, depth + 1, cut_depth, kept_at_cut, seen_at_cut);
    Some(match value {
        Value::String(text) => Value::String(sanitize_text(&text)),
        Value::Array(items) => Value::Array(items.into_iter().filter_map(&mut child).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter_map(|(key, value)| child(value).map(|value| (sanitize_text(&key), value)))
                .collect(),
        ),
        other => other,
    })
}

/// ログに書く時刻の形（RFC 3339、ミリ秒まで、PC の時刻帯）。
pub fn format_time<Tz: TimeZone>(at: &DateTime<Tz>) -> String {
    at.with_timezone(&Local).to_rfc3339_opts(SecondsFormat::Millis, false)
}

/// UNIX ミリ秒の時刻を、[`format_time`] の形にする。
pub fn format_ms(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .map(|t| format_time(&t))
        .unwrap_or_default()
}

struct Writer {
    /// いま書いているファイルの日付
    day: Option<NaiveDate>,
    /// その日のファイルの大きさ
    size: u64,
    /// その日の上限に達したことを書いたか
    capped: bool,
}

/// デバッグ用のログ。アプリに 1 つ置き、どのスレッドからでも書ける。
pub struct DebugLog {
    dir: PathBuf,
    writer: Mutex<Writer>,
    /// 「詳しい記録」を続ける期限（UNIX ミリ秒）。0 ならオフ
    verbose_until: AtomicI64,
    /// 期限が過ぎてオフに戻した「詳しい記録」の期限（UNIX ミリ秒）。0 ならなし。
    /// 呼び出す側が [`DebugLog::take_expired`] で受け取り、保存してある期限を消す
    expired: AtomicI64,
}

impl DebugLog {
    /// `dir`（ふつうはアプリのデータの場所の `logs/`）にログを書く。フォルダを作る。
    /// 古いファイルは、最初の 1 行を書くとき（と、日付が変わったとき）に消す。
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            writer: Mutex::new(Writer {
                day: None,
                size: 0,
                capped: false,
            }),
            verbose_until: AtomicI64::new(0),
            expired: AtomicI64::new(0),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 「詳しい記録」の期限を決める（`None` ならオフ）。
    pub fn set_verbose_until(&self, until_ms: Option<i64>) {
        self.verbose_until.store(until_ms.unwrap_or(0), Ordering::SeqCst);
    }

    /// 「詳しい記録」の期限（`now_ms` の時点でオフなら `None`）。
    pub fn verbose_until(&self, now_ms: i64) -> Option<i64> {
        let until = self.verbose_until.load(Ordering::SeqCst);
        (until > now_ms).then_some(until)
    }

    /// 出来事を書く（いまの時刻で）。
    pub fn write(&self, level: Level, source: Source, event: &str, fields: Value) {
        self.write_at(Local::now(), level, source, event, fields);
    }

    /// 「詳しい記録」の期限が `at` までに過ぎていたら、オフに戻し、戻したこと（`log.verbose.ended`）を
    /// `at` の日のファイルに 1 行書く。戻したなら、過ぎた期限を返す（同じ期限について 1 回だけ）。
    /// アプリを止めている間に期限が過ぎたときは、起動して期限を読み込んだあとに呼ぶ。
    pub fn expire_verbose_at<Tz: TimeZone>(&self, at: DateTime<Tz>) -> Option<i64> {
        let at = at.with_timezone(&Local);
        let mut writer = self.writer.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.expire_verbose(&mut writer, at)
    }

    fn expire_verbose(&self, writer: &mut Writer, at: DateTime<Local>) -> Option<i64> {
        let until = self.verbose_until.load(Ordering::SeqCst);
        if until == 0 || until > at.timestamp_millis() {
            return None;
        }
        // 書く人は writer の鍵を持っているので、ここで戻せるのは 1 回だけ
        self.verbose_until.store(0, Ordering::SeqCst);
        let mut fields = Map::new();
        fields.insert("until".into(), Value::String(format_ms(until)));
        self.append(
            writer,
            at,
            Line {
                at: format_time(&at),
                level: Level::Info,
                source: Source::App,
                event: "log.verbose.ended".into(),
                fields,
            },
        );
        self.expired.store(until, Ordering::SeqCst);
        Some(until)
    }

    /// 期限が過ぎてオフに戻した「詳しい記録」の期限を、1 回だけ返す（保存してある期限を消すため）。
    pub fn take_expired(&self) -> Option<i64> {
        let until = self.expired.swap(0, Ordering::SeqCst);
        (until != 0).then_some(until)
    }

    /// `at` の時刻の出来事として書く。書けなかったとき（ディスクがいっぱい など）は、何もしない。
    /// 「詳しい記録」の期限が過ぎていたら、先にオフに戻す（[`DebugLog::expire_verbose_at`]）。
    pub fn write_at<Tz: TimeZone>(&self, at: DateTime<Tz>, level: Level, source: Source, event: &str, fields: Value) {
        let at = at.with_timezone(&Local);
        let now_ms = at.timestamp_millis();
        let mut writer = self.writer.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.expire_verbose(&mut writer, at);
        if level == Level::Detail && self.verbose_until(now_ms).is_none() {
            return;
        }
        let event = if valid_event_name(event) { event.to_string() } else { "invalid-event-name".to_string() };
        let fields = match sanitize_value(fields) {
            Value::Object(map) => map,
            Value::Null => Map::new(),
            other => {
                let mut map = Map::new();
                map.insert("value".into(), other);
                map
            }
        };
        self.append(
            &mut writer,
            at,
            Line {
                at: format_time(&at),
                level,
                source,
                event,
                fields,
            },
        );
    }

    fn append(&self, writer: &mut Writer, at: DateTime<Local>, line: Line) {
        let day = at.date_naive();
        if writer.day != Some(day) {
            // 日付が変わったら（起動して最初の 1 行も）、古いファイルを消してから、その日のファイルに移る
            self.prune(day);
            writer.day = Some(day);
            let path = self.dir.join(file_name(day));
            writer.size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            // 起動し直す前に上限に達していた日は、そのまま書かない（上限に達したことを書き直さない）
            writer.capped = ends_with_full_marker(&path);
        }
        if writer.capped {
            return;
        }
        let Ok(mut text) = serde_json::to_string(&line) else {
            return;
        };
        text.push('\n');
        let line_bytes = text.len() as u64;
        if writer.size + line_bytes > MAX_FILE_BYTES {
            writer.capped = true;
            let marker = Line {
                at: line.at.clone(),
                level: Level::Info,
                source: Source::App,
                event: FULL_EVENT.into(),
                fields: Map::new(),
            };
            if let Ok(mut text) = serde_json::to_string(&marker) {
                text.push('\n');
                let _ = self.append_raw(day, &text);
            }
            return;
        }
        if self.append_raw(day, &text).is_ok() {
            writer.size += line_bytes;
        }
    }

    fn append_raw(&self, day: NaiveDate, text: &str) -> io::Result<()> {
        let mut file: File = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(file_name(day)))?;
        file.write_all(text.as_bytes())
    }

    /// ログのファイル（古い順）。
    pub fn files(&self) -> Vec<PathBuf> {
        list_files(&self.dir).into_iter().map(|(_, path, _)| path).collect()
    }

    /// `today` から数えて [`KEEP_DAYS`] 日より前のファイルを消し、合計が [`MAX_TOTAL_BYTES`] を超えるなら、
    /// 古いファイルから消す（今日のファイルは消さない）。
    pub fn prune(&self, today: NaiveDate) {
        let oldest_kept = today - chrono::Duration::days(KEEP_DAYS - 1);
        let mut kept = Vec::new();
        for (day, path, size) in list_files(&self.dir) {
            if day < oldest_kept {
                let _ = fs::remove_file(&path);
            } else {
                kept.push((day, path, size));
            }
        }
        let mut total: u64 = kept.iter().map(|(_, _, size)| size).sum();
        for (day, path, size) in kept {
            if total <= MAX_TOTAL_BYTES || day >= today {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= size;
            }
        }
    }
}

/// ファイルの最後の行が、上限に達したことを書いた行か。
fn ends_with_full_marker(path: &Path) -> bool {
    const TAIL: u64 = 512;
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).is_err() {
        return false;
    }
    let mut tail = Vec::new();
    if file.read_to_end(&mut tail).is_err() {
        return false;
    }
    let tail = String::from_utf8_lossy(&tail);
    tail.trim_end()
        .rsplit('\n')
        .next()
        .and_then(|line| serde_json::from_str::<Line>(line).ok())
        .is_some_and(|line| line.event == FULL_EVENT)
}

/// `dir` の中のログのファイル（日付・パス・大きさ）を、古い順に返す。
fn list_files(dir: &Path) -> Vec<(NaiveDate, PathBuf, u64)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(NaiveDate, PathBuf, u64)> = entries
        .flatten()
        .filter_map(|entry| {
            let day = day_of(&entry.file_name().to_string_lossy())?;
            let meta = entry.metadata().ok().filter(|m| m.is_file())?;
            Some((day, entry.path(), meta.len()))
        })
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(text: &str) -> DateTime<Local> {
        let naive = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap();
        Local.from_local_datetime(&naive).single().unwrap()
    }

    fn lines(path: &Path) -> Vec<Line> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// 日ごとのファイルに、1 行に 1 つの出来事を JSON で書く。
    // [LOG-001]
    #[test]
    fn writes_one_json_line_per_event_into_a_file_per_day() {
        let dir = tempfile::tempdir().unwrap();
        let log = DebugLog::open(dir.path().join(DIR_NAME)).unwrap();
        log.write_at(at("2026-11-26 23:59"), Level::Info, Source::App, "app.started", json!({ "appVersion": "0.4.1" }));
        log.write_at(at("2026-11-27 00:01"), Level::Error, Source::Ui, "ui.error", json!({ "message": "失敗" }));
        log.write_at(at("2026-11-27 00:02"), Level::Info, Source::App, "Bad Name!", json!(null));

        let names: Vec<String> = log
            .files()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["kiroscribe-20261126.log", "kiroscribe-20261127.log"]);
        let first = lines(&log.files()[0]);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].event, "app.started");
        assert_eq!(first[0].level, Level::Info);
        assert_eq!(first[0].source, Source::App);
        assert_eq!(first[0].fields["appVersion"], "0.4.1");
        assert!(first[0].at.starts_with("2026-11-26T23:59:00.000"), "{}", first[0].at);
        let second = lines(&log.files()[1]);
        assert_eq!(second[0].source, Source::Ui);
        assert_eq!(second[0].level, Level::Error);
        assert_eq!(second[1].event, "invalid-event-name");
        assert!(second[1].fields.is_empty());
    }

    /// 7 日より前のファイルと、合計 20MB を超えた分（古いものから）を消す。1 日のファイルは 10MB まで。
    // [LOG-002]
    #[test]
    fn old_and_oversized_files_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join(DIR_NAME);
        fs::create_dir_all(&logs).unwrap();
        let day = |d: u32| NaiveDate::from_ymd_opt(2026, 11, d).unwrap();
        // 11/20 は 7 日より前（11/27 から数えて 8 日目）。11/21〜11/23 は合わせて 20MB を超える
        fs::write(logs.join(file_name(day(20))), b"old").unwrap();
        for d in [21, 22, 23] {
            fs::write(logs.join(file_name(day(d))), vec![b'x'; 8 * 1024 * 1024]).unwrap();
        }
        fs::write(logs.join(file_name(day(26))), b"keep").unwrap();
        fs::write(logs.join("README.txt"), b"not a log").unwrap();

        let log = DebugLog::open(&logs).unwrap();
        log.prune(day(27));
        let names: Vec<String> = log
            .files()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec![file_name(day(22)), file_name(day(23)), file_name(day(26))]);
        assert!(logs.join("README.txt").exists(), "ログでないファイルは消さない");

        // 日付が変わって最初に書くときにも消す（11/22 が 7 日より前になる）
        log.write_at(at("2026-11-29 09:00"), Level::Info, Source::App, "app.started", json!({}));
        assert!(!logs.join(file_name(day(22))).exists());
        assert!(logs.join(file_name(day(23))).exists());

        // 1 日のファイルが上限に達したら、それ以上は書かず、達したことを 1 行だけ書く
        let today = logs.join(file_name(day(29)));
        let size_before = fs::metadata(&today).unwrap().len();
        let filler = "x".repeat(MAX_TEXT_CHARS);
        let per_line = serde_json::to_string(&json!({ "a": filler, "b": filler, "c": filler })).unwrap().len() as u64;
        let needed = (MAX_FILE_BYTES - size_before) / per_line + 10;
        for _ in 0..needed {
            log.write_at(
                at("2026-11-29 09:01"),
                Level::Info,
                Source::App,
                "filler",
                json!({ "a": filler, "b": filler, "c": filler }),
            );
        }
        let size = fs::metadata(&today).unwrap().len();
        assert!(size <= MAX_FILE_BYTES + 200, "{size}");
        let written = fs::read_to_string(&today).unwrap();
        assert_eq!(written.matches("log.file.full").count(), 1);
        assert!(written.trim_end().ends_with('}'));
        assert!(written.lines().last().unwrap().contains("log.file.full"));
    }

    /// 「詳しい記録」は、期限までの間だけ書き、期限が過ぎたらオフに戻して、戻したことを、そのときの日のファイルに 1 回だけ書く。
    // [LOG-010]
    #[test]
    fn detail_is_written_only_until_the_verbose_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let log = DebugLog::open(dir.path()).unwrap();
        let start = at("2026-11-27 10:00");
        log.write_at(start, Level::Detail, Source::App, "meeting.poll", json!({ "n": 1 }));
        log.set_verbose_until(Some(start.timestamp_millis() + VERBOSE_MS));
        assert_eq!(log.verbose_until(start.timestamp_millis()), Some(start.timestamp_millis() + VERBOSE_MS));
        log.write_at(start, Level::Detail, Source::App, "meeting.poll", json!({ "n": 2 }));
        log.write_at(at("2026-11-28 09:59"), Level::Detail, Source::App, "meeting.poll", json!({ "n": 3 }));
        log.write_at(at("2026-11-28 10:01"), Level::Detail, Source::App, "meeting.poll", json!({ "n": 4 }));
        log.write_at(at("2026-11-28 10:02"), Level::Info, Source::App, "app.exit", json!({}));
        assert_eq!(log.verbose_until(at("2026-11-28 10:01").timestamp_millis()), None);

        let all: Vec<Line> = log.files().iter().flat_map(|p| lines(p)).collect();
        let summary: Vec<String> = all
            .iter()
            .map(|l| match l.fields.get("n") {
                Some(n) => format!("{}:{n}", l.event),
                None => l.event.clone(),
            })
            .collect();
        assert_eq!(summary, vec!["meeting.poll:2", "meeting.poll:3", "log.verbose.ended", "app.exit"]);
        assert!(all[2].at.starts_with("2026-11-28T10:01:00.000"), "オフに戻したときの時刻で書く: {}", all[2].at);
        assert_eq!(all[2].fields["until"], format_ms(start.timestamp_millis() + VERBOSE_MS));
        assert_eq!(log.take_expired(), Some(start.timestamp_millis() + VERBOSE_MS), "保存した期限を消せるよう、過ぎた期限を渡す");
        assert_eq!(log.take_expired(), None, "渡すのは 1 回だけ");
    }

    /// アプリを止めている間に期限が過ぎたときは、起動して期限を読み込んだときに、その日のファイルに 1 回だけ書く。
    /// 期限が日付をまたいで過ぎていても、前の日のファイルには書かない。
    // [LOG-010]
    #[test]
    fn a_deadline_passed_while_stopped_is_ended_once_in_todays_file() {
        let dir = tempfile::tempdir().unwrap();
        let log = DebugLog::open(dir.path()).unwrap();
        let until = at("2026-11-26 10:00").timestamp_millis();
        log.set_verbose_until(Some(until));
        assert_eq!(log.expire_verbose_at(at("2026-11-28 08:00")), Some(until));
        assert_eq!(log.expire_verbose_at(at("2026-11-28 08:00")), None, "2 回目は書かない");
        log.write_at(at("2026-11-28 08:01"), Level::Info, Source::App, "app.started", json!({}));
        assert_eq!(log.take_expired(), Some(until));
        assert_eq!(log.verbose_until(at("2026-11-28 08:00").timestamp_millis()), None);

        let names: Vec<String> = log
            .files()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["kiroscribe-20261128.log"]);
        let written = lines(&log.files()[0]);
        let events: Vec<&str> = written.iter().map(|l| l.event.as_str()).collect();
        assert_eq!(events, vec!["log.verbose.ended", "app.started"]);
        assert!(written[0].at.starts_with("2026-11-28T08:00:00.000"), "{}", written[0].at);
        assert_eq!(written[0].fields["until"], format_ms(until));

        // 期限内なら、何もしない
        log.set_verbose_until(Some(at("2026-11-29 08:00").timestamp_millis()));
        assert_eq!(log.expire_verbose_at(at("2026-11-28 09:00")), None);
        assert_eq!(log.take_expired(), None);
    }

    /// 1 日のファイルが上限に達したあとに起動し直しても、上限に達したことを書き直さない。
    // [LOG-002]
    #[test]
    fn a_full_day_stays_full_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(file_name(NaiveDate::from_ymd_opt(2026, 11, 27).unwrap()));
        let marker = serde_json::to_string(&Line {
            at: "2026-11-27T09:00:00.000+09:00".into(),
            level: Level::Info,
            source: Source::App,
            event: FULL_EVENT.into(),
            fields: Map::new(),
        })
        .unwrap();
        let mut full = vec![b'x'; (MAX_FILE_BYTES - 100) as usize];
        full.push(b'\n');
        full.extend_from_slice(marker.as_bytes());
        full.push(b'\n');
        fs::write(&path, &full).unwrap();
        for _ in 0..2 {
            // 起動し直すたびに、ログを開き直す
            let log = DebugLog::open(dir.path()).unwrap();
            log.write_at(at("2026-11-27 10:00"), Level::Info, Source::App, "app.started", json!({}));
        }
        assert_eq!(fs::read(&path).unwrap(), full, "上限に達した日には、もう書かない");

        // 上限に達していない日は、起動し直しても書き足す
        let other = dir.path().join(file_name(NaiveDate::from_ymd_opt(2026, 11, 28).unwrap()));
        fs::write(&other, format!("{marker}\n{}\n", serde_json::to_string(&json!({"event": "x"})).unwrap())).unwrap();
        let log = DebugLog::open(dir.path()).unwrap();
        log.write_at(at("2026-11-28 10:00"), Level::Info, Source::App, "app.started", json!({}));
        assert!(fs::read_to_string(&other).unwrap().contains("app.started"));
    }

    /// 値が多すぎるときは、浅いところの値から残す。会議の検出の判定の様子なら、マイクの使用状況の記録が多くても、
    /// 判定・理由・段階と、記録を開けたか・件数は残る。
    // [LOG-005][LOG-009]
    #[test]
    fn shallow_values_are_kept_when_there_are_too_many() {
        let entry = |n: usize| {
            json!({
                "name": format!("app{n}.exe"), "nameKind": "exe", "location": "ProgramFiles", "app": null,
                "browser": false, "inUse": false, "start": "1", "stop": "2", "sinceStartSec": 3, "sinceStopSec": 4,
            })
        };
        let fields = json!({
            "micInUse": ["chrome.exe"],
            "browserWindows": 1,
            "verdict": "noMeetingTab",
            "why": "理由",
            "app": null,
            "phase": "idle",
            "phaseApp": null,
            "micStore": { "opened": true, "total": 40, "entries": (0..12).map(entry).collect::<Vec<_>>() },
        });
        let kept = sanitize_value(fields);
        for key in ["verdict", "why", "phase", "browserWindows", "micInUse"] {
            assert!(kept.get(key).is_some(), "{key} がない: {kept}");
        }
        assert_eq!(kept["verdict"], "noMeetingTab");
        assert_eq!((&kept["micStore"]["opened"], &kept["micStore"]["total"]), (&json!(true), &json!(40)));
        let entries = kept["micStore"]["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 12, "記録の数は分かる");
        assert_eq!(entries[0]["name"], "app0.exe", "先の記録は中身も残る");
        assert!(entries[11].as_object().unwrap().is_empty(), "後の記録の中身は落とす: {}", entries[11]);
        fn count(value: &Value) -> usize {
            1 + children(value).into_iter().map(count).sum::<usize>()
        }
        assert_eq!(count(&kept), MAX_VALUES);
    }

    /// API キーの形の文字列と、利用者のフォルダの名前を伏せ、長い文字列は切る。
    // [LOG-009]
    #[test]
    fn secrets_and_user_names_are_masked_and_long_text_is_cut() {
        assert_eq!(
            sanitize_text("Incorrect API key provided: sk-proj-abcdEFGH1234"),
            "Incorrect API key provided: （キーは表示しません）"
        );
        assert_eq!(
            sanitize_text(r"C:\Users\山田 太郎\AppData\Roaming\com.kiroscribe.app\logs"),
            r"C:\Users\<user>\AppData\Roaming\com.kiroscribe.app\logs"
        );
        assert_eq!(sanitize_text("/home/taro/.local/share/x"), "/home/<user>/.local/share/x");
        assert_eq!(sanitize_text("/Users/hanako/Library"), "/Users/<user>/Library");
        // 小文字にすると長さが変わる文字が前にあっても伏せる
        assert_eq!(sanitize_text(r"İ C:\USERS\taro\x"), r"İ C:\USERS\<user>\x");
        // 区切りが \\ と書かれたパス（JSON や {:?} の中）も伏せる
        assert_eq!(
            sanitize_text(r#"open "C:\\Users\\山田 太郎\\AppData\\x.db": denied"#),
            r#"open "C:\\Users\\<user>\\AppData\\x.db": denied"#
        );
        assert_eq!(sanitize_text("C:/Users/taro"), "C:/Users/<user>");
        assert_eq!(sanitize_text("/usersettings/x and /home/"), "/usersettings/x and /home/");
        // URL のクエリ（署名つきの URL の鍵など）は伏せる
        assert_eq!(
            sanitize_text("error sending request for url (https://example.com/a.zip?X-Amz-Signature=abc&x=1): timeout"),
            "error sending request for url (https://example.com/a.zip?<query>): timeout"
        );
        // 利用者のフォルダのパスの後ろに URL が続いても、URL のクエリを伏せる
        let path_then_url = sanitize_text(r"save C:\Users\taro failed https://example.com/a.zip?X-Amz-Signature=abc");
        assert!(!path_then_url.contains("X-Amz-Signature") && !path_then_url.contains("taro"), "{path_then_url}");
        let path_then_url = sanitize_text("/home/taro url=https://example.com/a?sig=abc");
        assert!(!path_then_url.contains("sig=abc") && !path_then_url.contains("taro"), "{path_then_url}");
        let long = "あ".repeat(MAX_TEXT_CHARS + 50);
        let cut = sanitize_text(&long);
        assert_eq!(cut.chars().count(), MAX_TEXT_CHARS + 1);
        assert!(cut.ends_with('…'));

        let value = sanitize_value(json!({
            "message": "key sk-ant-api03-SECRET",
            "nested": { "path": r"C:\Users\someone\x.json", "list": ["sk-live-XYZ123", 1, true] },
        }));
        let text = value.to_string();
        assert!(!text.contains("SECRET") && !text.contains("XYZ123") && !text.contains("someone"), "{text}");

        // 値の数が多すぎるときは、上限で打ち切る
        let many = sanitize_value(Value::Array((0..1000).map(|i| json!(i)).collect()));
        assert!(many.as_array().unwrap().len() < MAX_VALUES);

        // ファイルにも、伏せた形で書く
        let dir = tempfile::tempdir().unwrap();
        let log = DebugLog::open(dir.path()).unwrap();
        log.write(Level::Error, Source::Ui, "ui.error", json!({ "message": "bad key sk-test-openai-123456" }));
        let written = fs::read_to_string(&log.files()[0]).unwrap();
        assert!(!written.contains("sk-test-openai"), "{written}");
    }

    #[test]
    fn log_file_names_round_trip() {
        let day = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        assert_eq!(file_name(day), "kiroscribe-20260105.log");
        assert_eq!(day_of("kiroscribe-20260105.log"), Some(day));
        assert_eq!(day_of("kiroscribe-2026015.log"), None);
        assert_eq!(day_of("other-20260105.log"), None);
        assert!(valid_event_name("meeting.check"));
        assert!(!valid_event_name("Meeting check"));
    }
}
