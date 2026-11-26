//! 資料の索引の結合テスト: 実際のフォルダとファイルの索引（search-index.db）を相手に、
//! 裏での作成・フォルダの見張り・作り直し・オフにしたときの削除を確かめる。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use kiroscribe_core::search::store::SearchIndex;
use kiroscribe_core::search::{IndexState, IndexStatus, Indexer, SearchFolder, INDEX_FILE};

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    index_path: PathBuf,
    indexer: Indexer,
    events: Arc<Mutex<Vec<IndexStatus>>>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("共有資料");
    std::fs::create_dir_all(&root).unwrap();
    let index_path = dir.path().join("data").join(INDEX_FILE);
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let indexer = Indexer::spawn(index_path.clone(), move |status| sink.lock().unwrap().push(status.clone()));
    Fixture { _dir: dir, root, index_path, indexer, events }
}

fn folder(path: &Path) -> SearchFolder {
    SearchFolder { path: path.to_string_lossy().to_string(), no_content: false }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn wait_until(what: &str, timeout: Duration, mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while !check() {
        assert!(start.elapsed() < timeout, "{what} になりませんでした");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_ready(f: &Fixture, files: usize) {
    wait_until(&format!("索引に {files} 件"), Duration::from_secs(20), || {
        let status = f.indexer.status();
        status.state == IndexState::Ready && status.files == files
    });
}

/// 別の接続で索引を開いて検索する（作業用のスレッドが書いた内容が、ファイルに残っていることも確かめる）。
fn search(f: &Fixture, term: &str) -> Vec<String> {
    let index = SearchIndex::open(&f.index_path).unwrap();
    index
        .search(&[term.to_string()], 8)
        .unwrap()
        .into_iter()
        .map(|hit| hit.path.file_name().unwrap().to_string_lossy().to_string())
        .collect()
}

// [IDX-008]
#[test]
fn builds_in_the_background_only_when_enabled() {
    let f = fixture();
    for i in 0..30 {
        write(&f.root.join(format!("{i:02}.txt")), &format!("経費 {i}"));
    }
    // 相棒がオフのあいだは作らない
    f.indexer.set_folders(vec![folder(&f.root)]);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(f.indexer.status().state, IndexState::Off);
    assert!(!f.index_path.exists(), "オフのあいだは索引のファイルも作らない");

    f.indexer.enable(vec![folder(&f.root)]);
    wait_ready(&f, 30);
    let status = f.indexer.status();
    assert!(status.updated_at.is_some(), "更新した時刻");
    assert_eq!(status.folders[0].files, 30);
    let events = f.events.lock().unwrap().clone();
    assert!(events.iter().any(|s| s.state == IndexState::Building), "作っている途中の状態を知らせる");
    assert_eq!(search(&f, "経費").len(), 8);
}

// [IDX-009][IDX-007]
#[test]
fn watches_the_folder_and_reflects_changes() {
    let f = fixture();
    let manual = f.root.join("手順").join("経費精算.txt");
    write(&manual, "申請は月末まで");
    let before = std::fs::metadata(&manual).unwrap().modified().unwrap();
    f.indexer.enable(vec![folder(&f.root)]);
    wait_ready(&f, 1);
    assert_eq!(std::fs::read_to_string(&manual).unwrap(), "申請は月末まで", "ファイルは書き換えない");
    assert_eq!(std::fs::metadata(&manual).unwrap().modified().unwrap(), before, "更新日時も変えない");

    // 足された
    write(&f.root.join("出張.txt"), "宿泊の上限");
    wait_until("足したファイルの反映", Duration::from_secs(60), || search(&f, "宿泊") == ["出張.txt"]);
    // 変わった
    write(&manual, "申請は翌月 5 日まで");
    wait_until("変えたファイルの反映", Duration::from_secs(60), || search(&f, "翌月") == ["経費精算.txt"]);
    // 消えた（下のフォルダごと）
    std::fs::remove_dir_all(f.root.join("手順")).unwrap();
    wait_until("消したファイルの反映", Duration::from_secs(60), || search(&f, "申請").is_empty());
    wait_ready(&f, 1);
}

/// 更新日時と大きさが変わっていないファイルは読み直さない（作り直すときだけ、すべて読み直す）。
// [IDX-009][IDX-011]
#[test]
fn unchanged_files_are_not_reread_until_rebuild() {
    let f = fixture();
    let file = f.root.join("議事録.txt");
    write(&file, "予算はＡ案");
    let stamp = SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options().write(true).open(&file).unwrap().set_modified(stamp).unwrap();
    f.indexer.enable(vec![folder(&f.root)]);
    wait_ready(&f, 1);
    assert_eq!(search(&f, "a案"), ["議事録.txt"]);

    // 同じ大きさで中身を変え、更新日時を戻す（見張りの知らせが届いても、読み直さない）
    write(&file, "予算はＢ案");
    std::fs::File::options().write(true).open(&file).unwrap().set_modified(stamp).unwrap();
    std::thread::sleep(Duration::from_secs(4));
    f.indexer.set_folders(vec![folder(&f.root)]);
    wait_ready(&f, 1);
    assert_eq!(search(&f, "a案"), ["議事録.txt"], "変わっていないとみなす");

    assert!(f.indexer.rebuild());
    wait_until("作り直し", Duration::from_secs(20), || search(&f, "b案") == ["議事録.txt"]);
    assert!(search(&f, "a案").is_empty());
}

// [IDX-010]
#[test]
fn removing_a_folder_drops_its_files_but_a_missing_folder_keeps_them() {
    let f = fixture();
    let other = f.root.parent().unwrap().join("人事");
    write(&f.root.join("a.txt"), "経費");
    write(&other.join("b.txt"), "経費");
    f.indexer.enable(vec![folder(&f.root), folder(&other)]);
    wait_ready(&f, 2);

    f.indexer.set_folders(vec![folder(&f.root)]);
    wait_ready(&f, 1);
    assert_eq!(search(&f, "経費"), ["a.txt"], "外したフォルダのファイルは消す");

    // 見つからない（ドライブが外れたなど）
    let moved = f.root.parent().unwrap().join("退避");
    std::fs::rename(&f.root, &moved).unwrap();
    f.indexer.set_folders(vec![folder(&f.root)]);
    wait_until("見つからない表示", Duration::from_secs(20), || {
        let status = f.indexer.status();
        status.state == IndexState::Ready && status.folders[0].missing
    });
    assert_eq!(search(&f, "経費"), ["a.txt"], "索引は消さずに残す");
    assert_eq!(f.indexer.status().folders[0].files, 1);
}

// [IDX-003]
#[test]
fn partly_indexed_folder_is_marked() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("多い");
    for i in 0..5 {
        write(&root.join(format!("{i}.txt")), "x");
    }
    let limits = kiroscribe_core::search::walk::Limits { max_files: 3, ..Default::default() };
    let indexer = Indexer::spawn_with(dir.path().join(INDEX_FILE), limits, |_| {});
    indexer.enable(vec![folder(&root)]);
    wait_until("一部だけ索引に入れた表示", Duration::from_secs(20), || {
        let status = indexer.status();
        status.state == IndexState::Ready && status.folders[0].truncated && status.files == 3
    });
}

// [IDX-028][IDX-011]
#[test]
fn turning_off_clears_the_index_and_keeps_the_folders() {
    let f = fixture();
    write(&f.root.join("a.txt"), "経費");
    f.indexer.enable(vec![folder(&f.root)]);
    wait_ready(&f, 1);

    f.indexer.disable();
    wait_until("オフ", Duration::from_secs(20), || f.indexer.status().state == IndexState::Off);
    assert!(search(&f, "経費").is_empty(), "索引の中身を消す");
    let status = f.indexer.status();
    assert_eq!(status.folders.len(), 1, "探すフォルダの一覧は残す");
    assert_eq!(status.files, 0);
    assert!(!f.indexer.rebuild(), "オフのときは作り直さない");

    // オフのあいだの変更は反映しない（見張りも止まる）
    write(&f.root.join("b.txt"), "経費");
    std::thread::sleep(Duration::from_secs(4));
    assert!(search(&f, "経費").is_empty());

    // 次にオンにしたときに作り直す
    f.indexer.enable(vec![folder(&f.root)]);
    wait_ready(&f, 2);
}
