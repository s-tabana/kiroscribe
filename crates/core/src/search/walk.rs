//! 追加したフォルダの中の、索引に入れるファイルを集める。
//!
//! 隠しフォルダ・システムのフォルダ・「.」で始まるフォルダ・`node_modules`・ごみ箱は下りない。
//! シンボリックリンク・ジャンクション・ショートカット（.lnk）はたどらない。
//! OneDrive の同期したファイルのような、別の場所を指さない再解析ポイントは、ふつうのファイルとして扱う。
//!
//! 中のフォルダとファイルは、パスの文字列ではなく、開いた探すフォルダのハンドルを基準にたどる。
//! 探すフォルダの中にあると OS が保証できないものは、開かない（中身を読まない）。

use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::extract::{kind_of, Kind};

/// 1 つのフォルダから索引に入れるファイルの数の上限。
pub const MAX_FILES_PER_FOLDER: usize = 20_000;
/// 索引に入れる 1 ファイルの大きさの上限（これより大きいファイルは入れない）。
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_files: usize,
    pub max_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_files: MAX_FILES_PER_FOLDER, max_bytes: MAX_FILE_BYTES }
    }
}

/// 索引に入れるファイル。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: PathBuf,
    pub kind: Kind,
    /// 更新日時（ミリ秒）
    pub modified: i64,
    pub size: u64,
    /// クラウドにだけあるファイル。ダウンロードさせないよう、中身を読まずに名前だけで探す
    pub online_only: bool,
}

#[derive(Debug, Default)]
pub struct Walked {
    pub files: Vec<FileEntry>,
    /// 上限（[`MAX_FILES_PER_FOLDER`]）を超えたため、一部だけを集めた
    pub truncated: bool,
    /// 中を読めなかったフォルダ（読み取りの権限がないなど）。この中のファイルは、消えたとみなさない
    pub unreadable: Vec<PathBuf>,
}

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x4_0000;
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;

/// ファイルやフォルダの扱い方（Windows の属性から決める）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Traits {
    /// たどらないリンク（シンボリックリンク・ジャンクション）
    link: bool,
    hidden_or_system: bool,
    /// クラウドにだけある（OneDrive の「オンライン時のみ」など。読むとダウンロードが始まる）
    online_only: bool,
}

/// `name_surrogate` は、別の場所を指す再解析ポイント（シンボリックリンク・ジャンクション）か
/// （Rust の `FileType::is_symlink` が、Windows ではこれを返す）。
/// OneDrive の同期済みのファイルのような、ほかの再解析ポイントは、ふつうのファイルとして扱う。
fn traits(name_surrogate: bool, attributes: u32) -> Traits {
    Traits {
        link: name_surrogate,
        hidden_or_system: attributes & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0,
        online_only: attributes & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS) != 0,
    }
}

fn traits_of(meta: &Metadata) -> Traits {
    #[cfg(windows)]
    let attributes = {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes()
    };
    #[cfg(not(windows))]
    let attributes = 0;
    traits(meta.file_type().is_symlink(), attributes)
}

/// 下りないフォルダの名前か。
fn is_skipped_dir_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    name.starts_with('.')
        || lower == "node_modules"
        || lower == "$recycle.bin"
        || lower == "recycler"
        || lower == "recycled"
        || lower == "system volume information"
}

/// 入れないファイルの名前か（「.」で始まるもの、Office の一時ファイル）。
fn is_skipped_file_name(name: &str) -> bool {
    name.starts_with('.') || name.starts_with("~$")
}

fn modified_ms(meta: &Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// ファイル・フォルダの属性（リンクはたどらない。開いたハンドルか、フォルダの列挙から得る）。
#[derive(Debug, Clone, Copy)]
pub struct Info {
    dir: bool,
    file: bool,
    len: u64,
    /// 更新日時（ミリ秒）
    modified: i64,
    traits: Traits,
}

impl Info {
    fn of(meta: &Metadata) -> Info {
        Info { dir: meta.is_dir(), file: meta.is_file(), len: meta.len(), modified: modified_ms(meta), traits: traits_of(meta) }
    }

    pub fn is_dir(&self) -> bool {
        self.dir
    }

    pub fn is_file(&self) -> bool {
        self.file
    }
}

/// 索引に入れるファイルか（名前・種類・属性・大きさで決める）。入れないなら None。
fn entry_of(path: &Path, info: &Info, limits: Limits) -> Option<FileEntry> {
    let name = path.file_name()?.to_str()?;
    if is_skipped_file_name(name) {
        return None;
    }
    let kind = kind_of(path)?;
    if !info.file || info.traits.link || info.traits.hidden_or_system || info.len > limits.max_bytes {
        return None;
    }
    Some(FileEntry { path: path.to_path_buf(), kind, modified: info.modified, size: info.len, online_only: info.traits.online_only })
}

/// クラウドにだけあるファイルか（読む直前に確かめる。読むとダウンロードが始まるため）。
pub fn is_online_only(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| traits_of(&meta).online_only)
}

/// そのパスそのものが、たどらないリンク（シンボリックリンク・ジャンクション）か。
pub fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| traits_of(&meta).link)
}

/// ファイル・フォルダの識別子（Unix はデバイスと i ノードの番号、Windows はボリュームのシリアル番号と 128 ビットのファイル ID）。
/// 移したフォルダの代わりに、同じ名前のフォルダが作られたら変わる。
pub type FileId = (u64, u128);

/// 探すフォルダとして足したときに開いた実体: 実際の場所（OS の表し方のまま、文字に直さずに持つ）・識別子・作成日時。
/// 設定に保存し、次に開いたフォルダが、足したときと同じものかを確かめる（アプリを起動し直したあとも）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "StoredPlace", into = "StoredPlace")]
pub struct Place {
    /// 実際の場所を、損なわずに文字にしたもの（[`encode_os`]）。取れなかったときは None（同じ場所かを確かめられない）
    real: Option<String>,
    id: FileId,
    /// 作成日時（1970 年からのナノ秒）。取れなかったときは None
    born: Option<u128>,
}

impl Place {
    /// 開いたフォルダが、足したときと同じ場所にあるか（同じ場所で作り直したフォルダを含む）。
    /// 足したときと開いたときの両方で実際の場所が取れ、大文字と小文字を含めて完全に一致するときだけ認める
    /// （識別子が同じでも、別の場所なら認めない。消したフォルダの識別子は、別の場所のフォルダに使い回されることがある）。
    /// どちらかで取れなければ、同じ場所かを確かめられないので認めない（識別子と作成日時では、移したフォルダと区別できない）。
    pub fn admits(&self, opened: &Root) -> bool {
        match (&self.real, opened.real.as_deref()) {
            (Some(real), Some(opened_real)) => *real == encode_os(opened_real.as_os_str()),
            _ => false,
        }
    }

    /// 実際の場所を記録しておらず、あとで同じ場所のフォルダかを確かめられない。
    pub fn is_unverifiable(&self) -> bool {
        self.real.is_none()
    }

    pub fn id(&self) -> FileId {
        self.id
    }
}

/// 設定に保存する形（JSON の数は大きな値を損なうため、識別子は 10 進の文字にする）。
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredPlace {
    real: Option<String>,
    volume: String,
    file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    born: Option<String>,
}

impl TryFrom<StoredPlace> for Place {
    type Error = String;

    fn try_from(stored: StoredPlace) -> Result<Self, Self::Error> {
        let volume = stored.volume.parse().map_err(|_| "識別子を読めません".to_string())?;
        let file = stored.file.parse().map_err(|_| "識別子を読めません".to_string())?;
        let born = stored.born.map(|born| born.parse().map_err(|_| "作成日時を読めません".to_string())).transpose()?;
        // 実際の場所は、この OS の表し方で、損なわずに文字にしたものだけを認める
        if let Some(real) = &stored.real {
            if !is_encoded_os(real) {
                return Err("実際の場所を読めません".into());
            }
        }
        let place = Place { real: stored.real, id: (volume, file), born };
        // 同じフォルダかを確かめられない値は、記録していないものとして扱う（選び直してもらう）
        if place.is_unverifiable() {
            return Err("同じフォルダかを確かめられません".into());
        }
        Ok(place)
    }
}

impl From<Place> for StoredPlace {
    fn from(place: Place) -> Self {
        StoredPlace {
            real: place.real,
            volume: place.id.0.to_string(),
            file: place.id.1.to_string(),
            born: place.born.map(|born| born.to_string()),
        }
    }
}

/// [`encode_os`] で、この OS のパスを文字にしたものか（空のパス・別の OS の表し方・16 進でない値は、認めない）。
fn is_encoded_os(text: &str) -> bool {
    let (prefix, width) = if cfg!(windows) { ("w:", 4) } else { ("b:", 2) };
    text.strip_prefix(prefix)
        .is_some_and(|hex| !hex.is_empty() && hex.len() % width == 0 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// OS のパスを、文字に直さずに（損なわずに）比べられる文字にする。
/// Unix はバイト列、Windows は UTF-16 の並びを 16 進にする（OS が違えば、一致しない）。
fn encode_os(text: &OsStr) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        out.push_str("b:");
        for byte in text.as_bytes() {
            let _ = write!(out, "{byte:02x}");
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        out.push_str("w:");
        for unit in text.encode_wide() {
            let _ = write!(out, "{unit:04x}");
        }
    }
    out
}

/// 開いたフォルダの実際の場所を OS に問い合わせた結果を、場所が取れたかどうかに直す。
/// 問い合わせが失敗したときは、フォルダを開けたままで、実際の場所が取れなかったものとする（どの OS でも同じ扱い）。
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
fn real_or_none(path: &Path, lookup: io::Result<PathBuf>) -> Option<PathBuf> {
    #[cfg(test)]
    let lookup = if test_hooks::hides_real(path) { Err(io::Error::other("テスト用: 実際の場所を取れない")) } else { lookup };
    #[cfg(not(test))]
    let _ = path;
    lookup.ok()
}

/// テスト用: 開いたときに、実際の場所の問い合わせが失敗したことにするフォルダ（OS が実際の場所を返さない場合を再現する）。
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static HIDDEN: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    pub(crate) fn hide_real(path: &Path, hidden: bool) {
        let mut list = HIDDEN.lock().unwrap();
        list.retain(|p| p != path);
        if hidden {
            list.push(path.to_path_buf());
        }
    }

    pub(super) fn hides_real(path: &Path) -> bool {
        HIDDEN.lock().unwrap().iter().any(|p| p == path)
    }
}

/// 探すフォルダの範囲の外（リンクをたどらないと届かない・除くフォルダを通る・開いたものが別の場所にある）。
#[derive(Debug)]
struct Outside;

impl std::fmt::Display for Outside {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("探すフォルダの範囲の外です（リンクの先はたどりません）")
    }
}

impl std::error::Error for Outside {}

fn outside() -> io::Error {
    io::Error::other(Outside)
}

/// 範囲の外だったためのエラーか。
pub fn is_outside(err: &io::Error) -> bool {
    err.get_ref().is_some_and(|inner| inner.is::<Outside>())
}

/// ファイルを読んだ結果。
#[derive(Debug)]
pub enum Content {
    Bytes(Vec<u8>),
    /// クラウドにだけある（開いたあとで分かった。読むとダウンロードが始まるので読まない）
    OnlineOnly,
    /// 範囲の外にある・ふつうのファイルでない（中身も名前も索引に入れない）
    Outside,
    /// 読めなかった（権限がない・大きくなりすぎた・探すフォルダの中にあると確かめられないなど）
    Failed(io::Error),
}

/// 開いた探すフォルダ。中のフォルダとファイルは、このフォルダのハンドルを基準にたどる。
/// 実際に開いたものがこのフォルダの中にあると OS が保証できるときだけ、列挙したり読んだりする。
pub struct Root {
    path: PathBuf,
    dir: File,
    id: FileId,
    real: Option<PathBuf>,
    born: Option<u128>,
}

impl Root {
    /// 探すフォルダを開く。フォルダがない・フォルダそのものがリンク・フォルダでないときはエラー。
    pub fn open(path: &Path) -> io::Result<Root> {
        let (dir, id, real) = sys::open_root(path)?;
        let born = dir
            .metadata()
            .and_then(|meta| meta.created())
            .ok()
            .and_then(|created| created.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_nanos());
        Ok(Root { path: path.to_path_buf(), dir, id, real, born })
    }

    /// 足したときのパス（索引の行のパスは、これを基準にする）。
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn id(&self) -> FileId {
        self.id
    }

    /// 開いたときの実際の場所（上のフォルダのリンクをたどった先も含めた、本当の場所）。取れなかったときは None。
    pub fn real(&self) -> Option<&Path> {
        self.real.as_deref()
    }

    /// 開いたフォルダの実体（足したときに設定へ保存する）。
    pub fn place(&self) -> Place {
        Place { real: self.real.as_deref().map(|real| encode_os(real.as_os_str())), id: self.id, born: self.born }
    }

    /// `path` の、このフォルダからの名前の並び。中を指さない・除くフォルダを通るときは None。
    fn parts(&self, path: &Path) -> Option<Vec<OsString>> {
        let rest = path.strip_prefix(&self.path).ok()?;
        let mut parts = Vec::new();
        for part in rest.components() {
            let std::path::Component::Normal(name) = part else { return None };
            parts.push(name.to_os_string());
        }
        let (_, dirs) = parts.split_last()?;
        dirs.iter().all(|dir| dir.to_str().is_some_and(|name| !is_skipped_dir_name(name))).then_some(parts)
    }

    /// `path` が、途中に除くフォルダ（隠し・システム・「.」で始まるなど）を通らずに届く、このフォルダの中のパスか。
    /// 途中のフォルダの属性は、このフォルダのハンドルを基準に、リンクをたどらずに調べる。
    pub fn allows(&self, path: &Path) -> bool {
        let Some(parts) = self.parts(path) else { return false };
        let dirs = &parts[..parts.len() - 1];
        (0..dirs.len()).all(|i| {
            sys::stat_child(self, &dirs[..i], &dirs[i]).is_ok_and(|info| info.dir && !info.traits.link && !info.traits.hidden_or_system)
        })
    }

    /// このフォルダの中のファイル・フォルダの属性を、リンクをたどらずに調べる。範囲の外なら [`is_outside`] のエラー。
    pub fn stat(&self, path: &Path) -> io::Result<Info> {
        let parts = self.parts(path).ok_or_else(outside)?;
        let (name, dirs) = parts.split_last().ok_or_else(outside)?;
        sys::stat_child(self, dirs, name)
    }

    /// 1 つのファイルを、索引に入れるものとして調べる（入れないなら None）。見張りで届いた変更に使う。
    pub fn inspect(&self, path: &Path, limits: Limits) -> Option<FileEntry> {
        if !self.allows(path) {
            return None;
        }
        entry_of(path, &self.stat(path).ok()?, limits)
    }

    /// このフォルダの中にある、ふつうのファイルか（中身は読まない。名前だけで索引に入れるときに使う）。
    pub fn contains_file(&self, path: &Path) -> bool {
        self.stat(path).is_ok_and(|info| info.file && !info.traits.link)
    }

    /// このフォルダの中のファイルの中身を、`max_bytes` まで読む。
    /// 開いたファイルが、このフォルダの中にリンクをたどらずに届く、ふつうのファイルだと確かめてから読む。
    pub fn read(&self, path: &Path, max_bytes: u64) -> Content {
        use std::io::Read;
        let Some(parts) = self.parts(path) else { return Content::Outside };
        let file = match sys::open_file(self, &parts) {
            Ok(file) => file,
            Err(err) if is_outside(&err) => return Content::Outside,
            Err(err) => return Content::Failed(err),
        };
        let info = match file.metadata() {
            Ok(meta) => Info::of(&meta),
            Err(err) => return Content::Failed(err),
        };
        if !info.file || info.traits.link {
            return Content::Outside;
        }
        if info.traits.online_only {
            return Content::OnlineOnly;
        }
        let mut bytes = Vec::new();
        if let Err(err) = file.take(max_bytes.saturating_add(1)).read_to_end(&mut bytes) {
            return Content::Failed(err);
        }
        if bytes.len() as u64 > max_bytes {
            return Content::Failed(io::Error::new(io::ErrorKind::FileTooLarge, "ファイルが大きすぎます"));
        }
        Content::Bytes(bytes)
    }

    /// フォルダの中（下のフォルダを含む）の、索引に入れるファイルを集める。
    pub fn walk(&self, limits: Limits) -> Walked {
        let mut walked = Walked::default();
        let mut stack: Vec<Vec<OsString>> = vec![Vec::new()];
        while let Some(parts) = stack.pop() {
            let dir_path = parts.iter().fold(self.path.clone(), |path, part| path.join(part));
            let Ok(mut entries) = sys::list(self, &parts) else {
                walked.unreadable.push(dir_path);
                continue;
            };
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let mut subdirs = Vec::new();
            for (name, info) in entries {
                if info.traits.link || info.traits.hidden_or_system {
                    continue;
                }
                let Some(text) = name.to_str() else {
                    continue;
                };
                if info.dir {
                    if !is_skipped_dir_name(text) {
                        let mut sub = parts.clone();
                        sub.push(name.clone());
                        subdirs.push(sub);
                    }
                    continue;
                }
                if let Some(file) = entry_of(&dir_path.join(&name), &info, limits) {
                    if walked.files.len() >= limits.max_files {
                        walked.truncated = true;
                        return walked;
                    }
                    walked.files.push(file);
                }
            }
            // 名前の順にたどる（スタックなので逆に積む）
            stack.extend(subdirs.into_iter().rev());
        }
        walked
    }
}

/// 列挙した 1 つの項目の属性。消えたものは除き、調べられないものがあればフォルダごと読めなかったことにする
/// （中のファイルを、消えたとみなさない）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn listed(name: OsString, info: io::Result<Info>) -> io::Result<Option<(OsString, Info)>> {
    match info {
        Ok(info) => Ok(Some((name, info))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// 探すフォルダの中にあると確かめられないため、開かなかった（中身を読まない）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn unverifiable() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "この環境では、探すフォルダの中にあると確かめられないため、読みません")
}

/// Linux: 探すフォルダのハンドルを基準に、中のパス全体を `openat2` で 1 回で開く。
/// カーネルが、リンクをたどらず（RESOLVE_NO_SYMLINKS）、探すフォルダの下から出ずに（RESOLVE_BENEATH）解決したと保証する。
/// `openat2` を使えない環境（Linux 5.6 より前など）では、確かめられないので開かない。
#[cfg(target_os = "linux")]
mod sys {
    use super::*;
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    /// カーネルが、解決のあいだにフォルダが移されたため保証できない（EAGAIN）と返したときに、解決し直す回数。
    const RETRIES: usize = 8;

    fn handle_path(file: &File) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
    }

    fn id_of(file: &File) -> io::Result<FileId> {
        let meta = file.metadata()?;
        Ok((meta.dev(), u128::from(meta.ino())))
    }

    pub(super) fn open_root(path: &Path) -> io::Result<(File, FileId, Option<PathBuf>)> {
        let dir = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|err| match err.raw_os_error() {
                Some(libc::ELOOP) => io::Error::new(io::ErrorKind::InvalidInput, "リンクのフォルダはたどりません"),
                Some(libc::ENOTDIR) => io::Error::new(io::ErrorKind::NotADirectory, "フォルダではありません"),
                _ => err,
            })?;
        let id = id_of(&dir)?;
        let real = real_or_none(path, std::fs::read_link(handle_path(&dir)));
        Ok((dir, id, real))
    }

    #[cfg(test)]
    thread_local! {
        /// テスト用: `openat2` がない環境のふりをする
        pub(super) static NO_OPENAT2: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    fn openat2(dir: &File, path: &CString, how: &libc::open_how) -> libc::c_long {
        #[cfg(test)]
        if NO_OPENAT2.with(|flag| flag.get()) {
            unsafe { *libc::__errno_location() = libc::ENOSYS };
            return -1;
        }
        unsafe {
            libc::syscall(
                libc::SYS_openat2,
                dir.as_raw_fd(),
                path.as_ptr(),
                how as *const libc::open_how,
                std::mem::size_of::<libc::open_how>(),
            )
        }
    }

    /// 探すフォルダのハンドルを基準に、名前の並び `parts` を 1 回で開く（`parts` が空なら、探すフォルダそのもの）。
    fn open_beneath(root: &Root, parts: &[OsString], flags: libc::c_int) -> io::Result<File> {
        if parts.is_empty() {
            return root.dir.try_clone();
        }
        let mut relative = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            let bytes = part.as_bytes();
            if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
                return Err(outside());
            }
            if i > 0 {
                relative.push(b'/');
            }
            relative.extend_from_slice(bytes);
        }
        let relative = CString::new(relative).map_err(|_| outside())?;
        let mut how: libc::open_how = unsafe { std::mem::zeroed() };
        how.flags = (flags | libc::O_NOFOLLOW | libc::O_CLOEXEC) as u64;
        how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS;
        for _ in 0..RETRIES {
            let fd = openat2(&root.dir, &relative, &how);
            if fd >= 0 {
                return Ok(unsafe { File::from_raw_fd(fd as libc::c_int) });
            }
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                Some(libc::EINTR) | Some(libc::EAGAIN) => continue,
                // リンクを通る（ELOOP）・探すフォルダの下から出る（EXDEV）・途中がフォルダでない（ENOTDIR）
                Some(libc::ELOOP) | Some(libc::EXDEV) | Some(libc::ENOTDIR) => return Err(outside()),
                // openat2 がない・解決の指定に対応していない
                Some(libc::ENOSYS) | Some(libc::E2BIG) | Some(libc::EINVAL) => return Err(unverifiable()),
                _ => return Err(err),
            }
        }
        Err(unverifiable())
    }

    fn open_dirs(root: &Root, dirs: &[OsString]) -> io::Result<File> {
        open_beneath(root, dirs, libc::O_RDONLY | libc::O_DIRECTORY)
    }

    pub(super) fn stat_child(root: &Root, dirs: &[OsString], name: &OsString) -> io::Result<Info> {
        let mut parts = dirs.to_vec();
        parts.push(name.clone());
        // 中身は開かない（O_PATH）。最後の名前がリンクなら、リンクそのものの属性になる
        let file = open_beneath(root, &parts, libc::O_PATH)?;
        Ok(Info::of(&file.metadata()?))
    }

    pub(super) fn open_file(root: &Root, parts: &[OsString]) -> io::Result<File> {
        if parts.is_empty() {
            return Err(outside());
        }
        open_beneath(root, parts, libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOCTTY)
    }

    pub(super) fn list(root: &Root, dirs: &[OsString]) -> io::Result<Vec<(OsString, Info)>> {
        let dir = open_dirs(root, dirs)?;
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(handle_path(&dir))? {
            let entry = entry?;
            // 列挙したハンドルを基準に、リンクをたどらずに調べる
            entries.extend(listed(entry.file_name(), entry.metadata().map(|meta| Info::of(&meta)))?);
        }
        // 列挙したあとも、同じフォルダが探すフォルダの下の同じ名前の並びにある（列挙のあいだに外へ移されていない）
        if id_of(&open_dirs(root, dirs)?)? != id_of(&dir)? {
            return Err(outside());
        }
        Ok(entries)
    }
}

/// Windows: 探すフォルダのハンドルを基準に、名前を 1 つずつ `NtCreateFile` で開く（親のハンドルからの相対の名前で開くので、
/// 開いたものは、その親のフォルダの中にある。パスの文字列や大文字と小文字の比べ方には頼らない）。
/// リンクは開かずに、リンクそのものを開いて見分ける。途中のフォルダは、名前を変える・移すことを許さずに開いておき、
/// 開き終えるまで、探すフォルダの外へ移されないようにする。
#[cfg(windows)]
mod sys {
    use super::*;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        NtCreateFile, FILE_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
    };
    use windows_sys::Win32::Foundation::{
        RtlNtStatusToDosError, ERROR_DIRECTORY, ERROR_NO_MORE_FILES, HANDLE, OBJ_CASE_INSENSITIVE, UNICODE_STRING,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo, FileIdInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx, GetFinalPathNameByHandleW, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_BOTH_DIR_INFO,
        FILE_ID_INFO, FILE_LIST_DIRECTORY, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, SYNCHRONIZE, VOLUME_NAME_DOS,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    /// 再解析ポイントの種類のうち、別の場所を指すもの（シンボリックリンク・ジャンクション）の印。
    const NAME_SURROGATE: u32 = 0x2000_0000;
    /// FILETIME（1601 年からの 100 ナノ秒）と、1970 年からのミリ秒の差。
    const EPOCH_DIFFERENCE_MS: i64 = 11_644_473_600_000;

    fn file_id(file: &File) -> io::Result<FileId> {
        let mut info: FILE_ID_INFO = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle() as HANDLE,
                FileIdInfo,
                (&mut info as *mut FILE_ID_INFO).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        };
        if ok != 0 {
            return Ok((info.VolumeSerialNumber, u128::from_le_bytes(info.FileId.Identifier)));
        }
        // 128 ビットのファイル ID を返さないファイルシステム
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((u64::from(info.dwVolumeSerialNumber), (u128::from(info.nFileIndexHigh) << 32) | u128::from(info.nFileIndexLow)))
    }

    fn final_path(file: &File) -> io::Result<PathBuf> {
        let mut buf = vec![0u16; 512];
        loop {
            let len = unsafe {
                GetFinalPathNameByHandleW(file.as_raw_handle() as HANDLE, buf.as_mut_ptr(), buf.len() as u32, FILE_NAME_NORMALIZED | VOLUME_NAME_DOS)
            } as usize;
            if len == 0 {
                return Err(io::Error::last_os_error());
            }
            if len < buf.len() {
                buf.truncate(len);
                return Ok(PathBuf::from(OsString::from_wide(&buf)));
            }
            buf.resize(len + 1, 0);
        }
    }

    fn is_reparse_point(meta: &Metadata) -> bool {
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }

    pub(super) fn open_root(path: &Path) -> io::Result<(File, FileId, Option<PathBuf>)> {
        let open = |flags: u32| std::fs::OpenOptions::new().read(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS | flags).open(path);
        // フォルダそのものを開く（リンクなら、リンクの先ではなくリンクそのものを開く）
        let dir = open(FILE_FLAG_OPEN_REPARSE_POINT)?;
        let meta = dir.metadata()?;
        if traits_of(&meta).link {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "リンクのフォルダはたどりません"));
        }
        if !meta.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, "フォルダではありません"));
        }
        // 別の場所を指さない再解析ポイント（OneDrive のフォルダなど）は、中身を列挙できるよう、ふつうに開き直す。
        // 開き直すあいだにリンクに替わっていないことを、識別子で確かめる
        let dir = if is_reparse_point(&meta) {
            let again = open(0)?;
            if file_id(&again)? != file_id(&dir)? {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "開いているあいだに、フォルダが替わりました"));
            }
            again
        } else {
            dir
        };
        let id = file_id(&dir)?;
        // ネットワークのフォルダなどで実際の場所を取れないときは、識別子だけで確かめる
        let real = real_or_none(path, final_path(&dir));
        Ok((dir, id, real))
    }

    /// 親のフォルダのハンドルを基準に、名前 `name`（区切りを含まない 1 つの名前）を開く。
    fn nt_open(parent: &File, name: &OsStr, access: u32, share: u32, options: u32) -> io::Result<File> {
        let wide: Vec<u16> = name.encode_wide().collect();
        if wide.is_empty() || wide.contains(&u16::from(b'\\')) || wide.contains(&u16::from(b'/')) || wide.len() * 2 > usize::from(u16::MAX) {
            return Err(outside());
        }
        let bytes = (wide.len() * 2) as u16;
        let object_name = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: wide.as_ptr() as *mut u16 };
        let attributes = OBJECT_ATTRIBUTES {
            Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: parent.as_raw_handle() as HANDLE,
            ObjectName: &object_name,
            // 名前の探し方は、エクスプローラーなどのふつうの開き方（CreateFile）と同じにする
            // （大文字と小文字を区別するフォルダでは、OS が区別する）
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: std::ptr::null(),
            SecurityQualityOfService: std::ptr::null(),
        };
        let mut handle: HANDLE = std::ptr::null_mut();
        let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        let status = unsafe {
            NtCreateFile(
                &mut handle,
                access | SYNCHRONIZE,
                &attributes,
                &mut status_block,
                std::ptr::null(),
                0,
                share,
                FILE_OPEN,
                options | FILE_SYNCHRONOUS_IO_NONALERT,
                std::ptr::null(),
                0,
            )
        };
        if status < 0 {
            let code = unsafe { RtlNtStatusToDosError(status) };
            return Err(if code == ERROR_DIRECTORY { outside() } else { io::Error::from_raw_os_error(code as i32) });
        }
        Ok(unsafe { File::from_raw_handle(handle as _) })
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Want {
        /// 中をたどる・列挙するフォルダ（名前を変える・移すことを許さない）
        Dir,
        /// 中身を読むファイル
        Data,
        /// 属性だけ（リンクなら、リンクそのものの属性）
        Attributes,
    }

    fn open_child(parent: &File, name: &OsStr, want: Want) -> io::Result<File> {
        let (access, share, options) = match want {
            Want::Dir => (FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE, FILE_DIRECTORY_FILE),
            Want::Data => (FILE_READ_DATA | FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, 0),
            Want::Attributes => (FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, 0),
        };
        // リンクをたどらずに開く（リンクなら、リンクそのものが開く）
        let first = nt_open(parent, name, access, share, options | FILE_OPEN_REPARSE_POINT)?;
        if want == Want::Attributes {
            return Ok(first);
        }
        let meta = first.metadata()?;
        let traits = traits_of(&meta);
        if traits.link || (want == Want::Dir && !meta.is_dir()) {
            return Err(outside());
        }
        // クラウドにだけあるファイルは、取り寄せないよう、開き直さない（中身は読まない）
        if !is_reparse_point(&meta) || traits.online_only {
            return Ok(first);
        }
        // 別の場所を指さない再解析ポイント（OneDrive の同期済みのファイルなど）は、ふつうに開き直して、中身を読む。
        // 開き直すあいだにリンクに替わっていないことを、識別子で確かめる
        let again = nt_open(parent, name, access, share, options)?;
        if file_id(&again)? != file_id(&first)? {
            return Err(outside());
        }
        Ok(again)
    }

    /// 名前の並び `dirs` のフォルダを、探すフォルダのハンドルから 1 つずつ開く。開いたフォルダは、使い終えるまで持っておく。
    fn open_chain(root: &Root, dirs: &[OsString]) -> io::Result<Vec<File>> {
        let mut chain: Vec<File> = Vec::new();
        for name in dirs {
            let dir = open_child(chain.last().unwrap_or(&root.dir), name, Want::Dir)?;
            chain.push(dir);
        }
        Ok(chain)
    }

    pub(super) fn stat_child(root: &Root, dirs: &[OsString], name: &OsString) -> io::Result<Info> {
        let chain = open_chain(root, dirs)?;
        let file = open_child(chain.last().unwrap_or(&root.dir), name, Want::Attributes)?;
        Ok(Info::of(&file.metadata()?))
    }

    pub(super) fn open_file(root: &Root, parts: &[OsString]) -> io::Result<File> {
        let (name, dirs) = parts.split_last().ok_or_else(outside)?;
        let chain = open_chain(root, dirs)?;
        open_child(chain.last().unwrap_or(&root.dir), name, Want::Data)
    }

    /// 列挙で得た属性（`std` の `Metadata` と同じ決め方にする）。
    fn info_of_entry(entry: &FILE_ID_BOTH_DIR_INFO) -> Info {
        let attributes = entry.FileAttributes;
        // 再解析ポイントの種類は、列挙では EaSize の場所に入る
        let link = attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 && entry.EaSize & NAME_SURROGATE != 0;
        let dir = !link && attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        let modified = (entry.LastWriteTime / 10_000 - EPOCH_DIFFERENCE_MS).max(0);
        Info { dir, file: !link && !dir, len: entry.EndOfFile.max(0) as u64, modified, traits: traits(link, attributes) }
    }

    pub(super) fn list(root: &Root, dirs: &[OsString]) -> io::Result<Vec<(OsString, Info)>> {
        let chain = open_chain(root, dirs)?;
        let dir = chain.last().unwrap_or(&root.dir);
        let mut buf = vec![0u64; 8 * 1024];
        let mut class = FileIdBothDirectoryRestartInfo;
        let mut entries = Vec::new();
        loop {
            let ok = unsafe {
                GetFileInformationByHandleEx(dir.as_raw_handle() as HANDLE, class, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32)
            };
            if ok == 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                    break;
                }
                return Err(err);
            }
            class = FileIdBothDirectoryInfo;
            let base = buf.as_ptr().cast::<u8>();
            let mut offset = 0usize;
            loop {
                let entry = unsafe { std::ptr::read_unaligned(base.add(offset).cast::<FILE_ID_BOTH_DIR_INFO>()) };
                let len = entry.FileNameLength as usize / 2;
                let mut name = vec![0u16; len];
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        base.add(offset + std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName)),
                        name.as_mut_ptr().cast::<u8>(),
                        len * 2,
                    );
                }
                let dot = u16::from(b'.');
                if name != [dot] && name != [dot, dot] {
                    entries.push((OsString::from_wide(&name), info_of_entry(&entry)));
                }
                if entry.NextEntryOffset == 0 {
                    break;
                }
                offset += entry.NextEntryOffset as usize;
            }
        }
        Ok(entries)
    }
}

/// ほかの OS: 開いたものの場所を確かめられないので、読まない。
#[cfg(not(any(target_os = "linux", windows)))]
mod sys {
    use super::*;

    fn unsupported() -> io::Error {
        io::Error::new(io::ErrorKind::Unsupported, "この OS では資料の索引を作れません")
    }

    pub(super) fn open_root(_: &Path) -> io::Result<(File, FileId, Option<PathBuf>)> {
        Err(unsupported())
    }

    pub(super) fn stat_child(_: &Root, _: &[OsString], _: &OsString) -> io::Result<Info> {
        Err(unsupported())
    }

    pub(super) fn open_file(_: &Root, _: &[OsString]) -> io::Result<File> {
        Err(unsupported())
    }

    pub(super) fn list(_: &Root, _: &[OsString]) -> io::Result<Vec<(OsString, Info)>> {
        Err(unsupported())
    }
}

/// フォルダの中（下のフォルダを含む）の、索引に入れるファイルを集める。
/// フォルダがないとき・フォルダそのものがリンクのとき（リンクの先はたどらない）はエラー。
pub fn walk(root: &Path, limits: Limits) -> io::Result<Walked> {
    Ok(Root::open(root)?.walk(limits))
}


#[cfg(test)]
mod tests {
    use super::*;

    fn names(walked: &Walked, root: &Path) -> Vec<String> {
        walked
            .files
            .iter()
            .map(|f| f.path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"))
            .collect()
    }

    fn encoded(path: &str) -> String {
        encode_os(Path::new(path).as_os_str())
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "経費").unwrap();
    }

    // [IDX-002][IDX-004]
    #[test]
    fn skips_hidden_dot_and_tool_folders_and_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for name in [
            "a.txt",
            "sub/b.docx",
            "sub/deeper/c.pdf",
            ".git/config.txt",
            ".hidden.txt",
            "~$下書き.docx",
            "node_modules/pkg/readme.md",
            "Node_Modules/x.md",
            "$RECYCLE.BIN/old.txt",
            "sub/.cache/x.txt",
            "tool.exe",
            "script.ps1",
            "photo.png",
        ] {
            touch(&root.join(name));
        }
        let walked = walk(root, Limits::default()).unwrap();
        assert_eq!(names(&walked, root), ["a.txt", "photo.png", "sub/b.docx", "sub/deeper/c.pdf"]);
        assert!(!walked.truncated);
    }

    // [IDX-002]
    #[cfg(windows)]
    #[test]
    fn skips_hidden_and_system_attributes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("見える.txt"));
        touch(&root.join("隠し.txt"));
        touch(&root.join("システム.txt"));
        touch(&root.join("隠しフォルダ/中.txt"));
        let attrib = |flag: &str, path: &Path| {
            let status = std::process::Command::new("attrib").arg(flag).arg(path).status().unwrap();
            assert!(status.success());
        };
        attrib("+h", &root.join("隠し.txt"));
        attrib("+s", &root.join("システム.txt"));
        attrib("+h", &root.join("隠しフォルダ"));
        let walked = walk(root, Limits::default()).unwrap();
        assert_eq!(names(&walked, root), ["見える.txt"]);
    }

    fn bytes(content: Content) -> Option<Vec<u8>> {
        match content {
            Content::Bytes(bytes) => Some(bytes),
            _ => None,
        }
    }

    // [IDX-027]
    #[cfg(unix)]
    #[test]
    fn does_not_follow_symbolic_links() {
        let outside = tempfile::tempdir().unwrap();
        touch(&outside.path().join("外.txt"));
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("中.txt"));
        std::os::unix::fs::symlink(outside.path(), root.join("リンク")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("外.txt"), root.join("外へ.txt")).unwrap();
        let walked = walk(root, Limits::default()).unwrap();
        assert_eq!(names(&walked, root), ["中.txt"]);
        let opened = Root::open(root).unwrap();
        assert!(!opened.contains_file(&root.join("リンク").join("外.txt")));
        assert!(!opened.contains_file(&root.join("外へ.txt")));
        assert!(opened.contains_file(&root.join("中.txt")));
        assert!(!opened.contains_file(&outside.path().join("外.txt")), "フォルダの外");
        assert!(opened.inspect(&root.join("リンク").join("外.txt"), Limits::default()).is_none());
        assert!(is_outside(&opened.stat(&root.join("リンク").join("外.txt")).unwrap_err()));

        // 追加したフォルダそのものがリンクなら、開かない（リンクの先はたどらない）
        let linked = dir.path().join("リンクのフォルダ");
        std::os::unix::fs::symlink(outside.path(), &linked).unwrap();
        assert!(is_link(&linked));
        assert!(!is_link(root));
        assert!(walk(&linked, Limits::default()).is_err());
        assert!(Root::open(&linked).is_err());
        assert_eq!(bytes(opened.read(&root.join("中.txt"), MAX_FILE_BYTES)).unwrap(), "経費".as_bytes());
        assert!(matches!(opened.read(&root.join("外へ.txt"), MAX_FILE_BYTES), Content::Outside));
        assert!(matches!(opened.read(&root.join("リンク").join("外.txt"), MAX_FILE_BYTES), Content::Outside));
    }

    /// 開いたあとで、ファイルや途中のフォルダがリンクに替わっても、リンクの先を読まない（読む直前の差し替え）。
    // [IDX-027]
    #[cfg(unix)]
    #[test]
    fn files_and_folders_replaced_by_links_before_reading_are_not_read() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("a.txt"), "社外秘").unwrap();
        std::fs::write(outside.path().join("b.txt"), "社外秘").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("資料");
        touch(&root.join("a.txt"));
        touch(&root.join("下").join("b.txt"));
        let opened = Root::open(&root).unwrap();
        let walked = opened.walk(Limits::default());
        assert_eq!(names(&walked, &root), ["a.txt", "下/b.txt"]);

        // 調べたあと、読む前に差し替える
        std::fs::remove_file(root.join("a.txt")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("a.txt"), root.join("a.txt")).unwrap();
        std::fs::rename(root.join("下"), dir.path().join("退避")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("下")).unwrap();
        for file in &walked.files {
            let content = opened.read(&file.path, MAX_FILE_BYTES);
            assert!(matches!(content, Content::Outside), "{}: {content:?}", file.path.display());
            assert!(!opened.contains_file(&file.path));
        }
    }

    /// 開いたあとで、上のフォルダがリンクに替わり、別の場所の同じ名前のフォルダを指すようになっても、
    /// 開いたフォルダのハンドルを基準に読むので、別の場所の中身を読まない。開き直すと、実際の場所が変わったと分かる。
    // [IDX-027]
    #[cfg(unix)]
    #[test]
    fn a_parent_replaced_by_a_link_does_not_redirect_reading() {
        let outside = tempfile::tempdir().unwrap();
        touch(&outside.path().join("共有資料").join("外.txt"));
        std::fs::write(outside.path().join("共有資料").join("a.txt"), "社外秘").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("上").join("共有資料");
        touch(&root.join("a.txt"));
        let opened = Root::open(&root).unwrap();
        let place = opened.place();

        std::fs::rename(dir.path().join("上"), dir.path().join("退避")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("上")).unwrap();
        assert_eq!(bytes(opened.read(&root.join("a.txt"), MAX_FILE_BYTES)).unwrap(), "経費".as_bytes(), "開いたフォルダの中身");
        assert!(!opened.contains_file(&root.join("外.txt")), "別の場所のファイルは、開いたフォルダの中にない");
        assert!(bytes(opened.read(&root.join("外.txt"), MAX_FILE_BYTES)).is_none());
        let names: Vec<_> = opened.walk(Limits::default()).files.iter().map(|f| f.path.file_name().unwrap().to_owned()).collect();
        assert_eq!(names, ["a.txt"]);

        let reopened = Root::open(&root).unwrap();
        assert!(!place.admits(&reopened), "開き直すと、足したときと別のフォルダだと分かる");
        assert!(place.admits(&opened));
        assert_ne!(reopened.id(), opened.id());
    }

    /// 読み始めたあとで大きくなったファイルは、上限を超えて読まない。
    // [IDX-003]
    #[test]
    fn reading_stops_at_the_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.txt"), "12345").unwrap();
        let opened = Root::open(root).unwrap();
        assert_eq!(bytes(opened.read(&root.join("a.txt"), 5)).unwrap(), b"12345");
        assert!(matches!(opened.read(&root.join("a.txt"), 4), Content::Failed(_)));
    }

    // [IDX-010]
    #[cfg(unix)]
    #[test]
    fn folders_that_cannot_be_read_are_reported() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("a.txt"));
        touch(&root.join("鍵/b.txt"));
        std::fs::set_permissions(root.join("鍵"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = std::fs::read_dir(root.join("鍵")).is_ok();
        let walked = walk(root, Limits::default()).unwrap();
        std::fs::set_permissions(root.join("鍵"), std::fs::Permissions::from_mode(0o755)).unwrap();
        if readable {
            return; // 管理者で動かしているときは、読めてしまう
        }
        assert_eq!(names(&walked, root), ["a.txt"]);
        assert_eq!(walked.unreadable, [root.join("鍵")]);
    }

    // [IDX-027]
    #[cfg(windows)]
    #[test]
    fn does_not_follow_junctions_symlinks_or_shortcuts() {
        let outside = tempfile::tempdir().unwrap();
        touch(&outside.path().join("外.txt"));
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("中.txt"));
        touch(&root.join("ショートカット.lnk"));
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(root.join("ジャンクション"))
            .arg(outside.path())
            .status()
            .unwrap();
        assert!(status.success(), "ジャンクションを作れません");
        // シンボリックリンクは、権限（開発者モード）がないと作れないので、作れたときだけ確かめる
        let _ = std::os::windows::fs::symlink_dir(outside.path(), root.join("リンク"));
        let walked = walk(root, Limits::default()).unwrap();
        assert_eq!(names(&walked, root), ["中.txt"]);
        let opened = Root::open(root).unwrap();
        let through = root.join("ジャンクション").join("外.txt");
        assert!(!opened.contains_file(&through));
        assert!(matches!(opened.read(&through, MAX_FILE_BYTES), Content::Outside));
        assert_eq!(bytes(opened.read(&root.join("中.txt"), MAX_FILE_BYTES)).unwrap(), "経費".as_bytes());
        // 追加したフォルダそのものがジャンクションなら、開かない
        assert!(Root::open(&root.join("ジャンクション")).is_err());
    }

    /// 上のフォルダがジャンクションに替わり、別の場所の同じ名前のフォルダを指すようになっても、別の場所の中身を読まない。
    /// Windows は、中のフォルダを開いているあいだ上のフォルダの名前を変えさせないので、変えられたときだけ読み取りを確かめる。
    /// 開き直すと、実際の場所が変わったと分かる。
    // [IDX-027]
    #[cfg(windows)]
    #[test]
    fn a_parent_replaced_by_a_junction_does_not_redirect_reading() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(outside.path().join("共有資料")).unwrap();
        std::fs::write(outside.path().join("共有資料").join("a.txt"), "社外秘").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("上").join("共有資料");
        touch(&root.join("a.txt"));
        let opened = Root::open(&root).unwrap();
        let place = opened.place();
        assert_eq!(bytes(opened.read(&root.join("a.txt"), MAX_FILE_BYTES)).unwrap(), "経費".as_bytes());

        let renamed = std::fs::rename(dir.path().join("上"), dir.path().join("退避")).is_ok();
        let opened = if renamed {
            Some(opened)
        } else {
            drop(opened);
            std::fs::rename(dir.path().join("上"), dir.path().join("退避")).unwrap();
            None
        };
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(dir.path().join("上"))
            .arg(outside.path())
            .status()
            .unwrap();
        assert!(status.success(), "ジャンクションを作れません");
        if let Some(opened) = &opened {
            let content = opened.read(&root.join("a.txt"), MAX_FILE_BYTES);
            assert!(matches!(content, Content::Outside), "{content:?}");
            assert!(!opened.contains_file(&root.join("a.txt")));
            assert!(opened.walk(Limits::default()).files.is_empty());
        }
        let reopened = Root::open(&root).unwrap();
        assert!(!place.admits(&reopened), "開き直すと、足したときと別のフォルダだと分かる");
    }

    // [IDX-027]
    #[test]
    fn only_links_to_other_places_are_skipped() {
        const REPARSE_POINT: u32 = 0x400;
        const PINNED: u32 = 0x8_0000;
        // OneDrive の同期済みのファイル・フォルダ（クラウドの再解析ポイント。別の場所は指さない）
        let synced = traits(false, REPARSE_POINT | PINNED);
        assert_eq!(synced, Traits { link: false, hidden_or_system: false, online_only: false });
        // OneDrive の「オンライン時のみ」のファイル（中身を読むとダウンロードが始まる）
        let online = traits(false, REPARSE_POINT | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS);
        assert_eq!(online, Traits { link: false, hidden_or_system: false, online_only: true });
        assert!(traits(false, FILE_ATTRIBUTE_OFFLINE).online_only);
        // シンボリックリンク・ジャンクション
        assert!(traits(true, REPARSE_POINT).link);
        assert!(traits(false, FILE_ATTRIBUTE_HIDDEN).hidden_or_system);
        assert!(traits(false, FILE_ATTRIBUTE_SYSTEM).hidden_or_system);
        assert_eq!(traits(false, 0), Traits { link: false, hidden_or_system: false, online_only: false });
    }

    // [IDX-003]
    #[test]
    fn exactly_the_file_limit_is_not_marked_partial() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for i in 0..3 {
            touch(&root.join(format!("{i}.txt")));
        }
        let exact = walk(root, Limits { max_files: 3, ..Limits::default() }).unwrap();
        assert_eq!(exact.files.len(), 3);
        assert!(!exact.truncated, "上限ちょうどなら、一部だけではない");
        let over = walk(root, Limits { max_files: 2, ..Limits::default() }).unwrap();
        assert!(over.truncated);
    }

    // [IDX-003]
    #[test]
    fn limits_files_per_folder_and_file_size() {
        assert_eq!(MAX_FILES_PER_FOLDER, 20_000);
        assert_eq!(MAX_FILE_BYTES, 50 * 1024 * 1024);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for i in 0..5 {
            touch(&root.join(format!("{i}.txt")));
        }
        let big = std::fs::File::create(root.join("big.pdf")).unwrap();
        big.set_len(MAX_FILE_BYTES + 1).unwrap();
        let exact = std::fs::File::create(root.join("exact.pdf")).unwrap();
        exact.set_len(MAX_FILE_BYTES).unwrap();

        let all = walk(root, Limits::default()).unwrap();
        assert_eq!(all.files.len(), 6, "50MB ちょうどまでは入れ、超えたものは入れない");
        assert!(!all.files.iter().any(|f| f.path.ends_with("big.pdf")));

        let limited = walk(root, Limits { max_files: 3, ..Limits::default() }).unwrap();
        assert_eq!(limited.files.len(), 3);
        assert!(limited.truncated);
    }

    /// 足したときの実体は、同じフォルダと、同じ場所で作り直したフォルダを認め、別の場所のフォルダは認めない。
    /// 設定に保存しても損なわない（大きな識別子も、実際の場所も）。
    // [IDX-027][IDX-010]
    #[test]
    fn the_registered_place_admits_only_the_same_folder_or_one_recreated_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("資料");
        touch(&root.join("a.txt"));
        let place = Root::open(&root).unwrap().place();
        assert!(place.admits(&Root::open(&root).unwrap()), "同じフォルダ");

        let stored = serde_json::to_string(&place).unwrap();
        let loaded: Place = serde_json::from_str(&stored).unwrap();
        assert_eq!(loaded, place, "保存して読み直しても同じ");
        let big = Place { real: Some(encoded("/r")), id: (u64::MAX, u128::MAX), born: Some(u128::MAX) };
        assert_eq!(serde_json::from_str::<Place>(&serde_json::to_string(&big).unwrap()).unwrap(), big);
        // 実際の場所のない値は、同じ場所のフォルダかを確かめられないので読まない（選び直してもらう）
        for unverifiable in [Place { real: None, id: (1, 2), born: None }, Place { real: None, id: (1, 2), born: Some(3) }] {
            assert!(serde_json::from_str::<Place>(&serde_json::to_string(&unverifiable).unwrap()).is_err());
        }
        // 実際の場所の書き方が、この OS のものでない・壊れている値も読まない
        let other_os = if cfg!(windows) { "b:2f72" } else { "w:002f" };
        for broken in ["", "b:", "w:", "x:00", "b:zz", "b:0", other_os] {
            let stored = format!(r#"{{"real":"{broken}","volume":"1","file":"2"}}"#);
            assert!(serde_json::from_str::<Place>(&stored).is_err(), "{broken}");
        }
        // 作成日時を記録する前の値も読める
        let real = encoded("/r");
        assert_eq!(
            serde_json::from_str::<Place>(&format!(r#"{{"real":"{real}","volume":"1","file":"2"}}"#)).unwrap(),
            Place { real: Some(real), id: (1, 2), born: None }
        );

        // 別の場所の、同じ名前のフォルダは認めない
        let other = dir.path().join("別").join("資料");
        touch(&other.join("a.txt"));
        assert!(!place.admits(&Root::open(&other).unwrap()));

        // 同じ場所で作り直したフォルダは認める（IDX-010）
        std::fs::rename(&root, dir.path().join("退避")).unwrap();
        touch(&root.join("a.txt"));
        let recreated = Root::open(&root).unwrap();
        assert_ne!(recreated.id(), place.id());
        assert!(place.admits(&recreated));
    }

    /// 足したときと開いたときの両方で実際の場所が取れたら、識別子が同じでも、別の場所のフォルダは認めない
    /// （消したフォルダの識別子が、別の場所のフォルダに使い回された場合）。
    /// どちらかで実際の場所が取れないときは、識別子と作成日時が一致しても認めない。
    // [IDX-027]
    #[test]
    fn a_reused_identifier_at_another_place_is_not_admitted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("資料");
        let other = dir.path().join("別").join("資料");
        touch(&root.join("a.txt"));
        touch(&other.join("a.txt"));
        let at_other = Root::open(&other).unwrap();
        // 足したときの実体の識別子が、いまは別の場所のフォルダに使われている
        let registered = Place { id: at_other.id(), ..Root::open(&root).unwrap().place() };
        assert!(!registered.admits(&at_other), "実際の場所が違えば、識別子が同じでも認めない");
        assert!(registered.admits(&Root::open(&root).unwrap()), "足した場所のフォルダは認める");

        // 開いたときに実際の場所が取れなければ、識別子と作成日時が一致しても認めない
        // （同じ実体を別の場所へ移したもの・識別子が使い回されたものと区別できない）
        let mut opened = Root::open(&root).unwrap();
        let same = opened.place();
        opened.real = None;
        assert!(!same.admits(&opened), "足した場所のフォルダでも、いま実際の場所が取れなければ認めない");
        let mut moved = Root::open(&other).unwrap();
        let by_id = Place { real: None, ..moved.place() };
        assert!(!by_id.admits(&moved), "足したときに実際の場所が取れていなければ認めない");
        moved.real = None;
        assert!(!by_id.admits(&moved), "どちらでも取れなければ認めない");
    }

    /// 開いたあとで、中のフォルダが探すフォルダの外へ移されたら、そのフォルダの中身は読まない
    /// （読むたびに、探すフォルダから名前の並び全体を解決し直す）。
    // [IDX-027]
    #[cfg(target_os = "linux")]
    #[test]
    fn a_subfolder_moved_outside_after_listing_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("資料");
        touch(&root.join("下").join("b.txt"));
        let outside = dir.path().join("外");
        std::fs::create_dir_all(&outside).unwrap();
        let opened = Root::open(&root).unwrap();
        let walked = opened.walk(Limits::default());
        assert_eq!(names(&walked, &root), ["下/b.txt"]);

        std::fs::rename(root.join("下"), outside.join("下")).unwrap();
        for file in &walked.files {
            assert!(bytes(opened.read(&file.path, MAX_FILE_BYTES)).is_none(), "外へ移したフォルダの中身は読まない");
            assert!(!opened.contains_file(&file.path));
        }
        // 途中の名前が探すフォルダの中を指すリンクでも、たどらない
        std::os::unix::fs::symlink("../外/下", root.join("下")).unwrap();
        assert!(matches!(opened.read(&root.join("下").join("b.txt"), MAX_FILE_BYTES), Content::Outside));
        std::fs::remove_file(root.join("下")).unwrap();
        std::fs::create_dir(root.join("内")).unwrap();
        std::fs::write(root.join("内").join("c.txt"), "経費").unwrap();
        std::os::unix::fs::symlink("内", root.join("別名")).unwrap();
        assert!(matches!(opened.read(&root.join("別名").join("c.txt"), MAX_FILE_BYTES), Content::Outside));
        assert_eq!(bytes(opened.read(&root.join("内").join("c.txt"), MAX_FILE_BYTES)).unwrap(), "経費".as_bytes());
    }

    /// 探すフォルダの中にあると OS で確かめられない環境（openat2 がない）では、中身を読まず、列挙もしない。
    // [IDX-027]
    #[cfg(target_os = "linux")]
    #[test]
    fn nothing_is_read_when_the_folder_boundary_cannot_be_verified() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("a.txt"));
        touch(&root.join("下").join("b.txt"));
        let opened = Root::open(root).unwrap();
        sys::NO_OPENAT2.with(|flag| flag.set(true));
        let read = opened.read(&root.join("a.txt"), MAX_FILE_BYTES);
        let nested = opened.read(&root.join("下").join("b.txt"), MAX_FILE_BYTES);
        let contains = opened.contains_file(&root.join("a.txt"));
        let walked = opened.walk(Limits::default());
        sys::NO_OPENAT2.with(|flag| flag.set(false));
        assert!(bytes(read).is_none(), "中身を読まない");
        assert!(bytes(nested).is_none());
        assert!(!contains);
        assert_eq!(names(&walked, root), ["a.txt"], "探すフォルダそのものの列挙だけ（開いたハンドルなので確かめられる）");
        assert_eq!(walked.unreadable, [root.join("下")], "中のフォルダは読めなかったことにする（索引の行は消さない）");
        assert_eq!(bytes(opened.read(&root.join("a.txt"), MAX_FILE_BYTES)).unwrap(), "経費".as_bytes());
    }

    /// 大文字と小文字を区別するフォルダでは、名前が大文字と小文字だけ違うフォルダとファイルを、別のものとして扱う
    /// （開いたものの場所を、パスの文字列で比べない）。
    // [IDX-027]
    #[cfg(windows)]
    #[test]
    fn names_that_differ_only_in_case_are_different_places() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("区別");
        std::fs::create_dir(&parent).unwrap();
        let enabled = std::process::Command::new("fsutil.exe")
            .args(["file", "setCaseSensitiveInfo"])
            .arg(&parent)
            .arg("enable")
            .output()
            .is_ok_and(|output| output.status.success());
        if !enabled {
            eprintln!("大文字と小文字を区別するフォルダを作れない環境のため、確かめません");
            return;
        }
        let upper = parent.join("Docs");
        let lower = parent.join("docs");
        std::fs::create_dir(&upper).unwrap();
        std::fs::create_dir(&lower).unwrap();
        std::fs::write(upper.join("a.txt"), "経費").unwrap();
        std::fs::write(lower.join("a.txt"), "社外秘").unwrap();
        std::fs::create_dir(upper.join("Sub")).unwrap();
        std::fs::create_dir(upper.join("sub")).unwrap();
        std::fs::write(upper.join("Sub").join("b.txt"), "経費").unwrap();
        std::fs::write(upper.join("sub").join("b.txt"), "予算").unwrap();

        let opened = Root::open(&upper).unwrap();
        assert!(!opened.place().admits(&Root::open(&lower).unwrap()), "大文字と小文字だけ違うフォルダは、別の場所");
        assert_eq!(bytes(opened.read(&upper.join("a.txt"), MAX_FILE_BYTES)).unwrap(), "経費".as_bytes());
        assert_eq!(bytes(opened.read(&upper.join("Sub").join("b.txt"), MAX_FILE_BYTES)).unwrap(), "経費".as_bytes());
        assert_eq!(bytes(opened.read(&upper.join("sub").join("b.txt"), MAX_FILE_BYTES)).unwrap(), "予算".as_bytes());
        let walked = opened.walk(Limits::default());
        assert_eq!(names(&walked, &upper), ["a.txt", "Sub/b.txt", "sub/b.txt"]);
    }

    /// フォルダを開けたあとで、実際の場所の問い合わせ（Linux の `read_link`、Windows の `GetFinalPathNameByHandleW`）が
    /// 失敗しても、開けなかったことにしない。実際の場所のないフォルダとして返し、足したときの実体とは認めない。
    // [IDX-027]
    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn a_failed_real_place_lookup_opens_the_folder_without_a_place() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("資料");
        std::fs::create_dir_all(&root).unwrap();
        let registered = Root::open(&root).unwrap().place();
        test_hooks::hide_real(&root, true);
        let opened = Root::open(&root);
        test_hooks::hide_real(&root, false);
        let opened = opened.expect("開けたフォルダは、実際の場所が取れなくてもエラーにしない");
        assert_eq!(opened.real(), None);
        assert!(opened.place().is_unverifiable(), "足すときは、確かめられないフォルダとして断る");
        assert!(!registered.admits(&opened), "足したときと同じ場所かを確かめられない");
    }

    // [IDX-010]
    #[test]
    fn missing_folder_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(walk(&dir.path().join("ない"), Limits::default()).is_err());
    }
}
