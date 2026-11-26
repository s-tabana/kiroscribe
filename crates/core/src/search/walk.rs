//! 追加したフォルダの中の、索引に入れるファイルを集める。
//!
//! 隠しフォルダ・システムのフォルダ・「.」で始まるフォルダ・`node_modules`・ごみ箱は下りない。
//! シンボリックリンク・ジャンクション（Windows の再解析ポイント）・ショートカット（.lnk）はたどらない。

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
}

#[derive(Debug, Default)]
pub struct Walked {
    pub files: Vec<FileEntry>,
    /// 上限（[`MAX_FILES_PER_FOLDER`]）を超えたため、一部だけを集めた
    pub truncated: bool,
}

/// 再解析ポイント（シンボリックリンク・ジャンクションなど）か。
fn is_link(meta: &Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// 隠し属性かシステム属性か（Windows だけ。ほかの OS では、名前の「.」で決める）。
fn is_hidden_or_system(_meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
        if _meta.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0 {
            return true;
        }
    }
    false
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
    if !meta.is_file() || is_link(&meta) || is_hidden_or_system(&meta) || meta.len() > limits.max_bytes {
        return None;
    }
    Some(FileEntry { path: path.to_path_buf(), kind, modified: modified_ms(&meta), size: meta.len() })
}

/// `path` が `root` の中にあり、途中のフォルダを含めて、リンクをたどらずに届き、除くフォルダを通らないか。
pub fn reachable_without_links(root: &Path, path: &Path) -> bool {
    let Ok(rest) = path.strip_prefix(root) else {
        return false;
    };
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
        if is_link(&meta) {
            return false;
        }
        let last = index + 1 == parts.len();
        if !last && (is_hidden_or_system(&meta) || name.to_str().is_none_or(is_skipped_dir_name)) {
            return false;
        }
    }
    true
}

/// フォルダの中（下のフォルダを含む）の、索引に入れるファイルを集める。フォルダがなければエラー。
pub fn walk(root: &Path, limits: Limits) -> std::io::Result<Walked> {
    let meta = std::fs::metadata(root)?;
    if !meta.is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotADirectory, "フォルダではありません"));
    }
    let mut walked = Walked::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        let mut subdirs = Vec::new();
        for entry in entries {
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if is_link(&meta) || is_hidden_or_system(&meta) {
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
