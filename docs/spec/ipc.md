# IPC: 画面と本体の取り決め

画面（React）と本体（Rust）の間で交わす、コマンドとイベントの名前と形。利用者からは見えないが、片方だけを変えると動かなくなるため、項目にして守る。

- **IPC-001** ［単体・結合］〔実装待ち〕 画面が呼ぶコマンドは、すべて本体に登録されている（36 個: list_notes・search_notes・create_note・get_note・update_note・delete_note・get_settings・save_settings・get_api_key_status・set_api_key・export_markdown・save_markdown・list_audio_devices・list_segments・get_recording_status・start_recording・pause_recording・resume_recording・stop_recording・enhance_note・cancel_ai・list_chat_messages・clear_chat_messages・send_chat_message・get_popup_state・close_popup・open_note_from_popup・open_settings_from_popup・get_guide・get_release_notes・open_external・get_app_version・get_update_status・check_for_update・install_update・take_update_notice）。画面が呼ばないコマンドは登録しない。
  - **実装待ち**: `search_notes` は、本体にまだ登録されていない（NOTE-008）。
- **IPC-002** ［単体・結合］ コマンドの引数の名前は、画面と本体で一致する（画面は noteId・patch・settings・provider・value・options・path・content・kind・enhance・url・id・query の名前で渡す）。
- **IPC-003** ［単体・結合］ コマンドの戻り値とイベントの中身は、項目の名前（camelCase）と値の型が、画面の型の定義と一致する（ノート、ノートの要約、設定、API キーの状態、文字起こし、録音の状態、チャット、書き出しの結果、ポップアップの内容、使い方、リリースノート、更新の状態、マイク）。
- **IPC-004** ［単体・結合］ 決まった値の書き方は、画面と本体で一致する: キーの種類は openai・anthropic、話者は me・them、チャットの役は user・assistant、録音の状態は idle・recording・paused・finishing、更新の段階は idle・downloading・installing、ヘルプの出どころは network・cache・bundled、ポップアップの種類は recording・finishing・stopped・error、AI の種類は enhance・chat。
- **IPC-005** ［結合・E2E］ 本体が画面に送るイベントの名前と中身は、画面が待ち受けるものと一致する: recording-status（録音の状態）、transcript-segment（文字起こし 1 件）、audio-level（話者と大きさ）、recording-error（ノートの ID と文）、enhance-delta・chat-delta（ノートの ID と届いた文字）、note-created（ノート）、open-note（ノートの ID と、議事録を作るかどうか。メインウィンドウ宛て）、open-settings（メインウィンドウ宛て）、main-shown（中身なし。メインウィンドウ宛て）、meeting-popup（ポップアップの内容。ポップアップ宛て）、update-status（更新の状態）。
- **IPC-006** ［結合］ コマンドが失敗したとき、画面は理由を 1 つの文字列として受け取る。
- **IPC-007** ［単体・E2E］ メインウィンドウの名前は main、ポップアップの名前は meeting-popup。ポップアップは、メインウィンドウと同じ画面のコードを `index.html#/popup` で開く。
- **IPC-008** ［単体・結合］ 自動インストールのための取り決め: 設定に autoUpdate（真偽値）がある。更新の状態（update-status）に ready（真偽値。ダウンロードと署名の確認が済んでいる）がある。コマンド take_update_notice は、自動で更新したあとの知らせ（更新後のバージョンの文字列）を 1 回だけ返し、メインウィンドウを表示していないとき・知らせるものがないときは null を返す。イベント main-shown は、閉じていたメインウィンドウを表示したときに送る。
