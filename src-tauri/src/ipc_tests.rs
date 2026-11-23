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

use serde_json::{json, Value};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, MockRuntime, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{Manager, WebviewWindow, WebviewWindowBuilder};

use crate::ai::AiState;
use crate::background::{BackgroundState, MAIN_LABEL};
use crate::recording::RecorderState;
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
        if let Some((event, rest)) = quoted_after(line, "subscribe(") {
            frontend.events.push(EventBinding {
                method: method.clone(),
                event: event.to_string(),
                passes_payload: rest.trim_end().trim_end_matches(',') == ", cb)",
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

// ---- 模擬のランタイムでコマンドを呼ぶ ----

/// 模擬のランタイムの上に、製品と同じコマンドを登録したアプリを作る。
/// 状態は、データベースと API キーを持つ `AppState` 以外を用意する（`AppState` は使う側が足す）。
fn mock_app(update_endpoint: &str) -> (tauri::App<MockRuntime>, WebviewWindow<MockRuntime>) {
    let mut context = mock_context(noop_assets());
    context.config_mut().plugins.0.insert(
        "updater".into(),
        json!({
            "pubkey": "dGVzdA==",
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
    app.manage(UpdateState::new(false, None));
    let window = WebviewWindowBuilder::new(&app, MAIN_LABEL, Default::default())
        .build()
        .expect("模擬のウィンドウを作れません");
    (app, window)
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
#[test]
fn every_frontend_command_reaches_a_handler() {
    #[cfg(feature = "e2e")]
    scenario::fixture();
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
    /// 配列でない値（重複を除き、出てきた順）
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
        let mut lines = self.values.clone();
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
mod scenario;
