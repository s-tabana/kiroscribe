import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { extractReleaseNotes, listVersions } from "./release-notes.mjs";

const SCRIPT = fileURLToPath(new URL("./release-notes.mjs", import.meta.url));

const CHANGELOG = `# 変更履歴

見出しは \`## バージョン (日付)\` の形で書いてください。

## 0.2.0 (2026-11-21)

### 新しい機能

- **会議の自動検出**: 自動で録音します
- アプリ内の更新

### お知らせ

- 名前を変えました

## 0.1.0 (2026-11-20)

- 最初のリリース
`;

test("指定したバージョンの節の本文だけを取り出す（見出し行は含めない）", () => {
  assert.equal(
    extractReleaseNotes(CHANGELOG, "0.2.0"),
    "### 新しい機能\n\n- **会議の自動検出**: 自動で録音します\n- アプリ内の更新\n\n### お知らせ\n\n- 名前を変えました\n",
  );
});

test("最後の節はファイルの終わりまでを取り出す", () => {
  assert.equal(extractReleaseNotes(CHANGELOG, "0.1.0"), "- 最初のリリース\n");
});

test("バージョンの先頭の v は無視する", () => {
  assert.equal(extractReleaseNotes(CHANGELOG, "v0.1.0"), "- 最初のリリース\n");
});

test("改行が CRLF でも同じ結果になる（Windows でのチェックアウト）", () => {
  const crlf = CHANGELOG.replaceAll("\n", "\r\n");
  assert.equal(extractReleaseNotes(crlf, "0.2.0"), extractReleaseNotes(CHANGELOG, "0.2.0"));
  assert.ok(!extractReleaseNotes(crlf, "0.2.0").includes("\r"));
});

test("節がなければ、見つかった節を添えてエラーにする", () => {
  assert.throws(() => extractReleaseNotes(CHANGELOG, "0.3.0"), /0\.3\.0.*節がありません.*0\.2\.0、0\.1\.0/s);
});

test("前方一致では選ばない（0.2 や 0.2.0-beta.1 は 0.2.0 と別物）", () => {
  assert.throws(() => extractReleaseNotes(CHANGELOG, "0.2"), /節がありません/);
  assert.throws(() => extractReleaseNotes(CHANGELOG, "0.2.0-beta.1"), /節がありません/);
  const pre = "## 0.2.0-beta.1 (2026-11-01)\n\n- ベータ\n\n## 0.2.0 (2026-11-21)\n\n- 正式版\n";
  assert.equal(extractReleaseNotes(pre, "0.2.0"), "- 正式版\n");
  assert.equal(extractReleaseNotes(pre, "0.2.0-beta.1"), "- ベータ\n");
});

test("本文が空の節はエラーにする", () => {
  const empty = "## 0.3.0 (2026-12-01)\n\n## 0.2.0 (2026-11-21)\n\n- 何か\n";
  assert.throws(() => extractReleaseNotes(empty, "0.3.0"), /本文がありません/);
});

test("日付なし・角かっこ付きの見出しも読める", () => {
  const other = "## [1.0.0] - 2027-01-01\n\n- 一\n\n## v0.9.0\n\n- 二\n";
  assert.equal(extractReleaseNotes(other, "1.0.0"), "- 一\n");
  assert.equal(extractReleaseNotes(other, "0.9.0"), "- 二\n");
  assert.deepEqual(listVersions(other), ["1.0.0", "0.9.0"]);
});

test("コードブロックの中の「## 」は節の区切りにしない", () => {
  const withCode = "## 0.2.0 (2026-11-21)\n\n```md\n## 見出しの例\n```\n\n- 続き\n\n## 0.1.0 (2026-11-20)\n\n- 前\n";
  assert.equal(
    extractReleaseNotes(withCode, "0.2.0"),
    "```md\n## 見出しの例\n```\n\n- 続き\n",
  );
});

test("コマンドとして実行すると、本文を標準出力かファイルに書く", () => {
  const dir = mkdtempSync(join(tmpdir(), "kiroscribe-notes-"));
  try {
    const changelog = join(dir, "CHANGELOG.md");
    writeFileSync(changelog, CHANGELOG);

    const toStdout = spawnSync(process.execPath, [SCRIPT, "--version", "0.1.0", "--changelog", changelog], {
      encoding: "utf8",
    });
    assert.equal(toStdout.status, 0, toStdout.stderr);
    assert.equal(toStdout.stdout, "- 最初のリリース\n");

    const output = join(dir, "out", "notes.md");
    const toFile = spawnSync(
      process.execPath,
      [SCRIPT, "--version", "0.1.0", "--changelog", changelog, "--output", output],
      { encoding: "utf8" },
    );
    assert.equal(toFile.status, 0, toFile.stderr);
    assert.equal(readFileSync(output, "utf8"), "- 最初のリリース\n");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("コマンドとして実行し、節がなければ終了コード 1 で何も出力しない", () => {
  const dir = mkdtempSync(join(tmpdir(), "kiroscribe-notes-"));
  try {
    const changelog = join(dir, "CHANGELOG.md");
    writeFileSync(changelog, CHANGELOG);
    const result = spawnSync(process.execPath, [SCRIPT, "--version", "9.9.9", "--changelog", changelog], {
      encoding: "utf8",
      env: { ...process.env, GITHUB_ACTIONS: "" },
    });
    assert.equal(result.status, 1);
    assert.equal(result.stdout, "");
    assert.match(result.stderr, /エラー: CHANGELOG に「## 9\.9\.9 \(日付\)」の節がありません/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("引数が足りなければ、使い方を添えて終了コード 1 にする", () => {
  const result = spawnSync(process.execPath, [SCRIPT], {
    encoding: "utf8",
    env: { ...process.env, GITHUB_ACTIONS: "" },
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /--version/);
  assert.match(result.stderr, /使い方/);
});

test("GitHub Actions の中では、エラーを注釈（::error::）として出す", () => {
  const result = spawnSync(process.execPath, [SCRIPT], {
    encoding: "utf8",
    env: { ...process.env, GITHUB_ACTIONS: "true" },
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /^::error::引数が足りません: --version%0A使い方: /);
});
