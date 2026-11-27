//! 資料の索引の結合テスト: 実際のフォルダとファイルの索引（search-index.db）を相手に、
//! 裏での作成・フォルダの見張り・作り直し・オフにしたときの削除を確かめる。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use kiroscribe_core::search::store::SearchIndex;
use kiroscribe_core::search::walk::Limits;
use kiroscribe_core::search::{IndexState, IndexStatus, Indexer, Intervals, SearchFolder, INDEX_FILE};

/// 知らせた状態と、知らせた時点で索引のファイルに資料の中身（[`MARKER`]）が残っていたか。
type Events = Arc<Mutex<Vec<(IndexStatus, bool)>>>;

/// 資料の中身に入れておく、ファイルに残っていないかを確かめるための文字。
const MARKER: &str = "KIROSCRIBE-MARKER";

// 作業用のスレッド（indexer）を、一時フォルダ（_dir）より先に止める（フィールドは書いた順に片付く）
struct Fixture {
    indexer: Indexer,
    root: PathBuf,
    index_path: PathBuf,
    events: Events,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    fixture_with(Intervals::default())
}

fn fixture_with(intervals: Intervals) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("共有資料");
    std::fs::create_dir_all(&root).unwrap();
    let index_path = dir.path().join("data").join(INDEX_FILE);
    let events: Events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let path = index_path.clone();
    let indexer = Indexer::spawn_with_intervals(index_path.clone(), Limits::default(), intervals, move |status| {
        sink.lock().unwrap().push((status.clone(), left_on_disk(&path)));
    });
    Fixture { indexer, root, index_path, events, _dir: dir }
}

/// 索引のファイル（本体・-wal・-shm）のどこかに、資料の中身（[`MARKER`]）が残っているか。
fn left_on_disk(index_path: &Path) -> bool {
    ["", "-wal", "-shm"].iter().any(|suffix| {
        let mut name = index_path.as_os_str().to_os_string();
        name.push(suffix);
        std::fs::read(PathBuf::from(name)).is_ok_and(|bytes| bytes.windows(MARKER.len()).any(|w| w == MARKER.as_bytes()))
    })
}

fn events(f: &Fixture) -> Vec<IndexStatus> {
    f.events.lock().unwrap().iter().map(|(status, _)| status.clone()).collect()
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
    assert!(events(&f).iter().any(|s| s.state == IndexState::Building), "作っている途中の状態を知らせる");
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

/// オフにしたら、索引のファイルを消し終えてから「オフ」を知らせる。資料の中身は、本体にも -wal にも残らない。
// [IDX-028]
#[test]
fn turning_off_erases_the_files_before_reporting_off() {
    let f = fixture();
    for i in 0..20 {
        write(&f.root.join(format!("{i}.txt")), &format!("{MARKER} 経費 {i} {}", "x".repeat(3_000)));
    }
    f.indexer.enable(vec![folder(&f.root)]);
    wait_ready(&f, 20);
    assert!(left_on_disk(&f.index_path), "作ったあとは、ファイルにある");

    // オフにした直後に一覧を変えても、消し終える前に「オフ」を知らせない
    let before = f.events.lock().unwrap().len();
    f.indexer.disable();
    f.indexer.set_folders(vec![folder(&f.root)]);
    wait_until("オフ", Duration::from_secs(20), || f.indexer.status().state == IndexState::Off);
    let after: Vec<(IndexStatus, bool)> = f.events.lock().unwrap()[before..].to_vec();
    assert!(!after.is_empty());
    for (status, left) in &after {
        assert_eq!(status.state, IndexState::Off, "オフにしたあとに、ほかの状態を知らせない: {after:?}");
        assert!(!left, "「オフ」を知らせた時点で、中身は消えている");
    }
    assert!(!left_on_disk(&f.index_path), "資料の中身がファイルに残らない");
    assert!(!f.index_path.exists(), "索引のファイルも消す");
    assert_eq!(f.indexer.status().updated_at, None);
}

/// 索引のファイルを消せないとき（ほかのプログラムが開いているなど）も、中身は空にする。
// [IDX-028]
#[cfg(unix)]
#[test]
fn turning_off_empties_the_index_even_when_the_files_cannot_be_removed() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    write(&f.root.join("a.txt"), &format!("{MARKER} 経費"));
    f.indexer.enable(vec![folder(&f.root)]);
    wait_ready(&f, 1);
    let data = f.index_path.parent().unwrap().to_path_buf();
    // フォルダを読み取りだけにすると、中のファイルを消せない（管理者で動かしているときは消せてしまう）
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o555)).unwrap();
    f.indexer.disable();
    wait_until("オフ", Duration::from_secs(20), || f.indexer.status().state == IndexState::Off);
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!left_on_disk(&f.index_path), "消せなくても、中身は残さない");
}

/// 「中身を送らない」を切り替えても、調べ直さず、作っている途中の処理を止めず、状態と更新した時刻も変えない。
// [IDX-034]
#[test]
fn no_content_option_does_not_rescan() {
    let f = fixture();
    write(&f.root.join("a.txt"), "経費");
    f.indexer.enable(vec![folder(&f.root)]);
    wait_ready(&f, 1);
    let before = f.indexer.status();
    let emitted = f.events.lock().unwrap().len();
    let mut changed = folder(&f.root);
    changed.no_content = true;
    f.indexer.set_folder_options(vec![changed]);
    let after = f.indexer.status();
    assert!(after.folders[0].no_content);
    assert_eq!(after.state, IndexState::Ready);
    assert_eq!(after.updated_at, before.updated_at);
    assert!(after.revision > before.revision, "画面が新しい状態として受け取れる");
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(f.events.lock().unwrap().len(), emitted, "調べ直さない");

    // 作っている途中に切り替えても、作り終える
    for i in 0..300 {
        write(&f.root.join(format!("{i}.txt")), "経費");
    }
    assert!(f.indexer.rebuild());
    let mut back = folder(&f.root);
    back.no_content = false;
    f.indexer.set_folder_options(vec![back]);
    wait_ready(&f, 301);
    assert!(!f.indexer.status().folders[0].no_content, "作り終えたあとも、切り替えたあとの値");
}

/// 状態の番号は、知らせるたびに増える（画面は、古い戻り値で新しい知らせを上書きしない）。
// [IDX-008]
#[test]
fn revisions_increase_with_every_status() {
    let f = fixture();
    write(&f.root.join("a.txt"), "経費");
    f.indexer.enable(vec![folder(&f.root)]);
    wait_ready(&f, 1);
    let revisions: Vec<u64> = events(&f).iter().map(|s| s.revision).collect();
    assert!(revisions.windows(2).all(|w| w[0] < w[1]), "{revisions:?}");
    assert_eq!(f.indexer.status().revision, *revisions.last().unwrap());
}

/// 変更の知らせが途切れずに続いても、反映を後回しにし続けない。
// [IDX-009]
#[test]
fn changes_are_reflected_while_other_files_keep_changing() {
    let f = fixture();
    write(&f.root.join("log.txt"), "0");
    f.indexer.enable(vec![folder(&f.root)]);
    wait_ready(&f, 1);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let busy = {
        let stop = stop.clone();
        let log = f.root.join("log.txt");
        std::thread::spawn(move || {
            let mut i = 0;
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                i += 1;
                std::fs::write(&log, i.to_string()).unwrap();
                std::thread::sleep(Duration::from_millis(200));
            }
        })
    };
    write(&f.root.join("出張.txt"), "宿泊の上限");
    let started = Instant::now();
    wait_until("書き込みが続くあいだの反映", Duration::from_secs(25), || search(&f, "宿泊") == ["出張.txt"]);
    let elapsed = started.elapsed();
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    busy.join().unwrap();
    assert!(elapsed < Duration::from_secs(20), "{elapsed:?}");
}

/// 見張っているフォルダが見つからなくなっても（ドライブが外れたなど）、索引は消さずに「見つかりません」にする。
/// 戻ったら、見つからないあいだの変更を索引に反映し、また見張る。
// [IDX-010]
#[test]
fn a_watched_folder_that_disappears_keeps_its_index() {
    // 見つからないフォルダを見直す間隔（ふだんは 30 分）だけを縮める
    let f = fixture_with(Intervals { poll: Duration::from_secs(1), ..Intervals::default() });
    write(&f.root.join("a.txt"), "経費");
    f.indexer.enable(vec![folder(&f.root)]);
    wait_ready(&f, 1);
    let moved = f.root.parent().unwrap().join("退避");
    std::fs::rename(&f.root, &moved).unwrap();
    wait_until("見つからない表示", Duration::from_secs(30), || f.indexer.status().folders[0].missing);
    assert_eq!(search(&f, "経費"), ["a.txt"], "索引は消さずに残す");
    assert_eq!(f.indexer.status().files, 1);

    // 見つからないあいだに増えたファイルも、戻ったら索引に入れる
    write(&moved.join("b.txt"), "経費");
    std::fs::rename(&moved, &f.root).unwrap();
    wait_until("戻ったフォルダの反映", Duration::from_secs(30), || {
        let status = f.indexer.status();
        status.state == IndexState::Ready && !status.folders[0].missing && status.files == 2
    });

    // 戻ったフォルダを、また見張る
    write(&f.root.join("c.txt"), "経費");
    wait_until("戻ったフォルダの変更の反映", Duration::from_secs(30), || f.indexer.status().files == 3);
    let mut found = search(&f, "経費");
    found.sort();
    assert_eq!(found, ["a.txt", "b.txt", "c.txt"]);
}

/// 作業用のスレッドは、最後の窓口がなくなったら止まり、一時フォルダを作り直さない。
// [IDX-012]
#[test]
fn dropping_the_indexer_stops_the_worker() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("資料");
    write(&root.join("a.txt"), "経費");
    let index_path = dir.path().join("data").join(INDEX_FILE);
    let indexer = Indexer::spawn(index_path.clone(), |_| {});
    let copy = indexer.clone();
    indexer.enable(vec![folder(&root)]);
    drop(indexer);
    wait_until("索引", Duration::from_secs(20), || copy.status().state == IndexState::Ready);
    drop(copy);
    let path = dir.path().to_path_buf();
    drop(dir);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!path.exists(), "止めたあとに、索引のフォルダを作り直さない");
}
