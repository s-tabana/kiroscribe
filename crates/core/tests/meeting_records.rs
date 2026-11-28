//! 会議の検出の記録（`tests/fixtures/meetings/*.json`）を再生して、起きるはずの出来事が起きることを確かめる回帰テスト。
//! 記録の作り方は、`tests/fixtures/meetings/README.md` にある。旧い形式の記録（/1・/2・/3）も、その形式の判定と再生のしかたで確かめる。

use std::path::Path;

use kiroscribe_core::meeting_record::{replay_json, FORMAT, FORMAT_V2, FORMAT_V3};
use kiroscribe_core::meeting_record_v1;

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
    let mut formats = Vec::new();
    for file in &files {
        let name = file.file_name().unwrap().to_string_lossy().to_string();
        let replayed = replay_json(&std::fs::read_to_string(file).unwrap()).unwrap_or_else(|err| panic!("{name} を読めません: {err}"));
        assert!(!replayed.description.trim().is_empty(), "{name} に description（何を確かめるか）がありません");
        assert!(replayed.undetermined.is_empty(), "{name} の再生のしかたを決められません: {:?}", replayed.undetermined);
        if !formats.contains(&replayed.format) {
            formats.push(replayed.format.clone());
        }
        if replayed.events != replayed.expected {
            failures.push(format!(
                "{name}（{}）\n  起きるはず: {:?}\n  起きたこと: {:?}",
                replayed.description, replayed.expected, replayed.events
            ));
        }
    }
    assert!(failures.is_empty(), "記録の再生が合いません:\n{}", failures.join("\n"));
    for format in [FORMAT, FORMAT_V3, FORMAT_V2, meeting_record_v1::FORMAT] {
        assert!(formats.iter().any(|known| known == format), "{format} の記録がありません");
    }
}
