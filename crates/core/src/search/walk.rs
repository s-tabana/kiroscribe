//! 追加したフォルダの中の、索引に入れるファイルを集める。
//!
//! 隠しフォルダ・システムのフォルダ・「.」で始まるフォルダ・`node_modules`・ごみ箱は下りない。
//! シンボリックリンク・ジャンクション・ショートカット（.lnk）はたどらない。
//! OneDrive の同期したファイルのような、別の場所を指さない再解析ポイントは、ふつうのファイルとして扱う。

use std::ffi::OsString;
use std::fs::{File, Metadata};
use std::io;
use std::path::{Path, PathBuf};

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

/// 索引に入れるファイルか（名前・種類・属性・大きさで決める）。入れないなら None。
fn entry_of(path: &Path, meta: &Metadata, limits: Limits) -> Option<FileEntry> {
    let name = path.file_name()?.to_str()?;
    if is_skipped_file_name(name) {
        return None;
    }
    let kind = kind_of(path)?;
    let traits = traits_of(meta);
    if !meta.is_file() || traits.link || traits.hidden_or_system || meta.len() > limits.max_bytes {
        return None;
    }
    Some(FileEntry { path: path.to_path_buf(), kind, modified: modified_ms(meta), size: meta.len(), online_only: traits.online_only })
}

/// クラウドにだけあるファイルか（読む直前に確かめる。読むとダウンロードが始まるため）。
pub fn is_online_only(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| traits_of(&meta).online_only)
}

/// そのパスそのものが、たどらないリンク（シンボリックリンク・ジャンクション）か。
pub fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| traits_of(&meta).link)
}

/// ファイル・フォルダの識別子（Unix はデバイスと i ノードの番号、Windows はボリュームのシリアル番号とファイル ID）。
/// 移したフォルダの代わりに、同じ名前のフォルダが作られたら変わる。
pub type FileId = (u64, u64);

/// 2 つの実際の場所が同じか（Windows では大文字と小文字を区別しない）。
pub fn same_place(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase()
    }
    #[cfg(not(windows))]
    {
        a == b
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
    /// 読めなかった（権限がない・大きくなりすぎたなど）
    Failed(io::Error),
}

/// 開いた探すフォルダ。中のフォルダとファイルは、このフォルダのハンドルを基準にたどる。
/// 実際に開いたものがこのフォルダの中にあると確かめてから、列挙したり読んだりする（確かめられなければ読まない）。
pub struct Root {
    path: PathBuf,
    dir: File,
    id: FileId,
    real: PathBuf,
}

impl Root {
    /// 探すフォルダを開く。フォルダがない・フォルダそのものがリンク・フォルダでないときはエラー。
    pub fn open(path: &Path) -> io::Result<Root> {
        let (dir, id, real) = sys::open_root(path)?;
        Ok(Root { path: path.to_path_buf(), dir, id, real })
    }

    /// 足したときのパス（索引の行のパスは、これを基準にする）。
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn id(&self) -> FileId {
        self.id
    }

    /// 開いたときの実際の場所（上のフォルダのリンクをたどった先も含めた、本当の場所）。
    pub fn real(&self) -> &Path {
        &self.real
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
    /// 名前と属性で決める（リンクかどうかと、実際にどこにあるかは、開くときに確かめる）。
    pub fn allows(&self, path: &Path) -> bool {
        let Some(parts) = self.parts(path) else { return false };
        let mut current = self.path.clone();
        parts[..parts.len() - 1].iter().all(|part| {
            current.push(part);
            std::fs::symlink_metadata(&current).is_ok_and(|meta| !traits_of(&meta).hidden_or_system)
        })
    }

    /// このフォルダの中のファイル・フォルダの属性を、リンクをたどらずに調べる。範囲の外なら [`is_outside`] のエラー。
    pub fn stat(&self, path: &Path) -> io::Result<Metadata> {
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
        self.stat(path).is_ok_and(|meta| meta.is_file() && !traits_of(&meta).link)
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
        let meta = match file.metadata() {
            Ok(meta) => meta,
            Err(err) => return Content::Failed(err),
        };
        if !meta.is_file() {
            return Content::Outside;
        }
        if traits_of(&meta).online_only {
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
            for (name, meta) in entries {
                let traits = traits_of(&meta);
                if traits.link || traits.hidden_or_system {
                    continue;
                }
                let Some(text) = name.to_str() else {
                    continue;
                };
                if meta.is_dir() {
                    if !is_skipped_dir_name(text) {
                        let mut sub = parts.clone();
                        sub.push(name.clone());
                        subdirs.push(sub);
                    }
                    continue;
                }
                if let Some(file) = entry_of(&dir_path.join(&name), &meta, limits) {
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
fn listed(name: OsString, meta: io::Result<Metadata>) -> io::Result<Option<(OsString, Metadata)>> {
    match meta {
        Ok(meta) => Ok(Some((name, meta))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Linux: 開いたフォルダのハンドル（`/proc/self/fd/<番号>`）を基準に、名前を 1 つずつ、リンクをたどらずに開く。
#[cfg(target_os = "linux")]
mod sys {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    fn handle_path(file: &File) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
    }

    /// リンクを開こうとした（ELOOP）・フォルダでなかった（ENOTDIR）ときは、範囲の外にする。
    fn link_as_outside(err: io::Error) -> io::Error {
        match err.raw_os_error() {
            Some(libc::ELOOP) | Some(libc::ENOTDIR) => outside(),
            _ => err,
        }
    }

    fn open_dir(path: &Path) -> io::Result<File> {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(link_as_outside)
    }

    pub(super) fn open_root(path: &Path) -> io::Result<(File, FileId, PathBuf)> {
        let dir = open_dir(path).map_err(|err| {
            if is_outside(&err) {
                io::Error::new(io::ErrorKind::InvalidInput, "リンクのフォルダはたどりません")
            } else {
                err
            }
        })?;
        let meta = dir.metadata()?;
        let real = std::fs::read_link(handle_path(&dir))?;
        Ok((dir, (meta.dev(), meta.ino()), real))
    }

    fn open_dirs(root: &Root, dirs: &[OsString]) -> io::Result<File> {
        let mut dir = root.dir.try_clone()?;
        for name in dirs {
            dir = open_dir(&handle_path(&dir).join(name))?;
        }
        Ok(dir)
    }

    pub(super) fn stat_child(root: &Root, dirs: &[OsString], name: &OsString) -> io::Result<Metadata> {
        let dir = open_dirs(root, dirs)?;
        std::fs::symlink_metadata(handle_path(&dir).join(name))
    }

    pub(super) fn open_file(root: &Root, parts: &[OsString]) -> io::Result<File> {
        let (name, dirs) = parts.split_last().ok_or_else(outside)?;
        let dir = open_dirs(root, dirs)?;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC)
            .open(handle_path(&dir).join(name))
            .map_err(link_as_outside)
    }

    pub(super) fn list(root: &Root, dirs: &[OsString]) -> io::Result<Vec<(OsString, Metadata)>> {
        let dir = open_dirs(root, dirs)?;
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(handle_path(&dir))? {
            let entry = entry?;
            // 列挙したハンドルを基準に、リンクをたどらずに調べる
            entries.extend(listed(entry.file_name(), entry.metadata())?);
        }
        Ok(entries)
    }
}

/// Windows: 開いたファイル・フォルダの実際の場所（`GetFinalPathNameByHandleW`）が、
/// 探すフォルダのいまの実際の場所の下の、同じ名前の並びと一致するかを確かめる（途中にリンクがあると一致しない）。
#[cfg(windows)]
mod sys {
    use super::*;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, GetFinalPathNameByHandleW, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED, VOLUME_NAME_DOS,
    };

    /// フォルダそのものを開く（リンクなら、リンクの先ではなくリンクそのものを開く）。
    fn open_dir(path: &Path) -> io::Result<File> {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }

    fn file_id(file: &File) -> io::Result<FileId> {
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((u64::from(info.dwVolumeSerialNumber), (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow)))
    }

    fn final_path(file: &File) -> io::Result<PathBuf> {
        let mut buf = vec![0u16; 512];
        loop {
            let len = unsafe {
                GetFinalPathNameByHandleW(file.as_raw_handle(), buf.as_mut_ptr(), buf.len() as u32, FILE_NAME_NORMALIZED | VOLUME_NAME_DOS)
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

    /// 開いたものが、探すフォルダのいまの実際の場所から、名前の並び `parts` で届く場所にあるか。
    fn located(root: &Root, opened: &File, parts: &[OsString]) -> io::Result<()> {
        let expected = parts.iter().fold(final_path(&root.dir)?, |path, part| path.join(part));
        if same_place(&final_path(opened)?, &expected) {
            Ok(())
        } else {
            Err(outside())
        }
    }

    fn join(root: &Root, parts: &[OsString]) -> PathBuf {
        parts.iter().fold(root.path.clone(), |path, part| path.join(part))
    }

    pub(super) fn open_root(path: &Path) -> io::Result<(File, FileId, PathBuf)> {
        let dir = open_dir(path)?;
        let meta = dir.metadata()?;
        if traits_of(&meta).link {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "リンクのフォルダはたどりません"));
        }
        if !meta.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, "フォルダではありません"));
        }
        let id = file_id(&dir)?;
        let real = final_path(&dir)?;
        Ok((dir, id, real))
    }

    /// 中のフォルダを開き、リンクでないフォルダで、探すフォルダの中にあると確かめる。
    fn open_dirs(root: &Root, dirs: &[OsString]) -> io::Result<File> {
        let dir = open_dir(&join(root, dirs))?;
        let meta = dir.metadata()?;
        if traits_of(&meta).link || !meta.is_dir() {
            return Err(outside());
        }
        located(root, &dir, dirs)?;
        Ok(dir)
    }

    pub(super) fn stat_child(root: &Root, dirs: &[OsString], name: &OsString) -> io::Result<Metadata> {
        let path = join(root, dirs).join(name);
        open_dirs(root, dirs)?;
        std::fs::symlink_metadata(path)
    }

    pub(super) fn open_file(root: &Root, parts: &[OsString]) -> io::Result<File> {
        let file = File::open(join(root, parts))?;
        located(root, &file, parts)?;
        Ok(file)
    }

    pub(super) fn list(root: &Root, dirs: &[OsString]) -> io::Result<Vec<(OsString, Metadata)>> {
        let dir = open_dirs(root, dirs)?;
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(join(root, dirs))? {
            let entry = entry?;
            entries.extend(listed(entry.file_name(), entry.metadata())?);
        }
        // 列挙したあとも、同じフォルダが同じ場所にある（列挙のあいだに、別のフォルダに替わっていない）
        located(root, &dir, dirs)?;
        if file_id(&open_dir(&join(root, dirs))?)? != file_id(&dir)? {
            return Err(outside());
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

    pub(super) fn open_root(_: &Path) -> io::Result<(File, FileId, PathBuf)> {
        Err(unsupported())
    }

    pub(super) fn stat_child(_: &Root, _: &[OsString], _: &OsString) -> io::Result<Metadata> {
        Err(unsupported())
    }

    pub(super) fn open_file(_: &Root, _: &[OsString]) -> io::Result<File> {
        Err(unsupported())
    }

    pub(super) fn list(_: &Root, _: &[OsString]) -> io::Result<Vec<(OsString, Metadata)>> {
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
        let real = opened.real().to_path_buf();

        std::fs::rename(dir.path().join("上"), dir.path().join("退避")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("上")).unwrap();
        assert_eq!(bytes(opened.read(&root.join("a.txt"), MAX_FILE_BYTES)).unwrap(), "経費".as_bytes(), "開いたフォルダの中身");
        assert!(!opened.contains_file(&root.join("外.txt")), "別の場所のファイルは、開いたフォルダの中にない");
        assert!(bytes(opened.read(&root.join("外.txt"), MAX_FILE_BYTES)).is_none());
        let names: Vec<_> = opened.walk(Limits::default()).files.iter().map(|f| f.path.file_name().unwrap().to_owned()).collect();
        assert_eq!(names, ["a.txt"]);

        let reopened = Root::open(&root).unwrap();
        assert!(!same_place(reopened.real(), &real), "開き直すと、別の場所にあると分かる");
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

    /// 開いたあとで、上のフォルダがジャンクションに替わり、別の場所の同じ名前のフォルダを指すようになっても、
    /// 別の場所の中身を読まない。開き直すと、実際の場所が変わったと分かる。
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
        let real = opened.real().to_path_buf();
        assert_eq!(bytes(opened.read(&root.join("a.txt"), MAX_FILE_BYTES)).unwrap(), "経費".as_bytes());

        std::fs::rename(dir.path().join("上"), dir.path().join("退避")).unwrap();
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(dir.path().join("上"))
            .arg(outside.path())
            .status()
            .unwrap();
        assert!(status.success(), "ジャンクションを作れません");
        let content = opened.read(&root.join("a.txt"), MAX_FILE_BYTES);
        assert!(matches!(content, Content::Outside), "{content:?}");
        assert!(!opened.contains_file(&root.join("a.txt")));
        assert!(opened.walk(Limits::default()).files.is_empty());
        let reopened = Root::open(&root).unwrap();
        assert!(!same_place(reopened.real(), &real), "開き直すと、別の場所にあると分かる");
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

    // [IDX-010]
    #[test]
    fn missing_folder_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(walk(&dir.path().join("ない"), Limits::default()).is_err());
    }
}
