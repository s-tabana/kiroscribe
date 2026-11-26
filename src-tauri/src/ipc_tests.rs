//! 画面（`src/lib/backend.ts`）と Rust のコマンド・イベントが食い違っていないことを確かめるテスト。
//!
//! - コマンド名とイベント名: 画面側のソースと Rust 側のソースから一覧を取り出して突き合わせる
//! - 引数の名前: 画面側が渡す名前のまま、模擬のランタイム（`tauri::test`）の IPC でコマンドを呼ぶ
//! - 戻り値とイベントの形: 実際に返った JSON の見本を `src/test/ipc-contract.generated.ts` に書き出し、
//!   画面側の型に合うことを TypeScript のコンパイラ（`npm run typecheck`）に確かめさせる
//!
//! 録音や AI を含む一連の流れ（`scenario`）は、模擬の音声と接続先の差し替えが要るので feature `e2e` のときだけ動く:
//! `cargo test -p kiroscribe --features e2e`

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use kiroscribe_core::secrets::MemorySecretStore;
use kiroscribe_core::transcript::Speaker;
use kiroscribe_core::Database;
use serde_json::{json, Value};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, MockRuntime, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{Manager, WebviewWindow, WebviewWindowBuilder};

use crate::ai::AiState;
use crate::background::{BackgroundState, MAIN_LABEL};
use crate::recording::RecorderState;
use crate::state::AppState;
use crate::update::UpdateState;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|err| panic!("{} を読めません: {err}", path.display()))
}

// ---- 画面側のソース（backend.ts）から、コマンドとイベントの対応を取り出す ----

/// `Backend` のメソッドと、それが呼ぶコマンド。
#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandBinding {
    method: String,
    command: String,
    /// `invoke` に渡すオブジェクトのキー
    arg_keys: Vec<String>,
    /// `name: (a, b) => invoke("cmd", { a, b })` の形なら、メソッドの引数名（この順でコマンドの引数になる）。
    /// この形のときは、メソッドの引数と戻り値の型が、そのままコマンドの引数と戻り値の型になる
    direct_params: Option<Vec<String>>,
}

/// `Backend` のメソッドと、それが購読するイベント。
#[derive(Debug, Clone, PartialEq, Eq)]
struct EventBinding {
    method: String,
    event: String,
    /// イベントの内容をそのままコールバックに渡すか（`subscribe("x", cb)` の形）
    passes_payload: bool,
}

#[derive(Debug, Default)]
struct Frontend {
    commands: Vec<CommandBinding>,
    events: Vec<EventBinding>,
}

/// `prefix"名前"` の `名前` と、その後ろの文字列。
fn quoted_after<'a>(text: &'a str, prefix: &str) -> Option<(&'a str, &'a str)> {
    let start = text.find(prefix)? + prefix.len();
    let rest = text[start..].strip_prefix('"')?;
    let end = rest.find('"')?;
    Some((&rest[..end], &rest[end + 1..]))
}

/// `a, b: x, c = 1` の形から名前（`a`・`b`・`c`）を取り出す。
fn names(list: &str) -> Vec<String> {
    list.split(',')
        .map(|item| item.split([':', '=']).next().unwrap_or_default().trim().to_string())
        .filter(|name| !name.is_empty())
        .collect()
}

/// メソッド定義の始まり（`    name: (` または `    async name(`）なら、その名前。
fn method_start(line: &str) -> Option<&str> {
    let body = line.strip_prefix("    ")?;
    if body.starts_with(' ') {
        return None;
    }
    let body = body.strip_prefix("async ").unwrap_or(body);
    let end = body.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
    (end > 0 && (body[end..].starts_with(':') || body[end..].starts_with('('))).then(|| &body[..end])
}

fn parse_frontend(source: &str) -> Frontend {
    let start = source
        .find("export function createTauriBackend")
        .expect("backend.ts に createTauriBackend がありません");
    let body = &source[start..];
    let body = &body[..body[1..].find("\nexport ").map_or(body.len(), |end| end + 1)];

    let mut frontend = Frontend::default();
    let mut method = String::new();
    for line in body.lines() {
        if let Some(name) = method_start(line) {
            method = name.to_string();
        }
        if let Some((command, rest)) = quoted_after(line, "invoke(") {
            let arg_keys = rest
                .strip_prefix(", {")
                .and_then(|args| args.split('}').next())
                .map(names)
                .unwrap_or_default();
            // `name: (a, b) => invoke(...)` の形か
            let direct_params = line
                .trim_start()
                .strip_prefix(&format!("{method}: ("))
                .and_then(|after| after.split_once(") => invoke("))
                .map(|(params, _)| names(params))
                .filter(|params| {
                    params.len() == arg_keys.len() && params.iter().all(|param| arg_keys.contains(param))
                });
            frontend.commands.push(CommandBinding {
                method: method.clone(),
                command: command.to_string(),
                arg_keys,
                direct_params,
            });
        }
        if let Some((event, rest)) = quoted_after(line, "subscribe(")
            .or_else(|| quoted_after(line, "subscribeAsync("))
        {
            frontend.events.push(EventBinding {
                method: method.clone(),
                event: event.to_string(),
                // 登録が済んだときのコールバック（`onReady`）を続けて渡すものも、内容をそのまま渡す
                passes_payload: matches!(rest.trim_end().trim_end_matches(','), ", cb)" | ", cb, onReady)"),
            });
        }
    }
    frontend
}

fn frontend() -> Frontend {
    parse_frontend(&read(&repo_root().join("src/lib/backend.ts")))
}

/// `dir` 以下の、テスト以外の TypeScript ファイル。
fn frontend_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            frontend_sources(&path, out);
        } else if (name.ends_with(".ts") || name.ends_with(".tsx"))
            && !name.contains(".test.")
            && !name.contains(".generated.")
        {
            out.push(path);
        }
    }
}

// ---- Rust 側のソースから、登録しているコマンドとイベント名を取り出す ----

/// `generate_handler![...]` に並んでいるコマンドの名前。
fn registered_commands(lib_rs: &str) -> Vec<String> {
    let start = lib_rs.find("generate_handler![").expect("lib.rs に generate_handler! がありません");
    let list = &lib_rs[start + "generate_handler![".len()..];
    let list = &list[..list.find(']').expect("generate_handler! が閉じていません")];
    list.split(',')
        .map(|item| item.trim().rsplit("::").next().unwrap_or_default().to_string())
        .filter(|name| !name.is_empty())
        .collect()
}

/// `pub const EVENT_...: &str = "名前";` の名前（src-tauri/src の全ファイルから）。
fn declared_events() -> BTreeSet<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut events = BTreeSet::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        for line in read(&path).lines() {
            if !line.trim_start().starts_with("pub const EVENT_") {
                continue;
            }
            if let Some((name, _)) = quoted_after(line, "&str = ") {
                events.insert(name.to_string());
            }
        }
    }
    events
}

#[test]
fn parses_the_shapes_used_in_backend_ts() {
    let parsed = parse_frontend(
        r#"
export function createTauriBackend(): Backend {
  return {
    isPreview: false,
    listNotes: () => invoke("list_notes"),
    updateNote: (id, patch) => invoke("update_note", { id, patch }),
    confirm: (message, title = "Kiroscribe") => ask(message, { title }),
    onSegment: (cb) => subscribe("transcript-segment", cb),
    onReadySegment: (cb, onReady) => subscribe("ready-segment", cb, onReady),
    async saveMarkdown(noteId, options, fileName) {
      const path = await save({ defaultPath: fileName });
      await invoke("save_markdown", { noteId, options, path });
      return path;
    },
    onOpenSettings: (cb) => subscribe("open-settings", () => cb()),
  };
}

export function createDefaultBackend(): Backend {
  return invoke("not_part_of_the_backend");
}
"#,
    );
    let command = |method: &str, command: &str, keys: &[&str], direct: Option<&[&str]>| CommandBinding {
        method: method.into(),
        command: command.into(),
        arg_keys: keys.iter().map(|k| k.to_string()).collect(),
        direct_params: direct.map(|params| params.iter().map(|p| p.to_string()).collect()),
    };
    assert_eq!(
        parsed.commands,
        vec![
            command("listNotes", "list_notes", &[], Some(&[])),
            command("updateNote", "update_note", &["id", "patch"], Some(&["id", "patch"])),
            command("saveMarkdown", "save_markdown", &["noteId", "options", "path"], None),
        ]
    );
    assert_eq!(
        parsed.events,
        vec![
            EventBinding {
                method: "onSegment".into(),
                event: "transcript-segment".into(),
                passes_payload: true
            },
            EventBinding {
                method: "onReadySegment".into(),
                event: "ready-segment".into(),
                passes_payload: true
            },
            EventBinding {
                method: "onOpenSettings".into(),
                event: "open-settings".into(),
                passes_payload: false
            },
        ]
    );
    assert_eq!(
        registered_commands("x.invoke_handler(tauri::generate_handler![\n  commands::list_notes,\n  ai::cancel_ai,\n])"),
        vec!["list_notes", "cancel_ai"]
    );
}

/// 画面側が呼ぶコマンドの一覧と、Rust 側が登録しているコマンドの一覧が一致すること。
/// 片方にしかないコマンドがあると、画面の操作が実行時に「Command ... not found」で失敗するか、
/// 使われないコマンドが残る。
// [IPC-001]
#[test]
fn frontend_and_rust_agree_on_command_names() {
    let frontend = frontend();
    let called: BTreeSet<String> = frontend.commands.iter().map(|b| b.command.clone()).collect();
    let registered = registered_commands(include_str!("lib.rs"));
    let registered_set: BTreeSet<String> = registered.iter().cloned().collect();
    assert_eq!(registered.len(), registered_set.len(), "lib.rs に同じコマンドが 2 回登録されています");
    assert!(called.len() >= 30, "backend.ts からコマンドを取り出せていません: {called:?}");

    let only_frontend: Vec<_> = called.difference(&registered_set).collect();
    let only_rust: Vec<_> = registered_set.difference(&called).collect();
    assert!(
        only_frontend.is_empty() && only_rust.is_empty(),
        "コマンドが食い違っています。画面側（src/lib/backend.ts）だけ: {only_frontend:?} / Rust 側（src-tauri/src/lib.rs）だけ: {only_rust:?}"
    );

    // コマンドを呼ぶのは backend.ts だけ（ほかの場所から直接呼ぶと、この検査から漏れる）
    let mut sources = Vec::new();
    frontend_sources(&repo_root().join("src"), &mut sources);
    for path in sources {
        if path.ends_with("lib/backend.ts") {
            continue;
        }
        let text = read(&path);
        assert!(
            !text.contains("invoke(") && !text.contains("listen("),
            "{} が Tauri のコマンドやイベントを直接使っています。src/lib/backend.ts を通してください",
            path.display()
        );
    }
}

/// 画面側が購読するイベントの一覧と、Rust 側が定義しているイベント名（`EVENT_*`）の一覧が一致すること。
// [IPC-005]
#[test]
fn frontend_and_rust_agree_on_event_names() {
    let subscribed: BTreeSet<String> = frontend().events.iter().map(|b| b.event.clone()).collect();
    let declared = declared_events();
    assert!(subscribed.len() >= 10, "backend.ts からイベントを取り出せていません: {subscribed:?}");
    let only_frontend: Vec<_> = subscribed.difference(&declared).collect();
    let only_rust: Vec<_> = declared.difference(&subscribed).collect();
    assert!(
        only_frontend.is_empty() && only_rust.is_empty(),
        "イベントが食い違っています。画面側（src/lib/backend.ts）だけ: {only_frontend:?} / Rust 側（EVENT_*）だけ: {only_rust:?}"
    );
}

/// ウィンドウの名前は、本体・アプリの設定・画面に渡す権限・画面のコードで同じものを指していること。
/// ずれると、イベントが届かない・ポップアップに権限がない・ポップアップにメインの画面が出る、のどれかになる。
// [IPC-007]
#[test]
fn window_labels_match_the_config_and_the_frontend() {
    use crate::background::POPUP_LABEL;
    assert_eq!(MAIN_LABEL, "main");
    assert_eq!(POPUP_LABEL, "meeting-popup");

    let tauri_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let conf: Value = serde_json::from_str(&read(&tauri_dir.join("tauri.conf.json"))).unwrap();
    let windows = conf["app"]["windows"].as_array().expect("tauri.conf.json の app.windows");
    assert_eq!(windows.len(), 1, "起動時に作るのはメインウィンドウだけ（ポップアップは本体が作る）");
    assert_eq!(windows[0]["label"], MAIN_LABEL);

    let capability: Value = serde_json::from_str(&read(&tauri_dir.join("capabilities/default.json"))).unwrap();
    assert_eq!(capability["windows"], json!([MAIN_LABEL, POPUP_LABEL]));

    // ポップアップは、メインウィンドウと同じ画面のコードを index.html#/popup で開く
    assert!(include_str!("background.rs").contains(r#"WebviewUrl::App("index.html#/popup".into())"#));
    let main_tsx = read(&repo_root().join("src/main.tsx"));
    assert!(main_tsx.contains(r##"window.location.hash.startsWith("#/popup")"##), "src/main.tsx がポップアップを見分けていません");
}

// ---- 模擬のランタイムでコマンドを呼ぶ ----

/// 更新の署名を確かめる公開鍵（このテストのために作った使い捨ての鍵。秘密鍵は残していない）。
/// 模擬の配布元（`fixture`）が配るファイルのうち、「署名が正しい」ものは、この鍵で署名してある。
const TEST_UPDATE_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEIzNEM2QUNDQzU2NjQwMkEKUldRcVFHYkZ6R3BNc3pKUU5tYVVHbWs4REhFUTBOUWRWUmpPSm9QaERKTjdOOVcwWDk1eHpyZXoK";

/// 模擬のランタイムの上に、製品と同じコマンドを登録したアプリを作る。
/// 状態は、データベースと API キーを持つ `AppState` 以外を用意する（`AppState` は使う側が足す）。
fn mock_app(update_endpoint: &str) -> (tauri::App<MockRuntime>, WebviewWindow<MockRuntime>) {
    let mut context = mock_context(noop_assets());
    context.config_mut().plugins.0.insert(
        "updater".into(),
        json!({
            "pubkey": TEST_UPDATE_PUBKEY,
            "endpoints": [update_endpoint],
            "dangerousInsecureTransportProtocol": true,
        }),
    );
    let builder = mock_builder().plugin(tauri_plugin_updater::Builder::new().build());
    let app = crate::register_commands(builder)
        .build(context)
        .expect("模擬のアプリを作れません");
    app.manage(RecorderState::default());
    app.manage(AiState::default());
    app.manage(BackgroundState::default());
    app.manage(crate::meeting_watch::MeetingState::default());
    app.manage(UpdateState::new(false, None));
    app.manage(kiroscribe_core::debug_log::DebugLog::open(debug_log_dir()).expect("デバッグ用のログを用意できません"));
    // 資料の索引は、テストごとのフォルダに作る（相棒がオフのまま始める）
    let index_dir = index_root().join(uuid_like());
    crate::companion::setup(app.handle(), &index_dir, &kiroscribe_core::settings::Settings { companion: false, ..Default::default() }, Vec::new());
    let window = WebviewWindowBuilder::new(&app, MAIN_LABEL, Default::default())
        .build()
        .expect("模擬のウィンドウを作れません");
    (app, window)
}

/// 資料の索引を作るフォルダ（target の下に、テストのプロセスごとに 1 つ）。
/// 同じ target で同時に動くほかのテストの索引を消さないよう、消すのは自分のフォルダと、1 日より前に残ったフォルダだけ。
fn index_root() -> &'static std::path::Path {
    static ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let exe = std::env::current_exe().expect("テストの実行ファイルの場所が分かりません");
        let parent = exe
            .parent()
            .and_then(std::path::Path::parent)
            .expect("target の場所が分かりません")
            .join("ipc-search-index");
        let stale = std::time::Duration::from_secs(24 * 60 * 60);
        for entry in std::fs::read_dir(&parent).into_iter().flatten().flatten() {
            let old = entry.metadata().and_then(|meta| meta.modified()).is_ok_and(|at| at.elapsed().is_ok_and(|age| age > stale));
            if old {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
        let dir = parent.join(std::process::id().to_string());
        let _ = std::fs::remove_dir_all(&dir);
        dir
    })
}

/// フォルダの名前に使う、ほかのテストと重ならない文字列。
fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!("{}-{}-{}", std::process::id(), kiroscribe_core::time::now_ms(), NEXT.fetch_add(1, Ordering::SeqCst))
}

/// 模擬のアプリごとの、デバッグ用のログのフォルダ（テストの実行ファイルの隣、target の中）。
/// 模擬のアプリはテストの終わりまでログを書くので、フォルダはテストの間は残し、
/// 次にテストを流したときに、前の回のフォルダをまとめて消す（溜まり続けないように）。
fn debug_log_dir() -> PathBuf {
    static BASE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let base = BASE.get_or_init(|| {
        let exe = std::env::current_exe().expect("テストの実行ファイルの場所が分かりません");
        let base = exe
            .parent()
            .and_then(Path::parent)
            .expect("target の場所が分かりません")
            .join("ipc-debug-logs");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    });
    tempfile::Builder::new().tempdir_in(base).unwrap().keep()
}

/// 画面の `invoke(command, args)` と同じ経路でコマンドを呼ぶ。
fn invoke(window: &WebviewWindow<MockRuntime>, command: &str, args: Value) -> Result<Value, Value> {
    let url = if cfg!(any(windows, target_os = "android")) {
        "http://tauri.localhost"
    } else {
        "tauri://localhost"
    };
    get_ipc_response(
        window,
        InvokeRequest {
            cmd: command.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: url.parse().unwrap(),
            body: InvokeBody::Json(args),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
    .map(|body| body.deserialize::<Value>().expect("戻り値が JSON ではありません"))
}

/// 画面側が呼ぶすべてのコマンドが、実際に登録されていること（引数なしで呼び、「見つからない」以外の結果になる）。
/// ソースの突き合わせ（上のテスト）が、実際の登録内容とずれていないことの確認でもある。
///
/// データベースと API キーを持つ状態（`AppState`）は用意しないので、それを使うコマンドは実行の前に断られる
/// （このテストから外部の API を呼ばない）。更新の確認だけは実行されるので、どこにもつながらない配布元にしておく。
// [IPC-001]
#[test]
fn every_frontend_command_reaches_a_handler() {
    #[cfg(feature = "e2e")]
    fixture::fixture();
    let (_app, window) = mock_app("http://127.0.0.1:9/latest.json");
    for binding in frontend().commands {
        let result = invoke(&window, &binding.command, json!({}));
        if let Err(Value::String(message)) = &result {
            assert!(
                !message.contains("not found"),
                "{} は登録されていません: {message}",
                binding.command
            );
        }
        // 引数のあるコマンドは実行されずに断られる: 引数が足りないか、その前に AppState がないことが理由になる
        // （画面側の引数の名前を Rust 側が受け付けることは、scenario で確かめる）
        if !binding.arg_keys.is_empty() {
            let message = result.expect_err("引数がないのに受け付けました").to_string();
            assert!(
                message.contains("state not managed") || binding.arg_keys.iter().any(|key| message.contains(key.as_str())),
                "{}: 引数が足りないというエラーになるはずです: {message}",
                binding.command
            );
        }
    }
    let unknown = invoke(&window, "no_such_command", json!({})).unwrap_err();
    assert!(unknown.to_string().contains("not found"), "{unknown}");
}

/// 検索のコマンドは、画面が渡す名前（query）で検索の文字を受け取り、タイトル・メモの本文・議事録・文字起こしの
/// どれかにその文字を含むノートの ID を、一覧と同じ並び（新しい順）で返す。
// [NOTE-008]
#[test]
fn search_notes_command_covers_body_minutes_and_transcript() {
    #[cfg(feature = "e2e")]
    fixture::fixture();
    let (app, window) = mock_app("http://127.0.0.1:9/latest.json");
    app.manage(AppState::new(
        Database::open_in_memory().unwrap(),
        Box::new(MemorySecretStore::default()),
    ));
    let create = |title: &str, user_notes: &str| {
        let id = invoke(&window, "create_note", json!({})).unwrap()["id"].as_str().unwrap().to_string();
        let patch = json!({ "title": title, "userNotes": user_notes });
        invoke(&window, "update_note", json!({ "id": id, "patch": patch })).unwrap();
        id
    };
    let by_title = create("Offsite の計画", "");
    // 一覧のプレビュー（1 行目）には出ない行
    let by_body = create("定例", "- 日程\n- 来月の offsite の場所");
    let by_minutes = create("採用面談", "- 経歴");
    let by_transcript = create("", "");
    {
        let state = app.state::<AppState>();
        let db = state.db();
        db.save_enhanced_notes(&by_minutes, "## 決定事項\n- 一次面接は通過\n- OFFSITE は箱根", None).unwrap();
        db.add_segment(&by_transcript, Speaker::Them, 1_000, 2_000, "オフサイト、つまり Offsite ですね").unwrap();
    }

    let search = |query: &str| invoke(&window, "search_notes", json!({ "query": query })).unwrap();
    // 大文字と小文字は区別しない。新しいノートが先
    assert_eq!(search("offsite"), json!([by_transcript, by_minutes, by_body, by_title]));
    assert_eq!(search("来月"), json!([by_body]), "メモの本文（プレビューに出ない行）");
    assert_eq!(search("箱根"), json!([by_minutes]), "議事録");
    assert_eq!(search("オフサイト"), json!([by_transcript]), "文字起こし");
    assert_eq!(search("どこにもない言葉"), json!([]));
    assert_eq!(search("   "), json!([]), "空の検索では何も返さない（絞り込まないのは画面の役目）");

    // 検索の文字は query の名前で渡す（ほかの名前では受け付けない）
    let refused = invoke(&window, "search_notes", json!({ "text": "offsite" })).unwrap_err();
    assert!(refused.to_string().contains("query"), "{refused}");
}

/// 過去のノートの索引は、相棒と「Kiroscribe の過去のノート」の両方がオンで保存したときに作り、
/// ノートを保存するたびに更新し、ノートを削除したら消す。
// [IDX-017][NOTE-015]
#[test]
fn past_notes_are_indexed_while_both_settings_are_on() {
    #[cfg(feature = "e2e")]
    fixture::fixture();
    let (app, window) = mock_app("http://127.0.0.1:9/latest.json");
    app.manage(AppState::new(Database::open_in_memory().unwrap(), Box::new(MemorySecretStore::default())));
    let search = |term: &str| {
        let companion = app.state::<crate::companion::CompanionState>();
        companion.indexer.search(&[term.to_string()], None, 8).1.into_iter().map(|hit| hit.note_id).collect::<Vec<_>>()
    };
    let wait_for = |term: &str, expected: Vec<String>| {
        let start = std::time::Instant::now();
        while search(term) != expected {
            assert!(start.elapsed() < std::time::Duration::from_secs(20), "{term} の検索結果が {expected:?} になりません: {:?}", search(term));
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };
    let id = invoke(&window, "create_note", json!({})).unwrap()["id"].as_str().unwrap().to_string();
    let patch = json!({ "title": "経理の定例", "userNotes": "- 経費精算は新システムへ" });
    invoke(&window, "update_note", json!({ "id": id, "patch": patch })).unwrap();

    // 両方がオンで保存した時点で、すべてのノートを入れ直す
    let mut settings = invoke(&window, "get_settings", json!({})).unwrap();
    settings["companion"] = json!(false);
    invoke(&window, "save_settings", json!({ "settings": settings })).unwrap();
    settings["companion"] = json!(true);
    invoke(&window, "save_settings", json!({ "settings": settings })).unwrap();
    wait_for("経費精算", vec![id.clone()]);

    // 保存するたびに更新する
    let patch = json!({ "userNotes": "- 交通費の上限を決めた" });
    invoke(&window, "update_note", json!({ "id": id, "patch": patch })).unwrap();
    wait_for("交通費", vec![id.clone()]);
    wait_for("経費精算", vec![]);

    // 「Kiroscribe の過去のノート」をオフにして保存すると、ノートの索引を消す。オンに戻すと入れ直す
    settings["companionNotes"] = json!(false);
    invoke(&window, "save_settings", json!({ "settings": settings })).unwrap();
    assert!(search("交通費").is_empty());
    settings["companionNotes"] = json!(true);
    invoke(&window, "save_settings", json!({ "settings": settings })).unwrap();
    wait_for("交通費", vec![id.clone()]);

    // ノートを削除したら消す
    invoke(&window, "delete_note", json!({ "id": id })).unwrap();
    wait_for("交通費", vec![]);
}

// ---- 見本の書き出し ----

/// 見本の値を、実行のたびに変わらない形にする。型の確認に要る情報（キー・null かどうか・列挙の値）だけを残す:
/// 数値は 0、文字列は「…」。ただし英小文字で始まる英字だけの文字列（`"recording"` などの列挙の値）はそのまま残す。
/// 配列は、要素の形の種類だけを残す（件数や順番は、音声の区切れ方などその時々で変わるため）。
fn normalize(value: &Value) -> Value {
    match value {
        Value::Number(_) => json!(0),
        Value::String(text) => {
            let enum_like = text.starts_with(|c: char| c.is_ascii_lowercase())
                && text.chars().all(|c| c.is_ascii_alphabetic());
            if enum_like {
                value.clone()
            } else {
                json!("…")
            }
        }
        Value::Array(items) => {
            let kinds: BTreeMap<String, Value> = items
                .iter()
                .map(normalize)
                .map(|item| (item.to_string(), item))
                .collect();
            Value::Array(kinds.into_values().collect())
        }
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), normalize(v))).collect()),
        other => other.clone(),
    }
}

/// 1 つのメソッドの見本。
#[derive(Default)]
struct MethodSamples {
    /// 配列でない値（重複を除く。見本には並べ替えて出す）
    values: Vec<String>,
    /// 配列の値は、すべての呼び出しの要素をまとめて 1 つの配列にする
    array_items: Option<BTreeSet<String>>,
}

impl MethodSamples {
    fn add_value(&mut self, text: String) {
        if !self.values.contains(&text) {
            self.values.push(text);
        }
    }

    fn lines(&self) -> Vec<String> {
        // 出てきた順ではなく、並べ替えた順にする（裏で進む処理のイベントは、届く順がその時々で変わるため）
        let mut lines = self.values.clone();
        lines.sort();
        if let Some(items) = &self.array_items {
            lines.push(format!("[{}]", items.iter().cloned().collect::<Vec<_>>().join(",")));
        }
        lines
    }
}

/// メソッドごとの見本。
#[derive(Default)]
struct Samples(BTreeMap<String, MethodSamples>);

impl Samples {
    /// 戻り値やイベントの内容を足す。
    fn add(&mut self, method: &str, value: &Value) {
        let samples = self.0.entry(method.to_string()).or_default();
        match normalize(value) {
            Value::Array(items) => samples
                .array_items
                .get_or_insert_with(BTreeSet::new)
                .extend(items.iter().map(Value::to_string)),
            other => samples.add_value(other.to_string()),
        }
    }

    /// 引数の並びを足す（並びなので、順番と個数はそのまま残す）。
    fn add_arguments(&mut self, method: &str, arguments: &[Value]) {
        let tuple = Value::Array(arguments.iter().map(normalize).collect());
        self.0.entry(method.to_string()).or_default().add_value(tuple.to_string());
    }
}

#[test]
fn samples_keep_only_what_the_type_check_needs() {
    let value = json!({
        "id": "0b5c2c3e-0000-4000-8000-000000000000",
        "state": "recording",
        "kind": "googleMeet",
        "title": "週次定例",
        "model": "gpt-4o-transcribe",
        "createdAt": 1_800_000_000_000_i64,
        "enhancedNotes": null,
        "sources": ["me", "them"],
        "ok": true,
    });
    assert_eq!(
        normalize(&value),
        json!({
            "id": "…", "state": "recording", "kind": "googleMeet", "title": "…", "model": "…",
            "createdAt": 0, "enhancedNotes": null, "sources": ["me", "them"], "ok": true,
        })
    );
    assert_eq!(
        normalize(&json!([{ "speaker": "them", "id": 3 }, { "speaker": "me", "id": 1 }, { "speaker": "me", "id": 2 }])),
        json!([{ "speaker": "me", "id": 0 }, { "speaker": "them", "id": 0 }]),
        "配列は要素の形の種類だけ（件数と順番によらない）"
    );

    let mut samples = Samples::default();
    samples.add("getNote", &json!({ "id": "あ", "n": 1 }));
    samples.add("getNote", &json!({ "id": "い", "n": 2 }));
    samples.add("getNote", &json!({ "id": null, "n": 2 }));
    assert_eq!(samples.0["getNote"].lines().len(), 2, "同じ形の見本は 1 つにまとめる");

    samples.add("listSegments", &json!([]));
    samples.add("listSegments", &json!([{ "speaker": "me" }]));
    samples.add("listSegments", &json!([{ "speaker": "them" }, { "speaker": "me" }]));
    assert_eq!(
        samples.0["listSegments"].lines(),
        [r#"[{"speaker":"me"},{"speaker":"them"}]"#],
        "配列の戻り値は、すべての呼び出しの要素をまとめる"
    );

    samples.add_arguments("setApiKey", &[json!("openai"), json!("sk-1")]);
    samples.add_arguments("setApiKey", &[json!("openai"), json!("sk-2")]);
    samples.add_arguments("openNoteFromPopup", &[json!("id"), json!(true)]);
    assert_eq!(samples.0["setApiKey"].lines(), [r#"["openai","…"]"#]);
    assert_eq!(samples.0["openNoteFromPopup"].lines(), [r#"["id",true]"#]);
}

#[cfg(feature = "e2e")]
mod fixture;
#[cfg(feature = "e2e")]
mod flows;
#[cfg(feature = "e2e")]
mod scenario;
