# 開発とリリース

Kiroscribe をビルドする人と、リリースを出す人のための説明です。アプリの使い方は [guide.md](guide.md) にあります。

## 構成

```text
crates/core/   画面に依存しないロジック（保存・音声処理・文字起こし・Claude API・書き出し）
src-tauri/     Tauri アプリ（コマンドとイベント）
src/           画面（React + TypeScript）
scripts/       リリース用のスクリプトと、そのテスト
.github/       CI・リリース・依存関係の検査のワークフロー
docs/          使い方（guide.md。アプリのヘルプにも表示される）とスクリーンショット
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

# インストーラの作成（target/release/bundle/nsis/ にできる）
npm run tauri build -- --config scripts/tauri.ci.conf.json

# 録音デバイスの動作確認（API は呼ばない）
cargo run -p kiroscribe-core --example record_probe -- 15
```

`--config scripts/tauri.ci.conf.json` は、更新用の署名を付けずにビルドするための指定です。付けないと、署名鍵（`TAURI_SIGNING_PRIVATE_KEY`）がないというエラーで止まります。

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

4. **リリースを始める。** 次のどちらかです。
   - タグを push する: `git tag v0.3.0 && git push origin v0.3.0`（タグは `v` + バージョン）
   - GitHub の **Actions → release → Run workflow** で main を選び、バージョン（例: `0.3.0`）を入力する

バージョンの食い違い、CHANGELOG の節の不足、署名鍵の未設定、テストの失敗があると、Release を作らずに失敗します。公開したあとで不具合が見つかったときは、同じバージョンを差し替えずに、次のバージョンを出します。`0.3.0-beta.1` のようにハイフンの付くバージョンはプレリリースになり、利用者のアプリには配られません。

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
