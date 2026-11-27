//! 追加したフォルダの中の、索引に入れるファイルを集める。
//!
//! 隠しフォルダ・システムのフォルダ・「.」で始まるフォルダ・`node_modules`・ごみ箱は下りない。
//! シンボリックリンク・ジャンクション・ショートカット（.lnk）はたどらない。
//! OneDrive の同期したファイルのような、別の場所を指さない再解析ポイントは、ふつうのファイルとして扱う。

use std::fs::Metadata;
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

/// 1 つのファイルを、索引に入れるものとして調べる（入れないなら None）。見張りで届いた変更にも使う。
pub fn inspect_file(path: &Path, limits: Limits) -> Option<FileEntry> {
    let name = path.file_name()?.to_str()?;
    if is_skipped_file_name(name) {
        return None;
    }
    let kind = kind_of(path)?;
    let meta = std::fs::symlink_metadata(path).ok()?;
    let traits = traits_of(&meta);
    if !meta.is_file() || traits.link || traits.hidden_or_system || meta.len() > limits.max_bytes {
        return None;
    }
    Some(FileEntry { path: path.to_path_buf(), kind, modified: modified_ms(&meta), size: meta.len(), online_only: traits.online_only })
}

/// `path` が `root` の中にあり、途中のフォルダを含めて、リンクをたどらずに届き、除くフォルダを通らないか。
/// 足したあとでフォルダそのものがリンクに替わったときも、リンクの先を範囲に入れない。
pub fn reachable_without_links(root: &Path, path: &Path) -> bool {
    let Ok(rest) = path.strip_prefix(root) else {
        return false;
    };
    if !is_plain_dir(root) {
        return false;
    }
    let mut current = root.to_path_buf();
    let parts: Vec<_> = rest.components().collect();
    for (index, part) in parts.iter().enumerate() {
        let std::path::Component::Normal(name) = part else {
            return false;
        };
        current.push(name);
        let Ok(meta) = std::fs::symlink_metadata(&current) else {
            return false;
        };
        let traits = traits_of(&meta);
        if traits.link {
            return false;
        }
        let last = index + 1 == parts.len();
        if !last && (traits.hidden_or_system || name.to_str().is_none_or(is_skipped_dir_name)) {
            return false;
        }
    }
    true
}

/// クラウドにだけあるファイルか（読む直前に確かめる。読むとダウンロードが始まるため）。
pub fn is_online_only(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| traits_of(&meta).online_only)
}

/// そのパスそのものが、リンクでないフォルダか（リンクをたどらずに確かめる）。
pub fn is_plain_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir() && !traits_of(&meta).link)
}

/// フォルダの中のファイルを、リンクをたどらずに届くときだけ読む（範囲の外なら None）。
/// 調べてから開くまでのあいだに途中のフォルダがリンクに替わっても、範囲の外の中身を返さないよう、
/// 開いたファイルが調べたファイルと同じかと、読み終えたあとにも範囲の中にあるかを確かめる。
pub fn read_within(root: &Path, path: &Path) -> Option<std::io::Result<Vec<u8>>> {
    use std::io::Read;
    if !reachable_without_links(root, path) {
        return None;
    }
    let before = std::fs::symlink_metadata(path).ok()?;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) => return Some(Err(err)),
    };
    let opened = file.metadata().ok()?;
    if !same_file(&before, &opened) {
        return None;
    }
    let mut bytes = Vec::new();
    if let Err(err) = file.read_to_end(&mut bytes) {
        return Some(Err(err));
    }
    reachable_without_links(root, path).then_some(Ok(bytes))
}

/// リンクをたどらずに調べたファイルと、開いたファイルが同じか。
fn same_file(before: &Metadata, opened: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        before.is_file() && before.dev() == opened.dev() && before.ino() == opened.ino()
    }
    #[cfg(not(unix))]
    {
        before.is_file() && !traits_of(before).link && opened.is_file()
    }
}

/// そのパスそのものが、たどらないリンク（シンボリックリンク・ジャンクション）か。
pub fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| traits_of(&meta).link)
}

/// フォルダの中（下のフォルダを含む）の、索引に入れるファイルを集める。
/// フォルダがないとき・フォルダそのものがリンクのとき（リンクの先はたどらない）はエラー。
pub fn walk(root: &Path, limits: Limits) -> std::io::Result<Walked> {
    let meta = std::fs::symlink_metadata(root)?;
    if traits_of(&meta).link {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "リンクのフォルダはたどりません"));
    }
    if !meta.is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotADirectory, "フォルダではありません"));
    }
    let mut walked = Walked::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            walked.unreadable.push(dir);
            continue;
        };
        let Ok(mut entries) = entries.collect::<std::io::Result<Vec<_>>>() else {
            walked.unreadable.push(dir);
            continue;
        };
        entries.sort_by_key(|entry| entry.file_name());
        let mut subdirs = Vec::new();
        for entry in entries {
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            let traits = traits_of(&meta);
            if traits.link || traits.hidden_or_system {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if meta.is_dir() {
                if !is_skipped_dir_name(name) {
                    subdirs.push(entry.path());
                }
                continue;
            }
            if let Some(file) = inspect_file(&entry.path(), limits) {
                if walked.files.len() >= limits.max_files {
                    walked.truncated = true;
                    return Ok(walked);
                }
                walked.files.push(file);
            }
        }
        // 名前の順にたどる（スタックなので逆に積む）
        stack.extend(subdirs.into_iter().rev());
    }
    Ok(walked)
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
        assert!(!reachable_without_links(root, &root.join("リンク").join("外.txt")));
        assert!(!reachable_without_links(root, &root.join("外へ.txt")));
        assert!(reachable_without_links(root, &root.join("中.txt")));
        assert!(!reachable_without_links(root, &outside.path().join("外.txt")), "フォルダの外");

        // 追加したフォルダそのものがリンクでも、リンクの先はたどらない
        let linked = dir.path().join("リンクのフォルダ");
        std::os::unix::fs::symlink(outside.path(), &linked).unwrap();
        assert!(is_link(&linked));
        assert!(!is_link(root));
        assert!(walk(&linked, Limits::default()).is_err());
        assert!(!is_plain_dir(&linked) && is_plain_dir(root));
        // フォルダそのものがリンクなら、リンクの先のファイルは範囲の外（読まない）
        assert!(!reachable_without_links(&linked, &linked.join("外.txt")));
        assert!(read_within(&linked, &linked.join("外.txt")).is_none());
        assert_eq!(read_within(root, &root.join("中.txt")).unwrap().unwrap(), "経費".as_bytes());
        assert!(read_within(root, &root.join("外へ.txt")).is_none());
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
        assert!(!reachable_without_links(root, &root.join("ジャンクション").join("外.txt")));
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
