import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createTestKey, signForTest } from "./lib/minisign-fixture.mjs";
import { buildLatestJson } from "./make-latest-json.mjs";

const SCRIPT = fileURLToPath(new URL("./make-latest-json.mjs", import.meta.url));

const INSTALLER = "Kiroscribe_0.2.0_x64-setup.exe";
const key = createTestKey();
const SIGNATURE = signForTest(key, Buffer.from("installer"), {
  file: INSTALLER,
  version: "0.2.0",
});

const base = {
  version: "0.2.0",
  installer: INSTALLER,
  signature: `${SIGNATURE}\n`,
  notes: "### 新しい機能\r\n\r\n- アプリ内の更新\r\n",
  repo: "s-tabana/kiroscribe",
  pubDate: "2026-11-21T09:00:00Z",
};

test("Tauri updater の静的な JSON の形で組み立てる", () => {
  assert.deepEqual(buildLatestJson(base), {
    version: "0.2.0",
    notes: "### 新しい機能\n\n- アプリ内の更新",
    pub_date: "2026-11-21T09:00:00Z",
    platforms: {
      "windows-x86_64": {
        signature: SIGNATURE,
        url: "https://github.com/s-tabana/kiroscribe/releases/download/v0.2.0/Kiroscribe_0.2.0_x64-setup.exe",
      },
    },
  });
});

test("signature は .sig の中身そのもの（前後の空白・改行だけを落とす）", () => {
  const latest = buildLatestJson({ ...base, signature: `  ${SIGNATURE}\r\n` });
  assert.equal(latest.platforms["windows-x86_64"].signature, SIGNATURE);
});

test("バージョンの先頭の v は落とし、URL のタグには v を付ける", () => {
  const latest = buildLatestJson({ ...base, version: "v0.2.0" });
  assert.equal(latest.version, "0.2.0");
  assert.match(latest.platforms["windows-x86_64"].url, /\/releases\/download\/v0\.2\.0\//);
});

test("日時を省くと、いまの時刻を RFC 3339（UTC・秒まで）で入れる", () => {
  const before = Date.now();
  const latest = buildLatestJson({ ...base, pubDate: undefined });
  assert.match(latest.pub_date, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/);
  const stamp = Date.parse(latest.pub_date);
  assert.ok(stamp >= before - 1000 && stamp <= Date.now());
});

test("SemVer でないバージョンは受け付けない", () => {
  for (const version of ["", "0.2", "0.2.0.1", "latest", "01.2.3"]) {
    assert.throws(() => buildLatestJson({ ...base, version }), /SemVer/, version);
  }
});

test("owner/name の形でないリポジトリは受け付けない", () => {
  for (const repo of ["", "kiroscribe", "https://github.com/s-tabana/kiroscribe", "a/b/c"]) {
    assert.throws(() => buildLatestJson({ ...base, repo }), /owner\/name/, repo);
  }
});

test("URL とずれるおそれのあるファイル名（空白・パスなど）は受け付けない", () => {
  for (const installer of ["", "Kiro scribe_0.2.0_x64-setup.exe", "nsis/Kiroscribe.exe", "議事録.exe"]) {
    assert.throws(() => buildLatestJson({ ...base, installer }), /ファイル名/, installer);
  }
});

test("RFC 3339 でない日時は受け付けない", () => {
  for (const pubDate of ["2026-11-21", "2026/11/21 09:00", "2026-13-41T09:00:00Z"]) {
    assert.throws(() => buildLatestJson({ ...base, pubDate }), /RFC 3339/, pubDate);
  }
  assert.equal(
    buildLatestJson({ ...base, pubDate: "2026-11-21T18:00:00+09:00" }).pub_date,
    "2026-11-21T18:00:00+09:00",
  );
});

test("署名が空・minisign の形式でないときは受け付けない", () => {
  assert.throws(() => buildLatestJson({ ...base, signature: "  \n" }), /base64/);
  assert.throws(() => buildLatestJson({ ...base, signature: "target/x.exe.sig" }), /base64/);
  assert.throws(
    () => buildLatestJson({ ...base, signature: Buffer.from("hello").toString("base64") }),
    /minisign/,
  );
});

test("別のファイルの署名は受け付けない", () => {
  const other = signForTest(key, Buffer.from("x"), {
    file: "Kiroscribe_0.1.0_x64-setup.exe",
    version: "0.2.0",
  });
  assert.throws(
    () => buildLatestJson({ ...base, signature: other }),
    /Kiroscribe_0\.1\.0_x64-setup\.exe.*のものです/,
  );
});

test("別のバージョンとして署名されたものは受け付けない（アプリ側の検証で失敗するため）", () => {
  const other = signForTest(key, Buffer.from("x"), { file: INSTALLER, version: "0.1.0" });
  assert.throws(() => buildLatestJson({ ...base, signature: other }), /バージョン 0\.1\.0 のもの/);
});

test("ファイル名・バージョンの入っていない古い形式の署名は、そのまま通す", () => {
  const plain = signForTest(key, Buffer.from("x"));
  assert.equal(
    buildLatestJson({ ...base, signature: plain }).platforms["windows-x86_64"].signature,
    plain,
  );
});

test("コマンドとして実行すると、JSON をファイルか標準出力に書く", () => {
  const dir = mkdtempSync(join(tmpdir(), "kiroscribe-latest-"));
  try {
    const sigPath = join(dir, `${INSTALLER}.sig`);
    const notesPath = join(dir, "notes.md");
    writeFileSync(sigPath, SIGNATURE);
    writeFileSync(notesPath, "- 更新の内容\n");
    const args = [
      SCRIPT,
      "--version", "0.2.0",
      "--installer", INSTALLER,
      "--signature", sigPath,
      "--notes", notesPath,
      "--repo", "s-tabana/kiroscribe",
    ];

    const toStdout = spawnSync(process.execPath, args, { encoding: "utf8" });
    assert.equal(toStdout.status, 0, toStdout.stderr);
    const printed = JSON.parse(toStdout.stdout);
    assert.deepEqual(Object.keys(printed), ["version", "notes", "pub_date", "platforms"]);
    assert.equal(printed.notes, "- 更新の内容");

    const output = join(dir, "out", "latest.json");
    const toFile = spawnSync(
      process.execPath,
      [...args, "--pub-date", "2026-11-21T09:00:00Z", "--output", output],
      { encoding: "utf8" },
    );
    assert.equal(toFile.status, 0, toFile.stderr);
    assert.equal(toFile.stdout, "");
    assert.deepEqual(JSON.parse(readFileSync(output, "utf8")), {
      ...buildLatestJson({ ...base, notes: "- 更新の内容" }),
    });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("コマンドとして実行し、署名ファイルがなければ終了コード 1 にする", () => {
  const dir = mkdtempSync(join(tmpdir(), "kiroscribe-latest-"));
  try {
    const notesPath = join(dir, "notes.md");
    writeFileSync(notesPath, "- 更新の内容\n");
    const result = spawnSync(
      process.execPath,
      [
        SCRIPT,
        "--version", "0.2.0",
        "--installer", INSTALLER,
        "--signature", join(dir, "missing.sig"),
        "--notes", notesPath,
        "--repo", "s-tabana/kiroscribe",
      ],
      { encoding: "utf8", env: { ...process.env, GITHUB_ACTIONS: "" } },
    );
    assert.equal(result.status, 1);
    assert.equal(result.stdout, "");
    assert.match(result.stderr, /エラー: 署名ファイル（.*missing\.sig）を読めません/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
