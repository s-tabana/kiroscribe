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

/// 選んだフォルダとして足す（足したときの実体を記録する。フォルダはあらかじめ作っておく）。
fn folder(path: &Path) -> SearchFolder {
    SearchFolder::register(path).unwrap()
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

    assert!(f.indexer.rebuild().is_some());
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
    let registered = folder(&f.root);
    f.indexer.enable(vec![registered.clone(), folder(&other)]);
    wait_ready(&f, 2);

    f.indexer.set_folders(vec![registered.clone()]);
    wait_ready(&f, 1);
    assert_eq!(search(&f, "経費"), ["a.txt"], "外したフォルダのファイルは消す");

    // 見つからない（ドライブが外れたなど）
    let moved = f.root.parent().unwrap().join("退避");
    std::fs::rename(&f.root, &moved).unwrap();
    f.indexer.set_folders(vec![registered]);
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
    assert!(f.indexer.rebuild().is_none(), "オフのときは作り直さない");

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
    assert!(f.indexer.rebuild().is_some());
    let mut back = folder(&f.root);
    back.no_content = false;
    f.indexer.set_folder_options(vec![back]);
    wait_ready(&f, 301);
    assert!(!f.indexer.status().folders[0].no_content, "作り終えたあとも、切り替えたあとの値");
}

/// 命令の戻り値は、命令を出した時点の状態（作業用のスレッドの進み方によらない）。
/// 空のフォルダはすぐ作り終えるので、命令のあとに状態を読み直すと「できている」が返ることがある。
// [IDX-008]
#[test]
fn commands_return_the_status_at_the_time_of_the_command() {
    let f = fixture();
    f.indexer.enable(vec![folder(&f.root)]);
    wait_ready(&f, 0);
    for _ in 0..50 {
        let before = f.indexer.status().revision;
        let returned = f.indexer.set_folders(vec![folder(&f.root)]);
        assert_eq!((returned.state, returned.revision), (IndexState::Building, before + 1), "フォルダの一覧を変えた直後");
        wait_ready(&f, 0);

        let before = f.indexer.status().revision;
        let returned = f.indexer.rebuild().unwrap();
        assert_eq!((returned.state, returned.revision), (IndexState::Building, before + 1), "作り直しを始めた直後");
        wait_ready(&f, 0);

        let before = f.indexer.status().revision;
        let mut changed = folder(&f.root);
        changed.no_content = !f.indexer.status().folders[0].no_content;
        let returned = f.indexer.set_folder_options(vec![changed.clone()]);
        assert_eq!((returned.state, returned.revision), (IndexState::Ready, before + 1), "「中身を送らない」を変えた直後");
        assert_eq!(returned.folders[0].no_content, changed.no_content);
    }
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
    // 見張れないフォルダの見直し（30 分）を待たずに、フォルダそのものを確かめる間隔で、戻ったことに気づく
    let f = fixture_with(Intervals { poll: Duration::from_secs(3600), root_check: Duration::from_millis(200), ..Intervals::default() });
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

/// 見張っているフォルダを移して、すぐに同じ名前のフォルダを作っても、新しいフォルダを調べ直して見張る。
/// 移した先のフォルダの変更は、索引に入れない。
// [IDX-010]
#[test]
fn a_watched_folder_replaced_by_one_of_the_same_name_is_watched_again() {
    let f = fixture_with(Intervals { poll: Duration::from_secs(3600), root_check: Duration::from_millis(500), ..Intervals::default() });
    write(&f.root.join("a.txt"), "経費");
    f.indexer.enable(vec![folder(&f.root)]);
    wait_ready(&f, 1);
    let moved = f.root.parent().unwrap().join("退避");
    std::fs::rename(&f.root, &moved).unwrap();
    write(&f.root.join("b.txt"), "経費");
    wait_until("新しいフォルダの反映", Duration::from_secs(30), || search(&f, "経費") == ["b.txt"]);

    write(&f.root.join("c.txt"), "経費");
    write(&moved.join("d.txt"), "経費");
    wait_until("新しいフォルダの変更の反映", Duration::from_secs(30), || f.indexer.status().files == 2);
    std::thread::sleep(Duration::from_secs(3));
    let mut found = search(&f, "経費");
    found.sort();
    assert_eq!(found, ["b.txt", "c.txt"], "移した先の変更は入れない");
}

/// 見直しても索引が変わらなければ、更新した時刻は変えない。
// [IDX-008]
#[test]
fn updated_at_moves_only_when_the_index_changes() {
    let f = fixture_with(Intervals { poll: Duration::from_secs(3600), root_check: Duration::from_millis(200), ..Intervals::default() });
    write(&f.root.join("a.txt"), "経費");
    f.indexer.enable(vec![folder(&f.root)]);
    wait_ready(&f, 1);
    let before = f.indexer.status().updated_at;
    let moved = f.root.parent().unwrap().join("退避");
    std::fs::rename(&f.root, &moved).unwrap();
    wait_until("見つからない表示", Duration::from_secs(30), || f.indexer.status().folders[0].missing);
    std::fs::rename(&moved, &f.root).unwrap();
    wait_until("戻ったフォルダの見直し", Duration::from_secs(30), || {
        let status = f.indexer.status();
        status.state == IndexState::Ready && !status.folders[0].missing
    });
    assert_eq!(f.indexer.status().updated_at, before, "変わったファイルがなければ、前の時刻のまま");

    std::thread::sleep(Duration::from_millis(20));
    write(&f.root.join("b.txt"), "経費");
    wait_ready(&f, 2);
    assert!(f.indexer.status().updated_at > before, "ファイルが増えたら、時刻を進める");
}

/// アプリを終えるときは、窓口が残っていても作業用のスレッドを止めて、終わるのを待てる。
// [IDX-012]
#[test]
fn shutdown_and_wait_stops_the_worker_while_handles_remain() {
    let f = fixture();
    write(&f.root.join("a.txt"), "経費");
    f.indexer.enable(vec![folder(&f.root)]);
    wait_ready(&f, 1);
    assert!(f.indexer.shutdown_and_wait(Duration::from_secs(5)));
    let emitted = f.events.lock().unwrap().len();
    write(&f.root.join("b.txt"), "経費");
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(f.events.lock().unwrap().len(), emitted, "止めたあとは、変更を反映しない");
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

/// 足したあとで、フォルダそのものがリンクに替わっても、リンクの先（範囲の外）のファイルを索引に入れない。
/// 見張りの知らせで 1 つずつ反映する経路・すべてを調べ直す経路・作り直しのどれでも入れず、それまでの索引は残す。
// [IDX-027][IDX-010]
#[cfg(unix)]
#[test]
fn a_folder_replaced_by_a_link_does_not_bring_in_files_outside_it() {
    // フォルダそのものを確かめる間隔を長くし、見張りの知らせから反映する経路を通す
    let f = fixture_with(Intervals { poll: Duration::from_secs(3600), root_check: Duration::from_secs(3600), ..Intervals::default() });
    write(&f.root.join("a.txt"), "経費");
    f.indexer.enable(vec![folder(&f.root)]);
    wait_ready(&f, 1);

    // 範囲の外の、無害な一時フォルダ
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("leak.txt"), "社外秘");
    write(&outside.path().join("下").join("deep.txt"), "社外秘");
    let moved = f.root.parent().unwrap().join("退避");
    std::fs::rename(&f.root, &moved).unwrap();
    std::os::unix::fs::symlink(outside.path(), &f.root).unwrap();
    // 移したフォルダへの書き込みは、見張りから、もとのパス（いまはリンク）の下のファイルの変更として届く
    write(&moved.join("leak.txt"), "経費");
    wait_until("見つからない表示", Duration::from_secs(30), || f.indexer.status().folders[0].missing);
    std::thread::sleep(Duration::from_secs(3));
    assert!(search(&f, "社外秘").is_empty(), "リンクの先の中身を入れない");
    assert_eq!(search(&f, "経費"), ["a.txt"], "それまでの索引は残す");

    // 作り直しても、リンクの先は入れない
    assert!(f.indexer.rebuild().is_some());
    wait_until("作り直し", Duration::from_secs(20), || {
        let status = f.indexer.status();
        status.state == IndexState::Ready && status.folders[0].missing
    });
    assert!(search(&f, "社外秘").is_empty());
}

/// 足したあとで、探すフォルダより上のフォルダがリンクに替わり、別の場所の同じ名前のフォルダを指すようになっても、
/// その中身を索引に入れない。見張りの知らせ・すべてを調べ直す経路・作り直しのどれでも入れず、それまでの索引は残す。
/// 上のフォルダが戻ったら、もとのフォルダとして調べ直す。
// [IDX-027][IDX-010]
#[cfg(unix)]
#[test]
fn a_parent_folder_replaced_by_a_link_does_not_bring_in_files_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("上");
    let root = parent.join("共有資料");
    write(&root.join("a.txt"), "経費");
    let index_path = dir.path().join("data").join(INDEX_FILE);
    let intervals = Intervals { poll: Duration::from_secs(3600), root_check: Duration::from_millis(200), ..Intervals::default() };
    let indexer = Indexer::spawn_with_intervals(index_path.clone(), Limits::default(), intervals, |_| {});
    let registered = folder(&root);
    indexer.enable(vec![registered.clone()]);
    wait_until("索引", Duration::from_secs(20), || {
        let status = indexer.status();
        status.state == IndexState::Ready && status.files == 1
    });
    let search = |term: &str| -> Vec<String> {
        let index = SearchIndex::open(&index_path).unwrap();
        index.search(&[term.to_string()], 10).unwrap().into_iter().map(|hit| hit.file_name).collect()
    };

    // 範囲の外の、同じ名前のフォルダ
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("共有資料").join("leak.txt"), "社外秘");
    write(&outside.path().join("共有資料").join("a.txt"), "社外秘");
    let moved = dir.path().join("退避");
    std::fs::rename(&parent, &moved).unwrap();
    std::os::unix::fs::symlink(outside.path(), &parent).unwrap();
    // もとのフォルダへの書き込みと、もとのパスの下の変更の知らせ
    write(&moved.join("共有資料").join("b.txt"), "経費");
    write(&outside.path().join("共有資料").join("leak2.txt"), "社外秘");
    wait_until("見つからない表示", Duration::from_secs(30), || indexer.status().folders[0].missing);
    std::thread::sleep(Duration::from_secs(3));
    assert!(search("社外秘").is_empty(), "別の場所の中身を入れない");
    assert_eq!(search("経費"), ["a.txt"], "それまでの索引は残す");

    // 一覧を変えて調べ直しても、作り直しても、別の場所の中身を入れない
    let returned = indexer.set_folders(vec![registered]);
    wait_until("調べ直し", Duration::from_secs(20), || {
        let status = indexer.status();
        status.state == IndexState::Ready && status.revision > returned.revision && status.folders[0].missing
    });
    assert!(search("社外秘").is_empty());
    let returned = indexer.rebuild().unwrap();
    wait_until("作り直し", Duration::from_secs(20), || {
        let status = indexer.status();
        status.state == IndexState::Ready && status.revision > returned.revision && status.folders[0].missing
    });
    assert!(search("社外秘").is_empty());

    // 上のフォルダが戻ったら、もとのフォルダとして調べ直す
    std::fs::remove_file(&parent).unwrap();
    std::fs::rename(&moved, &parent).unwrap();
    wait_until("戻ったフォルダの索引", Duration::from_secs(30), || {
        let status = indexer.status();
        status.state == IndexState::Ready && !status.folders[0].missing && status.files == 2
    });
    assert!(search("社外秘").is_empty());
    assert!(indexer.shutdown_and_wait(Duration::from_secs(5)));
}

/// 前回の索引が残ったまま、相棒がオフで起動し、索引のファイルを消すことも、中身を空にすることもできないとき:
/// 「消している」のままにして、消し直す。消し直せたら、消し終えてから「オフ」を知らせる。
// [IDX-028]
#[cfg(unix)]
#[test]
fn erasing_is_reported_until_a_retry_erases_the_index() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("共有資料");
    write(&root.join("a.txt"), &format!("{MARKER} 経費"));
    let data = dir.path().join("data");
    let index_path = data.join(INDEX_FILE);
    // 前回の起動で作った索引
    {
        let indexer = Indexer::spawn(index_path.clone(), |_| {});
        indexer.enable(vec![folder(&root)]);
        wait_until("前回の索引", Duration::from_secs(20), || {
            let status = indexer.status();
            status.state == IndexState::Ready && status.files == 1
        });
        assert!(indexer.shutdown_and_wait(Duration::from_secs(5)));
    }
    assert!(left_on_disk(&index_path));

    // 索引のファイルとフォルダを、書き込めないようにする（管理者で動かしているときは確かめられない）
    let files: Vec<PathBuf> = kiroscribe_core::search::store::index_files(&index_path).into_iter().filter(|p| p.exists()).collect();
    for file in &files {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o555)).unwrap();
    let restore = || {
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o755)).unwrap();
        for file in &files {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
    };
    if std::fs::write(data.join("probe"), "").is_ok() {
        restore();
        eprintln!("書き込みを禁じられない（管理者で動かしている）ため、確かめません");
        return;
    }

    // 知らせた状態と、知らせた時点で、索引のファイルがあったか・中身が残っていたか
    let events: Arc<Mutex<Vec<(IndexState, bool, bool)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let path = index_path.clone();
    let intervals = Intervals { purge_retry: Duration::from_millis(300), ..Intervals::default() };
    let indexer = Indexer::spawn_with_intervals(index_path.clone(), Limits::default(), intervals, move |status| {
        sink.lock().unwrap().push((status.state, path.exists(), left_on_disk(&path)));
    });
    // 起動時（相棒がオフ）と同じ順に命令する
    indexer.set_folders(vec![folder(&root)]);
    let returned = indexer.disable();
    assert_eq!(returned.state, IndexState::Erasing, "命令の戻り値");
    assert_eq!(indexer.status().state, IndexState::Erasing, "状態の読み取り");
    wait_until("「消している」の知らせ", Duration::from_secs(10), || !events.lock().unwrap().is_empty());
    // 何度か消し直しても消せないあいだは、「消している」のまま
    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(indexer.status().state, IndexState::Erasing);
    assert!(events.lock().unwrap().iter().all(|(state, ..)| *state == IndexState::Erasing), "{:?}", events.lock().unwrap());
    assert!(left_on_disk(&index_path), "消せないあいだは、中身が残っている（だから「オフ」にしない）");

    // 書き込めるようになったら、消し直して「オフ」にする
    restore();
    // 状態を更新してから知らせるので、知らせが届くまで待つ
    wait_until("オフ", Duration::from_secs(10), || {
        indexer.status().state == IndexState::Off && events.lock().unwrap().iter().any(|(state, ..)| *state == IndexState::Off)
    });
    let events = events.lock().unwrap().clone();
    let first_off = events.iter().position(|(state, ..)| *state == IndexState::Off).expect("「オフ」の知らせ");
    assert!(events[..first_off].iter().all(|(state, ..)| *state == IndexState::Erasing), "{events:?}");
    for (state, exists, left) in &events[first_off..] {
        assert_eq!(*state, IndexState::Off, "「オフ」のあとに、ほかの状態を知らせない: {events:?}");
        assert!(!exists && !left, "「オフ」を知らせた時点で、索引のファイルは消えている: {events:?}");
    }
    assert!(!index_path.exists());
    assert!(indexer.shutdown_and_wait(Duration::from_secs(5)));
}

/// 中を読めないフォルダ（読み取りの権限を外したなど）のファイルは、消えたとみなさず、索引に残す。
// [IDX-010]
#[cfg(unix)]
#[test]
fn files_in_a_folder_that_cannot_be_read_are_kept() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    write(&f.root.join("a.txt"), "経費");
    let locked = f.root.join("人事");
    write(&locked.join("b.txt"), "経費");
    f.indexer.enable(vec![folder(&f.root)]);
    wait_ready(&f, 2);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("読み取りを禁じられない（管理者で動かしている）ため、確かめません");
        return;
    }
    // 一覧を変えて、すべて調べ直す
    let returned = f.indexer.set_folders(vec![folder(&f.root)]);
    wait_until("調べ直し", Duration::from_secs(20), || {
        let status = f.indexer.status();
        status.state == IndexState::Ready && status.revision > returned.revision
    });
    let mut found = search(&f, "経費");
    found.sort();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(found, ["a.txt", "b.txt"], "読めないフォルダのファイルを消さない");
}

/// 上のフォルダを、範囲の外の同じ名前のフォルダを含む場所へのリンクに替える（無害な一時フォルダ）。戻すときに使うパスを返す。
#[cfg(unix)]
fn redirect_parent(parent: &Path, outside: &Path) -> PathBuf {
    let moved = parent.with_file_name("退避");
    std::fs::rename(parent, &moved).unwrap();
    std::os::unix::fs::symlink(outside, parent).unwrap();
    moved
}

fn search_at(index_path: &Path, term: &str) -> Vec<String> {
    let index = SearchIndex::open(index_path).unwrap();
    index.search(&[term.to_string()], 10).unwrap().into_iter().map(|hit| hit.file_name).collect()
}

/// オフのあいだに足したフォルダは、足したときの実体を基準にする。オンにする前に上のフォルダが替わり、
/// 別の場所の同じ名前のフォルダを指すようになっても、その中身を索引に入れない。戻ったら、足したフォルダを読む。
// [IDX-027][IDX-008]
#[cfg(unix)]
#[test]
fn a_folder_added_while_off_is_bound_to_the_folder_that_was_chosen() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("上");
    let root = parent.join("共有資料");
    write(&root.join("a.txt"), "経費");
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("共有資料").join("leak.txt"), "社外秘");
    let index_path = dir.path().join("data").join(INDEX_FILE);
    let intervals = Intervals { poll: Duration::from_secs(3600), root_check: Duration::from_millis(200), ..Intervals::default() };
    let indexer = Indexer::spawn_with_intervals(index_path.clone(), Limits::default(), intervals, |_| {});

    // オフのあいだに足す（起動したときと同じ命令の順）
    let registered = folder(&root);
    indexer.set_folders(vec![registered.clone()]);
    indexer.disable();
    let moved = redirect_parent(&parent, outside.path());

    indexer.enable(vec![registered]);
    wait_until("見つからない表示", Duration::from_secs(20), || {
        let status = indexer.status();
        status.state == IndexState::Ready && status.folders[0].missing
    });
    assert!(search_at(&index_path, "社外秘").is_empty(), "別の場所の中身を入れない");
    assert!(search_at(&index_path, "経費").is_empty());

    std::fs::remove_file(&parent).unwrap();
    std::fs::rename(&moved, &parent).unwrap();
    wait_until("戻ったフォルダの索引", Duration::from_secs(30), || {
        let status = indexer.status();
        status.state == IndexState::Ready && !status.folders[0].missing && status.files == 1
    });
    assert_eq!(search_at(&index_path, "経費"), ["a.txt"]);
    assert!(indexer.shutdown_and_wait(Duration::from_secs(5)));
}

/// アプリを起動し直したあとも、設定に保存した、足したときの実体を基準にする。起動していないあいだに上のフォルダが替わり、
/// 別の場所の同じ名前のフォルダを指すようになっても、その中身を索引に入れず、それまでの索引は残す。
// [IDX-027][IDX-010]
#[cfg(unix)]
#[test]
fn the_folder_chosen_when_added_is_checked_again_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("上");
    let root = parent.join("共有資料");
    write(&root.join("a.txt"), "経費");
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("共有資料").join("leak.txt"), "社外秘");
    let index_path = dir.path().join("data").join(INDEX_FILE);
    let db = kiroscribe_core::db::Database::open_in_memory().unwrap();
    db.save_search_folders(&[folder(&root)]).unwrap();
    let ready = |indexer: &Indexer, missing: bool| {
        wait_until("索引", Duration::from_secs(20), || {
            let status = indexer.status();
            status.state == IndexState::Ready && status.folders[0].missing == missing && status.files == 1
        });
    };

    let first = Indexer::spawn(index_path.clone(), |_| {});
    first.enable(db.load_search_folders().unwrap());
    ready(&first, false);
    assert!(first.shutdown_and_wait(Duration::from_secs(5)));
    drop(first);

    // 起動していないあいだに、上のフォルダが替わった
    let moved = redirect_parent(&parent, outside.path());
    let second = Indexer::spawn(index_path.clone(), |_| {});
    second.enable(db.load_search_folders().unwrap());
    ready(&second, true);
    assert!(search_at(&index_path, "社外秘").is_empty(), "別の場所の中身を入れない");
    assert_eq!(search_at(&index_path, "経費"), ["a.txt"], "それまでの索引は残す");
    let returned = second.rebuild().unwrap();
    wait_until("作り直し", Duration::from_secs(20), || {
        let status = second.status();
        status.state == IndexState::Ready && status.revision > returned.revision && status.folders[0].missing
    });
    assert!(search_at(&index_path, "社外秘").is_empty(), "作り直しても入れない");
    assert!(second.shutdown_and_wait(Duration::from_secs(5)));
    drop(second);

    // 足したフォルダが戻れば、また読む。同じ場所で作り直したフォルダも読む（IDX-010）
    std::fs::remove_file(&parent).unwrap();
    std::fs::rename(&moved, &parent).unwrap();
    std::fs::rename(&root, parent.join("古い")).unwrap();
    write(&root.join("b.txt"), "予算");
    let third = Indexer::spawn(index_path.clone(), |_| {});
    third.enable(db.load_search_folders().unwrap());
    ready(&third, false);
    assert_eq!(search_at(&index_path, "予算"), ["b.txt"]);
    assert!(search_at(&index_path, "経費").is_empty(), "作り直す前のフォルダのファイルは消す");
    assert!(third.shutdown_and_wait(Duration::from_secs(5)));
}

/// 足したときの実体を記録する前の設定のフォルダは、どこを読んだか確かめられないので、それまでの索引を消し、
/// 中身を読まずに「確認が必要」にする。選び直したら（足したときの実体を記録したら）、索引に入れる。
// [IDX-027][IDX-001]
#[test]
fn a_folder_saved_without_its_place_is_not_read_until_chosen_again() {
    let f = fixture();
    write(&f.root.join("a.txt"), "経費");
    let registered = folder(&f.root);
    f.indexer.enable(vec![registered.clone()]);
    wait_ready(&f, 1);

    let legacy = SearchFolder { place: None, ..registered.clone() };
    let returned = f.indexer.set_folders(vec![legacy]);
    assert!(returned.folders[0].unconfirmed, "戻り値にすぐ表れる");
    wait_until("確認が必要な表示", Duration::from_secs(20), || {
        let status = f.indexer.status();
        status.state == IndexState::Ready && status.revision > returned.revision && status.files == 0
    });
    let status = f.indexer.status();
    assert!(status.folders[0].unconfirmed && !status.folders[0].missing);
    assert!(search(&f, "経費").is_empty(), "それまでの索引も消す");

    // 確認が必要なフォルダの中の変更も読まない
    write(&f.root.join("b.txt"), "経費");
    std::thread::sleep(Duration::from_secs(3));
    assert!(search(&f, "経費").is_empty());

    f.indexer.set_folders(vec![registered]);
    wait_ready(&f, 2);
    assert!(!f.indexer.status().folders[0].unconfirmed);
}

/// 索引の行の削除を、失敗させる・失敗させないようにする（索引のファイルに、削除を止めるトリガーを作る・外す）。
fn block_deletes(index_path: &Path, table: &str, blocked: bool) {
    let conn = rusqlite::Connection::open(index_path).unwrap();
    conn.busy_timeout(Duration::from_secs(10)).unwrap();
    let sql = if blocked {
        format!("CREATE TRIGGER block_{table} BEFORE DELETE ON {table} BEGIN SELECT RAISE(ABORT, '消せない'); END;")
    } else {
        format!("DROP TRIGGER IF EXISTS block_{table};")
    };
    conn.execute_batch(&sql).unwrap();
}

/// 更新日時と大きさを変えずに、ファイルの中身を書き換える（索引の行を使い回すと、前の中身が残る）。
fn overwrite_keeping_stamp(path: &Path, text: &str) {
    let modified = std::fs::metadata(path).unwrap().modified().unwrap();
    assert_eq!(std::fs::metadata(path).unwrap().len(), text.len() as u64);
    std::fs::write(path, text).unwrap();
    std::fs::File::options().write(true).open(path).unwrap().set_modified(modified).unwrap();
}

fn stored_rows(index_path: &Path, root: &Path) -> usize {
    SearchIndex::open(index_path).unwrap().stored_rows(root).unwrap()
}

/// 出どころを確かめられない行（実体を記録していない設定のときの行）は、消せないあいだも検索に出さない。
/// 消す前に選び直しても、アプリを起動し直しても、その行を使い回さず、消し直す。消せたら、いまの中身を読み直す。
// [IDX-027][IDX-001]
#[test]
fn rows_that_could_not_be_erased_stay_out_of_search_until_erased() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("共有資料");
    write(&root.join("a.txt"), "経費");
    let index_path = dir.path().join("data").join(INDEX_FILE);
    let intervals = Intervals { purge_retry: Duration::from_millis(200), ..Intervals::default() };
    let registered = folder(&root);
    let settled = |indexer: &Indexer, after: u64, files: usize| {
        wait_until("索引", Duration::from_secs(20), || {
            let status = indexer.status();
            status.state == IndexState::Ready && status.revision > after && status.files == files
        });
    };

    let first = Indexer::spawn_with_intervals(index_path.clone(), Limits::default(), intervals, |_| {});
    first.enable(vec![registered.clone()]);
    settled(&first, 0, 1);
    block_deletes(&index_path, "docs", true);
    overwrite_keeping_stamp(&root.join("a.txt"), "予算");

    // 実体を記録していない設定になった。行を消せなくても、検索に出さない
    let returned = first.set_folders(vec![SearchFolder { place: None, ..registered.clone() }]);
    settled(&first, returned.revision, 0);
    assert_eq!(stored_rows(&index_path, &root), 1, "削除に失敗している");
    assert!(search_at(&index_path, "経費").is_empty(), "消せない行も検索に出さない");

    // 消す前に選び直しても、前の行を使い回さない（読まずに、消し直す）
    let returned = first.set_folders(vec![registered.clone()]);
    settled(&first, returned.revision, 0);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(stored_rows(&index_path, &root), 1);
    assert!(search_at(&index_path, "経費").is_empty());
    assert!(search_at(&index_path, "予算").is_empty());
    assert!(first.shutdown_and_wait(Duration::from_secs(5)));
    drop(first);

    // 起動し直しても、消すまでは検索に出さない
    let second = Indexer::spawn_with_intervals(index_path.clone(), Limits::default(), intervals, |_| {});
    second.enable(vec![registered]);
    settled(&second, 0, 0);
    std::thread::sleep(Duration::from_millis(600));
    assert!(search_at(&index_path, "経費").is_empty());
    assert_eq!(stored_rows(&index_path, &root), 1);

    // 消せるようになったら、消し直して、選び直したフォルダの中身を読み直す
    block_deletes(&index_path, "docs", false);
    settled(&second, 0, 1);
    assert_eq!(search_at(&index_path, "予算"), ["a.txt"]);
    assert!(search_at(&index_path, "経費").is_empty(), "前の中身を残さない");
    assert!(second.shutdown_and_wait(Duration::from_secs(5)));
}

/// 出どころを確かめられない行を、検索から外すこともできないときは、索引のファイルを消して作り直す。
// [IDX-027]
#[test]
fn when_rows_cannot_even_be_hidden_the_index_is_recreated() {
    let f = fixture();
    write(&f.root.join("a.txt"), "経費");
    let registered = folder(&f.root);
    f.indexer.enable(vec![registered.clone()]);
    wait_ready(&f, 1);
    block_deletes(&f.index_path, "docs", true);
    block_deletes(&f.index_path, "bindings", true);

    let returned = f.indexer.set_folders(vec![SearchFolder { place: None, ..registered }]);
    wait_until("作り直した索引", Duration::from_secs(20), || {
        let status = f.indexer.status();
        status.state == IndexState::Ready && status.revision > returned.revision && status.files == 0
    });
    assert_eq!(stored_rows(&f.index_path, &f.root), 0, "索引のファイルを作り直した");
    assert!(search(&f, "経費").is_empty());
}

/// 消去待ちでない索引に、検索に出す行として `term` を含む資料が入ったか（索引を作り直している途中でも、読み取りだけで確かめる。
/// [`SearchIndex::open`] は、開けないと索引のファイルを作り直すので、作業用のスレッドが作っている途中には使わない）。
#[cfg(unix)]
fn readable_hit(index_path: &Path, term: &str) -> bool {
    if kiroscribe_core::search::store::erase_marker(index_path).exists() {
        return false;
    }
    let Ok(conn) = rusqlite::Connection::open_with_flags(index_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return false;
    };
    conn.query_row(
        "SELECT COUNT(*) FROM docs WHERE instr(body, ?1) > 0 AND folder IN (SELECT folder FROM bindings)",
        [term],
        |row| row.get::<_, i64>(0),
    )
    .is_ok_and(|count| count > 0)
}

/// 索引のフォルダを読み取りだけにする（中のファイルを消せず、消去待ちの記録も作れない）。
/// 書き込みを禁じられない（管理者で動かしている）ときは、戻して false。
#[cfg(unix)]
fn lock_data_folder(data: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(data, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(data.join("probe"), "").is_ok() {
        let _ = std::fs::remove_file(data.join("probe"));
        unlock_data_folder(data);
        eprintln!("書き込みを禁じられない（管理者で動かしている）ため、確かめません");
        return false;
    }
    true
}

#[cfg(unix)]
fn unlock_data_folder(data: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(data, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// 出どころを確かめられない行を検索から外せず、索引を空にも消しもできず、消去待ちも記録できないときは、
/// 検索を止める（選び直しても再開しない）。書き込めるようになったら、消去待ちを記録してから索引を消し、
/// 消し終えてから検索を再開して、いまの中身を読み直す。
// [IDX-027]
#[cfg(unix)]
#[test]
fn an_index_that_cannot_be_invalidated_is_not_searched_until_it_is_erased() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("共有資料");
    write(&root.join("a.txt"), "経費");
    let index_path = dir.path().join("data").join(INDEX_FILE);
    let data = index_path.parent().unwrap().to_path_buf();
    let intervals = Intervals { purge_retry: Duration::from_millis(200), ..Intervals::default() };
    let indexer = Indexer::spawn_with_intervals(index_path.clone(), Limits::default(), intervals, |_| {});
    let registered = folder(&root);
    let settled = |after: u64| {
        wait_until("索引", Duration::from_secs(20), || {
            let status = indexer.status();
            status.state == IndexState::Ready && status.revision > after
        });
    };
    indexer.enable(vec![registered.clone()]);
    wait_until("索引", Duration::from_secs(20), || indexer.status().files == 1);
    block_deletes(&index_path, "docs", true);
    block_deletes(&index_path, "bindings", true);
    overwrite_keeping_stamp(&root.join("a.txt"), "予算");
    if !lock_data_folder(&data) {
        return;
    }

    let returned = indexer.set_folders(vec![SearchFolder { place: None, ..registered.clone() }]);
    settled(returned.revision);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(stored_rows(&index_path, &root), 1, "行を消せていない");
    assert!(search_at(&index_path, "経費").is_empty(), "外せない行は、検索を止めて出さない");
    assert!(!kiroscribe_core::search::store::erase_marker(&index_path).exists(), "消去待ちを記録できない状態");

    // 選び直しても、消し終えるまでは検索しない
    let returned = indexer.set_folders(vec![registered.clone()]);
    settled(returned.revision);
    std::thread::sleep(Duration::from_millis(600));
    assert!(search_at(&index_path, "経費").is_empty());

    unlock_data_folder(&data);
    wait_until("消し直したあとの読み直し", Duration::from_secs(20), || readable_hit(&index_path, "予算"));
    assert_eq!(search_at(&index_path, "予算"), ["a.txt"]);
    assert!(search_at(&index_path, "経費").is_empty(), "前の中身を残さない");
    assert!(!kiroscribe_core::search::store::erase_marker(&index_path).exists(), "消し終えたら、消去待ちを解く");
    assert!(indexer.shutdown_and_wait(Duration::from_secs(5)));
}

/// 前回の起動で消し終えられなかった索引（消去待ちの記録が残っている）は、起動し直したあとも、消し終えるまで検索しない。
/// 消せるようになったら消し直し、消去待ちを解いてから、いまの中身を読み直す。
// [IDX-027]
#[cfg(unix)]
#[test]
fn an_index_waiting_to_be_erased_stays_out_of_search_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("共有資料");
    write(&root.join("a.txt"), "経費");
    let index_path = dir.path().join("data").join(INDEX_FILE);
    let data = index_path.parent().unwrap().to_path_buf();
    let intervals = Intervals { purge_retry: Duration::from_millis(200), ..Intervals::default() };
    let registered = folder(&root);
    {
        let first = Indexer::spawn_with_intervals(index_path.clone(), Limits::default(), intervals, |_| {});
        first.enable(vec![registered.clone()]);
        wait_until("索引", Duration::from_secs(20), || first.status().files == 1);
        assert!(first.shutdown_and_wait(Duration::from_secs(5)));
    }
    // 前回の起動が、索引を検索から外せないまま終わった（消去待ちの記録だけが残っている）
    std::fs::write(kiroscribe_core::search::store::erase_marker(&index_path), "").unwrap();
    block_deletes(&index_path, "docs", true);
    block_deletes(&index_path, "bindings", true);
    overwrite_keeping_stamp(&root.join("a.txt"), "予算");
    // 読み取りだけのフォルダでも索引を開けるように、接続を開いたままにする（書き込みの途中の内容のファイルを残す）
    let held = SearchIndex::open(&index_path).unwrap();
    if !lock_data_folder(&data) {
        return;
    }

    let second = Indexer::spawn_with_intervals(index_path.clone(), Limits::default(), intervals, |_| {});
    second.enable(vec![registered]);
    wait_until("索引", Duration::from_secs(20), || second.status().state == IndexState::Ready);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(held.stored_rows(&root).unwrap(), 1, "行を消せていない");
    assert!(held.search(&["経費".to_string()], 8).unwrap().is_empty(), "消去待ちのあいだは検索しない");
    assert!(search_at(&index_path, "経費").is_empty());
    assert!(kiroscribe_core::search::store::erase_marker(&index_path).exists());

    drop(held);
    unlock_data_folder(&data);
    wait_until("消し直したあとの読み直し", Duration::from_secs(20), || readable_hit(&index_path, "予算"));
    assert_eq!(search_at(&index_path, "予算"), ["a.txt"]);
    assert!(search_at(&index_path, "経費").is_empty(), "前の中身を残さない");
    assert!(!kiroscribe_core::search::store::erase_marker(&index_path).exists());
    assert!(second.shutdown_and_wait(Duration::from_secs(5)));
}
