import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { afterEach, test } from "node:test";
import { fileURLToPath } from "node:url";
import {
  checkCoverage,
  checkRepository,
  checklistIds,
  collectSpec,
  formatSummary,
  parsePending,
  parseRetired,
  parseSpecFile,
  scanRust,
  scanScript,
  scanTests,
} from "./check-spec-coverage.mjs";

const SCRIPT = fileURLToPath(new URL("./check-spec-coverage.mjs", import.meta.url));

const dirs = [];
afterEach(() => {
  for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

/** 仕様一覧の項目を、検査に渡す形（ID → 項目）にする */
function spec(lines, file = "docs/spec/note.md") {
  const { items, problems } = collectSpec([parseSpecFile(file, lines.join("\n"))]);
  assert.deepEqual(problems, []);
  return items;
}

const ref = (ids, area = "other") => ({ file: `${area}/x.test.ts`, line: 1, name: "テスト", ids, area });
const pend = (id, kind, section = "理由") => ({ id, kind, section, line: 1 });

// ---- 仕様一覧の読み取り ----

test("項目の行から、ID・種類・印・文を読む", () => {
  const { items, problems } = parseSpecFile(
    "docs/spec/note.md",
    [
      "# NOTE: ノート",
      "",
      "ノートの作成と編集。",
      "",
      "- **NOTE-001** ［単体・E2E］ 新しいノートを作る。",
      "- **NOTE-002** ［結合・手動］〔見直し予定〕 いまはこう動く。",
      "- **NOTE-003** ［単体］〔実装待ち〕 決まった振る舞い。",
      "  - **実装待ち**: いまの状態。",
      "- **NOTE-004** ［未検証］ 確かめていない。",
    ].join("\r\n"),
  );
  assert.deepEqual(problems, []);
  assert.deepEqual(
    items.map(({ id, kinds, marks, line }) => ({ id, kinds, marks, line })),
    [
      { id: "NOTE-001", kinds: ["単体", "E2E"], marks: [], line: 5 },
      { id: "NOTE-002", kinds: ["結合", "手動"], marks: ["見直し予定"], line: 6 },
      { id: "NOTE-003", kinds: ["単体"], marks: ["実装待ち"], line: 7 },
      { id: "NOTE-004", kinds: ["未検証"], marks: [], line: 9 },
    ],
  );
  assert.equal(items[0].text, "新しいノートを作る。");
});

test("項目の行の形・ID の形・種類・印の誤りを、行の番号を添えて知らせる", () => {
  const cases = [
    ["- **NOTE-001** 種類がない。", /note\.md:1 項目の行の形が違います/],
    ["- **NOTE-001**［単体］空白がない。", /項目の行の形が違います/],
    ["- **NOTE-1** ［単体］ 番号が 3 桁でない。", /ID の形が違います: NOTE-1/],
    ["- **note-001** ［単体］ 小文字。", /ID の形が違います: note-001/],
    ["- **REC-001** ［単体］ 別の領域の ID。", /REC-001 は note\.md の項目ではありません/],
    ["- **NOTE-001** ［単体・目視］ 知らない種類。", /NOTE-001 の種類「目視」は使えません/],
    ["- **NOTE-001** ［単体,結合］ 区切りが違う。", /種類「単体,結合」は使えません/],
    ["- **NOTE-001** ［単体・単体］ 同じ種類。", /同じ種類が 2 回/],
    ["- **NOTE-001** ［未検証・単体］ 未検証と一緒。", /「未検証」は、ほかの種類と一緒には書けません/],
    ["- **NOTE-001** ［単体］〔要確認〕 知らない印。", /印「〔要確認〕」は使えません/],
  ];
  for (const [line, expected] of cases) {
    const { problems } = parseSpecFile("docs/spec/note.md", line);
    assert.equal(problems.length, 1, line);
    assert.match(problems[0], expected);
  }
});

test("ファイルをまたいだ ID の重複を見つける", () => {
  const { items, problems } = collectSpec([
    parseSpecFile("docs/spec/note.md", "- **NOTE-001** ［単体］ A。\n- **NOTE-001** ［単体］ B。"),
    parseSpecFile("docs/spec/rec.md", "- **REC-001** ［単体］ C。"),
  ]);
  assert.equal(items.size, 2);
  assert.equal(items.get("NOTE-001").text, "A。");
  assert.deepEqual(problems, ["docs/spec/note.md:2 NOTE-001 が重複しています（docs/spec/note.md:1 にもあります）"]);
});

// ---- テストの拾い方 ----

test("Rust: テストの関数の直前に続くコメントから ID を拾う", () => {
  const tests = scanRust(
    [
      "/// 説明。[NOTE-000] 本体のコードのコメントは拾わない",
      "pub fn production() {}",
      "",
      "#[cfg(test)]",
      "mod tests {",
      "    // [NOTE-001][NOTE-002]",
      "    #[test]",
      "    fn two_ids() {}",
      "",
      "    /// 本物の資格情報マネージャーを使う。",
      "    // [KEY-004]",
      "    #[cfg(windows)]",
      "    #[test]",
      "    fn doc_comment_then_ids_then_cfg() {}",
      "",
      "    /// [TRN-006] 説明の中に書いてもよい",
      '    #[tokio::test(flavor = "multi_thread")]',
      "    async fn tokio_test() {}",
      "",
      "    #[test]",
      "    fn untagged() {}",
      "",
      "    // [NOTE-003]",
      "    fn helper_is_not_a_test() {}",
      "",
      "    // [NOTE-004]",
      "    #[test]",
      "    #[ignore]",
      "    fn ignored_tests_do_not_count() {}",
      "",
      "    // [NOTE-005]",
      "",
      "    #[test]",
      "    fn blank_line_breaks_the_comment() {}",
      "}",
    ].join("\n"),
  );
  assert.deepEqual(
    tests.map(({ name, ids, line }) => ({ name, ids, line })),
    [
      { name: "two_ids", ids: ["NOTE-001", "NOTE-002"], line: 8 },
      { name: "doc_comment_then_ids_then_cfg", ids: ["KEY-004"], line: 14 },
      { name: "tokio_test", ids: ["TRN-006"], line: 18 },
      { name: "untagged", ids: [], line: 21 },
      { name: "blank_line_breaks_the_comment", ids: [], line: 34 },
    ],
  );
});

test("vitest・node:test: テストの名前から ID を拾う", () => {
  const tests = scanScript(
    [
      'describe("[NOTE-000] describe の名前は拾わない", () => {',
      '  it("[NOTE-001] 1 つの項目", async () => {});',
      "  it('[NOTE-002][REC-001] 2 つの項目', () => {});",
      "  test(`[NOTE-003] テンプレートの文字列`, () => {});",
      '  it("ID のないテスト", () => {});',
      '  it.each([["a", 1], ["b (2)", 2]])("[NOTE-004] 表のテスト %s", () => {});',
      '  test.each(cases)("[NOTE-005] 表（変数）", () => {});',
      '  it.skip("[NOTE-006] 動かさないテストは数えない", () => {});',
      '  test.todo("[NOTE-007] これから書くテストも数えない");',
      '  // it("[NOTE-008] コメントにしたテストは数えない", () => {});',
      "  it(name, () => {});",
      '  expect(submit("[NOTE-009] テストでない呼び出し")).toBe(true);',
      '  it.only("[NOTE-010] only は数える", () => {});',
      '  it("かっこ ) や引用符 \\" を含む名前 [NOTE-011]", () => {});',
      "});",
    ].join("\n"),
  );
  assert.deepEqual(
    tests.map(({ ids, line }) => [line, ...ids]),
    [
      [2, "NOTE-001"],
      [3, "NOTE-002", "REC-001"],
      [4, "NOTE-003"],
      [5],
      [6, "NOTE-004"],
      [7, "NOTE-005"],
      [13, "NOTE-010"],
      [14, "NOTE-011"],
    ],
  );
  assert.equal(tests[0].name, "[NOTE-001] 1 つの項目");
});

/** 仮のリポジトリを作る。files は { 相対パス: 中身 } */
function makeRepo(files) {
  const root = mkdtempSync(join(tmpdir(), "kiroscribe-spec-"));
  dirs.push(root);
  for (const [path, content] of Object.entries(files)) {
    mkdirSync(dirname(join(root, path)), { recursive: true });
    writeFileSync(join(root, path), content);
  }
  return root;
}

test("テストの置き場所で層を分ける（e2e/・src-tauri/・それ以外）。テストでないファイルと node_modules は読まない", () => {
  const root = makeRepo({
    "crates/core/src/notes.rs": "// [NOTE-001]\n#[test]\nfn core_test() {}\n",
    "src-tauri/src/commands.rs": "// [NOTE-002]\n#[tokio::test]\nasync fn command_test() {}\n",
    "src/App.test.tsx": 'it("[NOTE-003] 画面", () => {});\n',
    "src/App.tsx": 'it("[NOTE-004] テストのファイルではない", () => {});\n',
    "scripts/check.test.mjs": 'test("[NOTE-005] スクリプト", () => {});\n',
    "e2e/specs/notes.e2e.ts": 'it("[NOTE-006] 実アプリ", async () => {});\n',
    "e2e/tests/launch.rs": "// [NOTE-007]\n#[test]\nfn e2e_in_rust() {}\n",
    "e2e/node_modules/pkg/index.test.js": 'it("[NOTE-008] 依存関係のテスト", () => {});\n',
    "target/debug/x.rs": "// [NOTE-009]\n#[test]\nfn build_output() {}\n",
  });
  assert.deepEqual(
    scanTests(root).map((found) => `${found.area} ${found.file} ${found.ids.join(",")}`),
    [
      "other crates/core/src/notes.rs NOTE-001",
      "tauri src-tauri/src/commands.rs NOTE-002",
      "other src/App.test.tsx NOTE-003",
      "other scripts/check.test.mjs NOTE-005",
      "e2e e2e/specs/notes.e2e.ts NOTE-006",
      "e2e e2e/tests/launch.rs NOTE-007",
    ],
  );
});

// ---- 未対応の一覧・欠番・チェックリスト ----

test("未対応の一覧: 「ID 区分」の行を、見出しごとに読む", () => {
  const { entries, problems } = parsePending(
    [
      "# コメント",
      "## E2E のテスト待ち",
      "NOTE-001 e2e",
      "NOTE-002 e2e   # 行の途中からのコメント",
      "",
      "## コマンドの層",
      "REC-003 test",
      "REC-001 tauri",
    ].join("\n"),
  );
  assert.deepEqual(problems, []);
  assert.deepEqual(entries, [
    { id: "NOTE-001", kind: "e2e", section: "E2E のテスト待ち", line: 3 },
    { id: "NOTE-002", kind: "e2e", section: "E2E のテスト待ち", line: 4 },
    { id: "REC-003", kind: "test", section: "コマンドの層", line: 7 },
    { id: "REC-001", kind: "tauri", section: "コマンドの層", line: 8 },
  ]);
});

test("未対応の一覧: 形の違う行と、同じ行の重複を知らせる", () => {
  const { entries, problems } = parsePending("NOTE-001\nNOTE-002 unit\nnote-003 e2e\nNOTE-004 e2e 余分\nNOTE-005 e2e\nNOTE-005 e2e\nNOTE-005 test\n");
  assert.deepEqual(
    entries.map((entry) => `${entry.id} ${entry.kind}`),
    ["NOTE-005 e2e", "NOTE-005 test"],
  );
  assert.equal(problems.length, 5);
  assert.match(problems[0], /pending\.txt:1 行の形が違います（「ID 区分」。区分は test・tauri・e2e のどれか）: NOTE-001/);
  assert.match(problems[4], /pending\.txt:6 NOTE-005 e2e が 2 回書かれています/);
});

test("欠番の一覧とチェックリストから ID を読む", () => {
  const retired = parseRetired("# 欠番\nNOTE-021\nREC-022  # 統合した\nnot an id\n");
  assert.deepEqual([...retired.ids], ["NOTE-021", "REC-022"]);
  assert.equal(retired.problems.length, 1);

  const ids = checklistIds("対応: REL-001、REL-002\n| A-1 | 5 分 | [KEY-004] |\nA-12 や UTF-8、XREL-0011 は ID の形ではない。SHA-256 は形だけ同じ\n");
  assert.deepEqual([...ids], ["REL-001", "REL-002", "KEY-004", "SHA-256"]);
});

// ---- 検査 ----

test("すべての項目にテストとチェックリストがそろっていれば合格し、件数を数える", () => {
  const items = spec([
    "- **NOTE-001** ［単体］ A。",
    "- **NOTE-002** ［単体・E2E］ B。",
    "- **NOTE-003** ［結合・手動］ C。",
    "- **NOTE-004** ［手動］ D。",
    "- **NOTE-005** ［未検証］ E。",
    "- **NOTE-006** ［E2E］〔見直し予定〕 F。",
  ]);
  const { problems, summary } = checkCoverage({
    items,
    tests: [
      ref(["NOTE-001", "NOTE-002"]),
      ref(["NOTE-002", "NOTE-006"], "e2e"),
      ref(["NOTE-003"], "tauri"),
      ref([]),
    ],
    checklist: new Set(["NOTE-003", "NOTE-004"]),
  });
  assert.deepEqual(problems, []);
  assert.deepEqual(
    { ...summary, pendingSections: undefined },
    {
      total: 6,
      automated: 4,
      covered: 4,
      pendingIds: 0,
      pendingLines: { test: 0, tauri: 0, e2e: 0 },
      pendingSections: undefined,
      manual: 2,
      manualOnly: 1,
      unverified: 1,
      review: 1,
      waiting: 0,
      tests: 4,
      taggedTests: 3,
    },
  );
});

test("テストのない項目・E2E のない項目・チェックリストにない項目は不合格", () => {
  const items = spec([
    "- **NOTE-001** ［単体］ A。",
    "- **NOTE-002** ［単体・E2E］ B。",
    "- **NOTE-003** ［結合・手動］ C。",
    "- **NOTE-004** ［E2E］ D。",
  ]);
  const { problems, summary } = checkCoverage({
    items,
    // E2E のテストからの参照だけでは、単体・結合のテストの代わりにならない。逆も同じ
    tests: [ref(["NOTE-001"], "e2e"), ref(["NOTE-002", "NOTE-003", "NOTE-004"])],
  });
  assert.deepEqual(problems, [
    "NOTE-001［単体］を参照するテストがありません（docs/spec/note.md:1）",
    "NOTE-002［単体・E2E］を参照する E2E のテスト（e2e/ の下）がありません（docs/spec/note.md:2）",
    "NOTE-003［結合・手動］が、リリース前のチェックリスト（docs/release-checklist.md）に載っていません",
    "NOTE-004［E2E］を参照する E2E のテスト（e2e/ の下）がありません（docs/spec/note.md:4）",
  ]);
  assert.equal(summary.covered, 0);
});

test("仕様一覧にない ID を参照するテスト・チェックリスト・未対応の一覧は不合格", () => {
  const items = spec(["- **NOTE-001** ［単体］ A。", "- **NOTE-002** ［単体］ B。"]);
  const { problems } = checkCoverage({
    items,
    tests: [ref(["NOTE-001", "NOTE-002", "NOTE-099"])],
    pending: [pend("REC-001", "test")],
    // SHA-256 のように、仕様の接頭辞でないものは ID とみなさない
    checklist: new Set(["NOTE-002", "NOTE-077", "SHA-256"]),
  });
  assert.deepEqual(problems, [
    "other/x.test.ts:1 テスト「テスト」が、仕様一覧にない ID [NOTE-099] を参照しています",
    "docs/spec/pending.txt:1 REC-001 は仕様一覧にありません（一覧から消してください）",
    "docs/release-checklist.md に NOTE-002 が載っていますが、仕様一覧の種類に「手動」がありません",
    "docs/release-checklist.md が、仕様一覧にない ID NOTE-077 を参照しています",
  ]);
});

test("未対応の一覧に載っている項目は、テストがなくても合格にする", () => {
  const items = spec([
    "- **NOTE-001** ［結合］ まだテストがない。",
    "- **NOTE-002** ［単体・結合・E2E］ 単体のテストだけある。",
    "- **NOTE-003** ［E2E］ E2E だけの項目。",
    "- **NOTE-004** ［単体］ そろっている。",
  ]);
  const { problems, summary } = checkCoverage({
    items,
    tests: [ref(["NOTE-002", "NOTE-004"])],
    pending: [
      pend("NOTE-001", "test", "コマンドの層"),
      pend("NOTE-002", "tauri", "コマンドの層"),
      pend("NOTE-002", "e2e", "E2E"),
      pend("NOTE-003", "e2e", "E2E"),
    ],
  });
  assert.deepEqual(problems, []);
  assert.equal(summary.covered, 1);
  assert.equal(summary.pendingIds, 3);
  assert.deepEqual(summary.pendingLines, { test: 1, tauri: 1, e2e: 2 });
  assert.deepEqual(summary.pendingSections, [
    { name: "コマンドの層", count: 2 },
    { name: "E2E", count: 2 },
  ]);
});

test("未対応の一覧に載っているのに、もうテストがある項目は不合格（一覧から消し忘れ）", () => {
  const items = spec([
    "- **NOTE-001** ［結合］ A。",
    "- **NOTE-002** ［単体・結合］ B。",
    "- **NOTE-003** ［単体・E2E］ C。",
  ]);
  const { problems } = checkCoverage({
    items,
    tests: [ref(["NOTE-001", "NOTE-003"]), ref(["NOTE-002"], "tauri"), ref(["NOTE-003"], "e2e")],
    pending: [pend("NOTE-001", "test"), pend("NOTE-002", "tauri"), pend("NOTE-003", "e2e")],
  });
  assert.deepEqual(problems, [
    "NOTE-001 は未対応の一覧（test）に載っていますが、参照するテストがあります。一覧から消してください",
    "NOTE-002 は未対応の一覧（tauri）に載っていますが、src-tauri/ のテストからの参照があります。一覧から消してください",
    "NOTE-003 は未対応の一覧（e2e）に載っていますが、e2e/ のテストからの参照があります。一覧から消してください",
  ]);
});

test("未対応の一覧の区分が、項目の種類やテストの状況と合わなければ不合格", () => {
  const items = spec([
    "- **NOTE-001** ［手動］ 手動だけ。",
    "- **NOTE-002** ［単体］ E2E の種類がない。",
    "- **NOTE-003** ［単体］ テストが 1 つもないのに tauri。",
    "- **NOTE-004** ［単体］ test と tauri の両方。",
    "- **NOTE-005** ［手動］ 手動だけなのに、テストが参照している。",
  ]);
  const { problems } = checkCoverage({
    items,
    tests: [ref(["NOTE-002", "NOTE-005"])],
    pending: [
      pend("NOTE-001", "test"),
      pend("NOTE-002", "e2e"),
      pend("NOTE-003", "tauri"),
      pend("NOTE-004", "test"),
      pend("NOTE-004", "tauri"),
    ],
    checklist: new Set(["NOTE-001", "NOTE-005"]),
  });
  assert.deepEqual(problems, [
    "NOTE-001 は未対応の一覧（test）に載っていますが、種類に「単体」「結合」がありません",
    "NOTE-002 は未対応の一覧（e2e）に載っていますが、種類に「E2E」がありません",
    "NOTE-003［単体］を参照するテストがありません（docs/spec/note.md:3）",
    "NOTE-004 が、未対応の一覧に test と tauri の両方で載っています（どちらか一方にしてください）",
    "NOTE-005［手動］をテストが参照しています。仕様一覧の種類に「単体」か「結合」を足してください",
  ]);
});

test("〔実装待ち〕の印の付いた項目は、未対応の一覧に載っていなければ不合格", () => {
  const items = spec([
    "- **NOTE-001** ［単体］〔実装待ち〕 実装とテストがそろった。",
    "- **NOTE-002** ［単体・結合］〔実装待ち〕 まだ。",
  ]);
  const { problems, summary } = checkCoverage({
    items,
    tests: [ref(["NOTE-001", "NOTE-002"])],
    pending: [pend("NOTE-002", "tauri")],
  });
  assert.deepEqual(problems, [
    "NOTE-001 に〔実装待ち〕の印がありますが、未対応の一覧に載っていません（実装とテストがそろったら、印を外してください）",
  ]);
  assert.equal(summary.waiting, 2);
});

test("欠番にした ID を項目に使っていれば不合格", () => {
  const items = spec(["- **NOTE-001** ［単体］ A。"]);
  const { problems } = checkCoverage({ items, tests: [ref(["NOTE-001"])], retired: new Set(["NOTE-001", "NOTE-002"]) });
  assert.deepEqual(problems, ["NOTE-001 は欠番です（docs/spec/retired.txt）。項目には新しい番号を使ってください"]);
});

test("--strict では、未対応の一覧に残っている行をすべて不足として数える", () => {
  const items = spec(["- **NOTE-001** ［結合］ A。", "- **NOTE-002** ［単体・E2E］ B。", "- **NOTE-003** ［単体］ C。"]);
  const input = {
    items,
    tests: [ref(["NOTE-002", "NOTE-003"])],
    pending: [pend("NOTE-001", "test"), pend("NOTE-002", "tauri"), pend("NOTE-002", "e2e")],
  };
  assert.deepEqual(checkCoverage(input).problems, []);
  assert.deepEqual(checkCoverage({ ...input, strict: true }).problems, [
    "NOTE-001 が未対応です（参照するテストがない）",
    "NOTE-002 が未対応です（src-tauri/ のテストからの参照がない）",
    "NOTE-002 が未対応です（e2e/ のテストからの参照がない）",
  ]);
});

test("件数の要約を Markdown の表にする", () => {
  const items = spec(["- **NOTE-001** ［単体］ A。", "- **NOTE-002** ［E2E・手動］ B。"]);
  const { summary } = checkCoverage({
    items,
    tests: [ref(["NOTE-001"])],
    pending: [pend("NOTE-002", "e2e", "E2E のテスト待ち")],
    checklist: new Set(["NOTE-002"]),
  });
  const table = formatSummary(summary);
  assert.match(table, /^\| 仕様とテストの対応 \| 件数 \|\n\| --- \| ---: \|\n\| 仕様の項目 \| 2 \|\n/);
  assert.match(table, /\| テストあり（求めるテストがすべてそろっている項目） \| 1 \|/);
  assert.match(table, /\| 未対応（未対応の一覧に載っている項目） \| 1 \|/);
  assert.match(table, /\| 手動（リリース前のチェックリストで確かめる項目） \| 1 \|/);
  assert.match(table, /\| 未対応の理由 \| 行数 \|\n\| --- \| ---: \|\n\| E2E のテスト待ち \| 1 \|\n$/);
});

// ---- コマンド ----

/** 仕様 3 項目（単体 / 単体・E2E / 手動）と、それに合うテスト・一覧・チェックリストを持つ仮のリポジトリ */
function coveredRepo(overrides = {}) {
  return makeRepo({
    "docs/spec/README.md": "# 仕様一覧\n\n- **ここは項目として読まない** ［単体］ 説明の例。\n",
    "docs/spec/note.md": "# NOTE\n\n- **NOTE-001** ［単体］ A。\n- **NOTE-002** ［単体・E2E］ B。\n",
    "docs/spec/rel.md": "# REL\n\n- **REL-001** ［手動］ C。\n",
    "docs/spec/pending.txt": "## E2E のテスト待ち\nNOTE-002 e2e\n",
    "docs/spec/retired.txt": "# 欠番\n",
    "docs/release-checklist.md": "# チェックリスト\n\n対応: REL-001\n",
    "crates/core/src/notes.rs": "// [NOTE-001]\n#[test]\nfn a() {}\n",
    "src/App.test.tsx": 'it("[NOTE-002] B", () => {});\n',
    ...overrides,
  });
}

test("リポジトリを調べる: 仕様一覧・テスト・未対応の一覧・チェックリストを読んで突き合わせる", () => {
  const ok = checkRepository({ root: coveredRepo() });
  assert.deepEqual(ok.problems, []);
  assert.equal(ok.summary.total, 3);
  assert.equal(ok.summary.covered, 1);
  assert.equal(ok.summary.pendingIds, 1);

  const strict = checkRepository({ root: coveredRepo(), strict: true });
  assert.deepEqual(strict.problems, ["NOTE-002 が未対応です（e2e/ のテストからの参照がない）"]);

  // e2e/ にテストができたら、一覧から消すまで不合格
  const withE2e = coveredRepo({ "e2e/notes.spec.ts": 'it("[NOTE-002] B", async () => {});\n' });
  assert.match(checkRepository({ root: withE2e }).problems[0], /NOTE-002 は未対応の一覧（e2e）に載っていますが/);

  // 一覧もチェックリストもなければ、足りないものをすべて知らせる
  const bare = coveredRepo({ "docs/spec/pending.txt": "", "docs/release-checklist.md": "" });
  assert.equal(checkRepository({ root: bare }).problems.length, 2);
});

test("コマンドとして実行すると、要約を標準出力に書き、未対応の一覧は減らすだけにするよう伝える", () => {
  const root = coveredRepo();
  const summaryFile = join(root, "summary.md");
  const ok = spawnSync(process.execPath, [SCRIPT, "--root", root], {
    encoding: "utf8",
    env: { ...process.env, GITHUB_ACTIONS: "", GITHUB_STEP_SUMMARY: summaryFile },
  });
  assert.equal(ok.status, 0, ok.stderr);
  assert.match(ok.stdout, /\| 仕様の項目 \| 3 \|/);
  assert.match(ok.stdout, /未対応の項目が 1 件あります（docs\/spec\/pending\.txt）。この一覧は減らすだけにしてください。/);
  // ジョブの Summary にも同じ表を出す
  const written = readFileSync(summaryFile, "utf8");
  assert.match(written, /^### 仕様とテストの対応\n\n\| 仕様とテストの対応 \| 件数 \|/);
  assert.match(written, /\| 未対応（未対応の一覧に載っている項目） \| 1 \|/);
});

test("コマンドとして実行し、問題があれば終了コード 1 で一覧を出す。--strict は未対応も問題にする", () => {
  const root = coveredRepo({ "src/App.test.tsx": 'it("[NOTE-009] 書き間違い", () => {});\n' });
  const env = { ...process.env, GITHUB_ACTIONS: "", GITHUB_STEP_SUMMARY: "" };
  const bad = spawnSync(process.execPath, [SCRIPT, "--root", root], { encoding: "utf8", env });
  assert.equal(bad.status, 1);
  assert.match(bad.stderr, /エラー: 仕様とテストの対応に問題があります（2 件）/);
  assert.match(bad.stderr, /- src\/App\.test\.tsx:1 テスト「\[NOTE-009\] 書き間違い」が、仕様一覧にない ID \[NOTE-009\] を参照しています/);
  assert.match(bad.stderr, /- NOTE-002［単体・E2E］を参照するテストがありません/);

  const strict = spawnSync(process.execPath, [SCRIPT, "--root", coveredRepo(), "--strict"], { encoding: "utf8", env });
  assert.equal(strict.status, 1);
  assert.match(strict.stderr, /問題があります（1 件）〔--strict: 未対応の一覧を見逃さない〕\n- NOTE-002 が未対応です/);

  const unknown = spawnSync(process.execPath, [SCRIPT, "--unknown"], { encoding: "utf8", env });
  assert.equal(unknown.status, 1);
  assert.match(unknown.stderr, /使い方: node scripts\/check-spec-coverage\.mjs \[--strict\] \[--root <リポジトリのルート>\]/);
});

test("仕様一覧のフォルダがない・項目がないときは、分かるメッセージで終わる", () => {
  assert.throws(() => checkRepository({ root: makeRepo({ "README.md": "" }) }), /docs[\\/]spec を読めません/);
  assert.throws(
    () => checkRepository({ root: makeRepo({ "docs/spec/note.md": "# NOTE\n" }) }),
    /docs[\\/]spec に仕様の項目がありません/,
  );
});
