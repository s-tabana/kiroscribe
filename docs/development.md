# 開発とリリース

Kiroscribe をビルドする人と、リリースを出す人のための説明です。アプリの使い方は [guide.md](guide.md) にあります。

## 構成

```text
crates/core/   画面に依存しないロジック（保存・音声処理・文字起こし・Claude API・書き出し）
src-tauri/     Tauri アプリ（コマンドとイベント）
e2e/           実際のアプリを起動して操作するテスト（E2E）と、そのための模擬サーバー
src/           画面（React + TypeScript）
scripts/       リリース用のスクリプト・仕様とテストの対応の検査と、そのテスト
.github/       CI・リリース・依存関係の検査・網羅率の計測のワークフロー
docs/          使い方（guide.md。アプリのヘルプにも表示される）とスクリーンショット
docs/spec/     仕様一覧（振る舞いを 1 項目ずつ書いたもの）と、未対応の一覧
docs/release-checklist.md  リリース前の実機チェックリスト
CHANGELOG.md   変更履歴（リリースノートの原稿）
```

## 必要なもの

- Rust（stable）と Node.js 22 以上
- Windows: Visual Studio Build Tools（C++）と WebView2
- Linux（開発用）: `libwebkit2gtk-4.1-dev`、`libasound2-dev`、`libssl-dev` など（[Tauri の前提条件](https://v2.tauri.app/start/prerequisites/)）

## 主なコマンド

```bash
npm ci                        # 依存関係のインストール
npm run tauri dev             # アプリを開発モードで起動
npm run dev                   # ブラウザで画面だけを確認（デモデータ。保存されない）
npm test                      # 画面のテスト（vitest）と Rust のテスト（cargo test）
(cd scripts && node --test)   # リリース用スクリプトのテスト
node scripts/check-spec-coverage.mjs   # 仕様一覧の項目が、テストとチェックリストに結び付いているか
npm run test:web:coverage     # 画面のテストの網羅率（結果は target/coverage-web/）

# インストーラの作成（target/release/bundle/nsis/ にできる）
npm run tauri build -- --config scripts/tauri.ci.conf.json

# 録音デバイスの動作確認（API は呼ばない）
cargo run -p kiroscribe-core --example record_probe -- 15
```

`--config scripts/tauri.ci.conf.json` は、更新用の署名を付けずにビルドするための指定です。付けないと、署名鍵（`TAURI_SIGNING_PRIVATE_KEY`）がないというエラーで止まります。

## 仕様とテスト

Kiroscribe の振る舞いは、[仕様一覧](spec/README.md)（`docs/spec/`。領域ごとに 1 ファイル）に、確かめられる単位で 1 項目ずつ書いてあります。各項目には ID（`NOTE-001` の形）と、確かめ方の種類（単体・結合・E2E・手動）が付いています。

**テストに仕様の ID を書いて、項目に結び付けます。** 書き方は 1 つだけで、角かっこに入れた ID です。1 つのテストに複数書けます。

```rust
// [NOTE-004][NOTE-007]   ← Rust は、#[test] の付いた関数の直前のコメントに書く
#[test]
fn list_notes_newest_first_with_preview() { /* … */ }
```

```ts
// vitest・node:test・E2E は、テストの名前に書く
it("[NOTE-014] 確認してからノートを削除し、次のノートを選ぶ", async () => { /* … */ });
```

**検査**は `node scripts/check-spec-coverage.mjs` です（CI とリリースのワークフローでも動きます）。種類に「単体」「結合」がある項目には ID を参照するテストが、「E2E」がある項目には `e2e/` の下のテストからの参照が、「手動」がある項目には[リリース前の実機チェックリスト](release-checklist.md)への記載が要ります。仕様一覧にない ID を書いたテストも失敗になります。

### 機能を足す・直すときの手順

1. **仕様の項目を書く。** `docs/spec/<領域>.md` に、「〜のとき、〜する」の形で 1 項目ずつ足します（ID は、その領域の最後の番号の次）。振る舞いを変えるときは、項目の文を直します（ID は変えません）。なくすときは、ID を `docs/spec/retired.txt` に移します。
2. **テストを書く。** 足した・直した項目の ID を付けたテストを書き、失敗することを確かめます。種類に「手動」があるときは、実機チェックリストに手順を足します。
3. **実装する。** テストと `node scripts/check-spec-coverage.mjs` が通るまで直します。利用者に見える変更なら、`CHANGELOG.md` と `docs/guide.md` も直します。

不具合を直すときは、再現するテストに、当てはまる項目の ID を付けます。当てはまる項目がなければ、仕様一覧の漏れなので、項目を足します。

### 未対応の一覧

まだテストのそろっていない項目は、[`docs/spec/pending.txt`](spec/pending.txt) に載っています。検査は、ここに載っている行だけ「テストがない」ことを見逃します。

- **この一覧は、減らすだけにします。** テストを書いたら、その行を消します（消し忘れると、検査が「一覧から消してください」と失敗します）。
- 検査を通すために行を足すことはしません。新しい項目には、テストを一緒に足します。
- `node scripts/check-spec-coverage.mjs --strict` は、一覧を見逃しに使わず、残っている行をすべて不足として数えます。リリースのワークフローは `--strict` で検査するので、一覧に行が残っているとリリースできません（いまは空です）。
## E2E テスト

実際のアプリを起動し、画面を操作して確かめるテストです（`e2e/specs/`）。アプリは E2E テスト用のビルド（cargo の feature `e2e`）を使います。このビルドでだけ、外部の API・音声・会議の検出・OS のダイアログなどを模擬のものに差し替えられます（差し替えの一覧は `crates/core/src/e2e/mod.rs` の先頭）。リリース用のビルドには含まれません。

```bash
cargo install tauri-driver --locked       # 初回だけ
npm run build:e2e                         # E2E テスト用のアプリを作る（target/debug/）
npm run test:e2e                          # すべて走らせる（数分）。1 つだけなら -- e2e/specs/03-recording.test.mjs
cargo test -p kiroscribe --features e2e   # コマンドの層の結合テスト（録音・AI・会議の検出・更新など）と、画面側の型との突き合わせ
```

ほかに、Linux では WebKitWebDriver（Ubuntu のパッケージは `webkitgtk-webdriver`。24.04 までは `webkit2gtk-driver`）と `xvfb`、Windows では WebView2 と同じバージョンの `msedgedriver.exe`（PATH に置くか、環境変数 `KIROSCRIBE_E2E_NATIVE_DRIVER` で場所を指定）が要ります。失敗したテストのスクリーンショットとログは `e2e/artifacts/` に残ります。`cargo build` や `cargo test` のあとは、`target/debug/` のアプリが E2E テスト用でなくなっていることがあるので、`npm run build:e2e` をやり直してください。

`npm run test:e2e` は、最初にアプリを 1 回起動して、WebDriver で操作できることを確かめます。できなければ、シナリオは動かさずに、原因を調べるための情報（道具のバージョン、プロセスの様子、ドライバーとアプリのログ）を標準エラーに出して終わります。データベースを直接書き換えるシナリオがあるので、Node.js は組み込みの SQLite（`node:sqlite`）を使える 22.13 以上が要ります。

### インストールと更新の通しのテスト（Windows）

利用者と同じインストーラ（NSIS）でアプリを入れ、アプリ内更新で新しい版（本物のインストーラ）に更新するまでを確かめるテストです（`e2e/installer/`。CI では `.github/workflows/installer-e2e.yml`、リリースの前にも走ります）。

```bash
npm run build:e2e:installer   # 古い版（0.0.1）と新しい版（0.0.2）のインストーラを作る（target/installer-e2e/。使い捨ての鍵で署名する）
npm run test:e2e:installer    # 入れる → 更新する → 起動し直す → データが残っている → アンインストール、を確かめる
```

どちらのインストーラも E2E テスト用のビルドで、バージョンと、アプリに埋め込む公開鍵（使い捨ての鍵のもの）だけが違います。更新は模擬サーバーから配ります。インストーラが起動し直したアプリは WebDriver の外で動くので、アプリが作業フォルダに残す起動の記録（`launches.log`）で確かめます。インストーラは環境変数を引き継がないことがあるため、作業フォルダは起動の引数 `--kiroscribe-e2e-dir=<作業フォルダ>` でも渡しています。

このテストは、PC の利用者の場所（`%LOCALAPPDATA%\Kiroscribe`）に本当にインストールし、最後にアンインストールします。Kiroscribe が入っている PC では、置き換えないよう、始める前に止まります（使い捨ての環境でだけ `-- --replace-installed` を付けます）。

### 画面と本体の取り決め

コマンドの引数・戻り値やイベントの内容を変えたときは、見本（`src/test/ipc-contract.generated.ts`）を `KIROSCRIBE_UPDATE_IPC_CONTRACT=1 cargo test -p kiroscribe --features e2e ipc_contract` で書き直し、`npm run typecheck` が通るよう画面側の型を合わせます。

## リリースの出し方

リリースは `.github/workflows/release.yml` が作ります。署名付きのインストーラをビルドし、GitHub の Release に添付します。利用者のアプリは、その Release を見て更新に気づきます。

1. **バージョンを上げる。** `package.json` の `version` と、`Cargo.toml` の `[workspace.package]` の `version` を同じ値にします。`src-tauri/tauri.conf.json` は `package.json` を参照しているので、書き換えません。lock ファイルも合わせます。

   ```bash
   npm install --package-lock-only   # package-lock.json
   cargo update --workspace          # Cargo.lock
   node scripts/check-version.mjs    # そろっていればバージョンが表示される
   ```

2. **`CHANGELOG.md` に節を書く。** 見出しは `## 0.3.0 (2026-12-01)` の形です。節の本文は、そのままアプリの「リリースノート」と GitHub のリリース本文になるので、利用者が読む文章として書きます。次のコマンドで、取り出される内容を確かめられます。

   ```bash
   node scripts/release-notes.mjs --version 0.3.0
   ```

3. **main に入れる。** CI が通ったことを確かめます。

4. **実機で確かめる。** CI が保存したインストーラで、[リリース前の実機チェックリスト](release-checklist.md)の「A. 公開の前に確かめること」を行います（公開のあとに「B」を行います）。インストール・アプリ内の更新・アンインストールは、CI のインストールと更新のテスト（installer-e2e）が確かめるので、チェックリストにはありません。

5. **リリースを始める。** 次のどちらかです。
   - タグを push する: `git tag v0.3.0 && git push origin v0.3.0`（タグは `v` + バージョン）
   - GitHub の **Actions → release → Run workflow** で main を選び、バージョン（例: `0.3.0`）を入力する

バージョンの食い違い、CHANGELOG の節の不足、署名鍵の未設定、仕様とテストの対応の不足、テストの失敗（インストールと更新の通しのテストを含む）があると、Release を作らずに失敗します。公開したあとで不具合が見つかったときは、同じバージョンを差し替えずに、次のバージョンを出します。`0.3.0-beta.1` のようにハイフンの付くバージョンはプレリリースになり、利用者のアプリには配られません。

細かい動作は、ワークフローのファイルの中のコメントに書いてあります。

## 更新用の署名鍵

アプリ内の更新のファイルは、Tauri の updater の鍵で署名します。インストール済みのアプリは、自分に埋め込まれた公開鍵で確かめられた更新だけを受け付けます。Windows のコード署名とは別のものです。

| もの | 置き場所 |
| --- | --- |
| 公開鍵 | `src-tauri/tauri.conf.json` の `plugins.updater.pubkey` |
| 秘密鍵 | GitHub の Secret `TAURI_SIGNING_PRIVATE_KEY` |
| 秘密鍵のパスワード（付けた場合だけ） | GitHub の Secret `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` |

**秘密鍵は、Secret とは別の安全な場所にも保管してください。** GitHub の Secret は、登録したあとで中身を読み出せません。秘密鍵をなくすと、既存の利用者にアプリ内の更新を届けられなくなります。秘密鍵はリポジトリにコミットしないでください。

## リリースを承認制にする

release.yml のジョブは、Environment「`release`」で動きます。**Settings → Environments → release** の **Required reviewers** に自分を入れると、Actions の画面で承認するまでビルドも公開も始まりません。
