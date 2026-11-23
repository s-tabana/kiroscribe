//! 会議の検出の記録（`tests/fixtures/meetings/*.json`）を再生して、起きるはずの出来事が起きることを確かめる回帰テスト。
//! 記録の作り方は、`tests/fixtures/meetings/README.md` にある。

use std::path::Path;

use kiroscribe_core::meeting_record::{replay, MeetingRecord, FORMAT};

// [MTG-025]
#[test]
fn every_recorded_meeting_replays_as_expected() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/meetings");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    assert!(files.len() >= 5, "記録が少なすぎます: {}", dir.display());
    let mut failures = Vec::new();
    for file in &files {
        let name = file.file_name().unwrap().to_string_lossy().to_string();
        let record: MeetingRecord = serde_json::from_str(&std::fs::read_to_string(file).unwrap())
            .unwrap_or_else(|err| panic!("{name} を読めません: {err}"));
        assert_eq!(record.format, FORMAT, "{name} の形式が違います");
        assert!(!record.description.trim().is_empty(), "{name} に description（何を確かめるか）がありません");
        let events = replay(&record);
        if events != record.expected {
            failures.push(format!(
                "{name}（{}）\n  起きるはず: {:?}\n  起きたこと: {:?}",
                record.description, record.expected, events
            ));
        }
    }
    assert!(failures.is_empty(), "記録の再生が合いません:\n{}", failures.join("\n"));
}
