#!/usr/bin/env node
// 仕様一覧（docs/spec/）のすべての項目が、テストかリリース前のチェックリストに結び付いているかを確かめる。
//
// 使い方:
//   node scripts/check-spec-coverage.mjs [--strict] [--root <リポジトリのルート>]
//
// 確かめること:
// - 仕様一覧の項目の行が「- **NOTE-001** ［単体・E2E］ 〜」の形で、ID の重複・形の誤り・知らない種類がないこと
// - 種類に「単体」か「結合」がある項目は、その ID を参照するテストが 1 つ以上あること
// - 種類に「E2E」がある項目は、e2e/ の下のテストからの参照が 1 つ以上あること
// - 種類に「手動」がある項目は、リリース前のチェックリスト（docs/release-checklist.md）に ID が載っていること
// - テストとチェックリストが、仕様一覧にない ID を参照していないこと
// - 〔実装待ち〕の印の付いた項目が、未対応の一覧に載っていること
//
// テストは、名前（Rust は直前のコメント）に「[NOTE-001]」と書いて項目に結び付ける。
//
// まだテストのない項目は、未対応の一覧（docs/spec/pending.txt）に載っているものだけ見逃す。
// 一覧に載っているのに、もうテストがある項目はエラーにする（一覧から消し忘れている）。
// --strict を付けると一覧を見逃しに使わず、載っている行をすべて不足として数える。

import { appendFileSync, existsSync, readFileSync, readdirSync } from "node:fs";
import { basename, join, relative, sep } from "node:path";
import { UsageError, isMain, readOptions, run } from "./lib/cli.mjs";

const USAGE = "node scripts/check-spec-coverage.mjs [--strict] [--root <リポジトリのルート>]";

export const SPEC_DIR = "docs/spec";
export const PENDING_FILE = "docs/spec/pending.txt";
export const RETIRED_FILE = "docs/spec/retired.txt";
export const CHECKLIST_FILE = "docs/release-checklist.md";

/** 確かめ方の種類。「未検証」は、テストもチェックリストも求めない（ほかの種類と一緒には書けない） */
export const KINDS = ["単体", "結合", "E2E", "手動", "未検証"];
/**
 * 項目に付けられる印。
 * - 見直し予定: いまの振る舞いを書いてあるが、変えることを考えている
 * - 実装待ち: 決まった振る舞いを書いてあるが、実装がまだ追いついていない（未対応の一覧に載っていること）
 */
export const MARKS = ["見直し予定", "実装待ち"];

/**
 * 未対応の一覧の区分。
 * - test: その ID を参照するテスト（e2e/ の外）が 1 つもない
 * - tauri: 単体のテストはあるが、src-tauri/ の下のテスト（本体のコマンドの層）からの参照がない
 * - e2e: e2e/ の下のテストからの参照がない
 */
export const PENDING_KINDS = ["test", "tauri", "e2e"];

const ID_FORM = /^[A-Z]{3,4}-\d{3}$/;
const ID_IN_BRACKETS = /\[([A-Z]{3,4}-\d{3})\]/g;
const ID_ANYWHERE = /(?<![A-Za-z0-9-])([A-Z]{3,4}-\d{3})(?![0-9])/g;

function idsIn(text, pattern) {
  return [...text.matchAll(pattern)].map((match) => match[1]);
}

// ---- 仕様一覧 ----

/**
 * 仕様一覧のファイル 1 つを読む。項目の行は、行頭が「- **」で始まる行。
 * 返すのは { items: [{ id, kinds, marks, text, file, line }], problems: [文] }。
 */
export function parseSpecFile(file, text) {
  const items = [];
  const problems = [];
  const prefix = basename(file).replace(/\.md$/, "").toUpperCase();
  text.split(/\r?\n/).forEach((raw, index) => {
    if (!raw.startsWith("- **")) return;
    const where = `${file}:${index + 1}`;
    const match = /^- \*\*([^*]+)\*\* ［([^］]*)］((?:〔[^〕]*〕)*) (\S.*)$/.exec(raw);
    if (!match) {
      problems.push(`${where} 項目の行の形が違います（「- **ID** ［種類］ 文」の形にしてください）: ${raw.slice(0, 40)}`);
      return;
    }
    const [, id, kindText, markText, body] = match;
    if (!ID_FORM.test(id)) {
      problems.push(`${where} ID の形が違います: ${id}（「NOTE-001」のように、英大文字 3〜4 字 + ハイフン + 数字 3 桁）`);
      return;
    }
    if (id.split("-")[0] !== prefix) {
      problems.push(`${where} ${id} は ${prefix.toLowerCase()}.md の項目ではありません（接頭辞とファイル名をそろえてください）`);
    }
    const kinds = kindText.split("・");
    for (const kind of kinds) {
      if (!KINDS.includes(kind)) problems.push(`${where} ${id} の種類「${kind}」は使えません（${KINDS.join("・")} のどれか）`);
    }
    if (new Set(kinds).size !== kinds.length) problems.push(`${where} ${id} に同じ種類が 2 回書かれています`);
    if (kinds.includes("未検証") && kinds.length > 1) {
      problems.push(`${where} ${id} の「未検証」は、ほかの種類と一緒には書けません`);
    }
    const marks = [...markText.matchAll(/〔([^〕]*)〕/g)].map((mark) => mark[1]);
    for (const mark of marks) {
      if (!MARKS.includes(mark)) problems.push(`${where} ${id} の印「〔${mark}〕」は使えません（〔${MARKS.join("〕〔")}〕）`);
    }
    items.push({ id, kinds, marks, text: body, file, line: index + 1 });
  });
  return { items, problems };
}

/** ファイルをまたいだ ID の重複を調べ、ID → 項目の Map にまとめる */
export function collectSpec(parsedFiles) {
  const items = new Map();
  const problems = [];
  for (const parsed of parsedFiles) {
    problems.push(...parsed.problems);
    for (const item of parsed.items) {
      const first = items.get(item.id);
      if (first) {
        problems.push(`${item.file}:${item.line} ${item.id} が重複しています（${first.file}:${first.line} にもあります）`);
      } else {
        items.set(item.id, item);
      }
    }
  }
  return { items, problems };
}

// ---- テストからの参照 ----

/**
 * Rust のソースからテストを拾う。`#[test]`・`#[tokio::test]` の付いた関数ごとに、
 * 直前に続くコメント（`//`・`///`）に書かれた ID を参照として返す。`#[ignore]` のテストは数えない。
 */
export function scanRust(text) {
  const lines = text.split(/\r?\n/);
  const tests = [];
  lines.forEach((line, index) => {
    const fn = /^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)/.exec(line);
    if (!fn) return;
    let isTest = false;
    let ignored = false;
    const ids = [];
    for (let i = index - 1; i >= 0; i -= 1) {
      const above = lines[i].trim();
      if (above.startsWith("#[")) {
        if (/^#\[(?:tokio::)?test\b/.test(above)) isTest = true;
        if (/^#\[ignore\b/.test(above)) ignored = true;
      } else if (above.startsWith("//")) {
        ids.unshift(...idsIn(above, ID_IN_BRACKETS));
      } else {
        break;
      }
    }
    if (isTest && !ignored) tests.push({ name: fn[1], line: index + 1, ids });
  });
  return tests;
}

/** 文字列リテラルの終わり（閉じる引用符の位置）を返す。閉じていなければ -1 */
function stringEnd(text, start) {
  const quote = text[start];
  for (let i = start + 1; i < text.length; i += 1) {
    if (text[i] === "\\") i += 1;
    else if (text[i] === quote) return i;
    else if (text[i] === "\n" && quote !== "`") return -1;
  }
  return -1;
}

/** 開きかっこに対応する閉じかっこの位置を返す（文字列の中のかっこは数えない）。なければ -1 */
function closingParen(text, open) {
  let depth = 0;
  for (let i = open; i < text.length; i += 1) {
    const char = text[i];
    if (char === '"' || char === "'" || char === "`") {
      const end = stringEnd(text, i);
      if (end === -1) return -1;
      i = end;
    } else if (char === "(") {
      depth += 1;
    } else if (char === ")") {
      depth -= 1;
      if (depth === 0) return i;
    }
  }
  return -1;
}

/**
 * JavaScript / TypeScript のテスト（vitest・node:test・E2E）を拾う。`it("名前", …)`・`test("名前", …)` と、
 * `it.each(表)("名前", …)` の形の名前に書かれた ID を参照として返す。`.skip`・`.todo` のテストは数えない。
 */
export function scanScript(text) {
  const tests = [];
  const call = /(?<![\w.$])(?:it|test)((?:\.\w+)*)\s*\(/g;
  for (const match of text.matchAll(call)) {
    const lineStart = text.lastIndexOf("\n", match.index) + 1;
    // コメントにした行は数えない
    if (text.slice(lineStart, match.index).includes("//")) continue;
    const modifiers = match[1].split(".").filter(Boolean);
    let open = match.index + match[0].length - 1;
    let name = null;
    // `it.each(表)("名前")` のように、名前の前に引数のかっこが挟まる形は 1 つ読み飛ばす
    for (let hop = 0; hop < 2 && name === null; hop += 1) {
      const rest = /^\s*/.exec(text.slice(open + 1))[0].length + open + 1;
      if (['"', "'", "`"].includes(text[rest])) {
        const end = stringEnd(text, rest);
        if (end !== -1) name = text.slice(rest + 1, end);
        break;
      }
      const close = closingParen(text, open);
      if (close === -1) break;
      const next = /^\s*\(/.exec(text.slice(close + 1));
      if (!next) break;
      open = close + next[0].length;
    }
    if (name === null) continue;
    if (modifiers.some((modifier) => modifier === "skip" || modifier === "todo")) continue;
    const line = text.slice(0, match.index).split("\n").length;
    tests.push({ name, line, ids: idsIn(name, ID_IN_BRACKETS) });
  }
  return tests;
}

const SKIP_DIRS = new Set(["node_modules", "target", "dist", "gen", "coverage", ".git"]);

function walk(dir, visit) {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (!SKIP_DIRS.has(entry.name)) walk(path, visit);
    } else if (entry.isFile()) {
      visit(path);
    }
  }
}

/** テストの置き場所（リポジトリのルートからの相対パス）ごとの拾い方 */
const TEST_ROOTS = [
  { dir: "crates", area: "other", pick: (path) => (path.endsWith(".rs") ? scanRust : null) },
  { dir: "src-tauri", area: "tauri", pick: (path) => (path.endsWith(".rs") ? scanRust : null) },
  { dir: "src", area: "other", pick: (path) => (/\.test\.tsx?$/.test(path) ? scanScript : null) },
  { dir: "scripts", area: "other", pick: (path) => (/\.test\.mjs$/.test(path) ? scanScript : null) },
  {
    dir: "e2e",
    area: "e2e",
    pick: (path) => (path.endsWith(".rs") ? scanRust : /\.[cm]?[jt]sx?$/.test(path) ? scanScript : null),
  },
];

/**
 * リポジトリのテストをすべて拾う。返すのは [{ file, line, name, ids, area }]。
 * area は、e2e/ の下なら "e2e"、src-tauri/ の下なら "tauri"、それ以外は "other"。
 */
export function scanTests(root) {
  const tests = [];
  for (const { dir, area, pick } of TEST_ROOTS) {
    walk(join(root, dir), (path) => {
      const scan = pick(path);
      if (!scan) return;
      const file = relative(root, path).split(sep).join("/");
      for (const found of scan(readFileSync(path, "utf8"))) tests.push({ ...found, file, area });
    });
  }
  return tests;
}

// ---- 未対応の一覧・欠番・チェックリスト ----

/**
 * 未対応の一覧を読む。1 行に「ID 区分」（区分は test・tauri・e2e）。`#` より後ろはコメント。
 * `## 見出し` の行は、その下の行をまとめる理由の見出しで、集計に使う。
 */
export function parsePending(text, file = PENDING_FILE) {
  const entries = [];
  const problems = [];
  const seen = new Set();
  let section = "（見出しなし）";
  text.split(/\r?\n/).forEach((raw, index) => {
    const heading = /^##\s+(.+?)\s*$/.exec(raw);
    if (heading) {
      section = heading[1];
      return;
    }
    const line = raw.replace(/#.*$/, "").trim();
    if (line === "") return;
    const where = `${file}:${index + 1}`;
    const [id, kind, ...rest] = line.split(/\s+/);
    if (!ID_FORM.test(id) || !PENDING_KINDS.includes(kind) || rest.length > 0) {
      problems.push(`${where} 行の形が違います（「ID 区分」。区分は ${PENDING_KINDS.join("・")} のどれか）: ${line}`);
      return;
    }
    if (seen.has(`${id} ${kind}`)) {
      problems.push(`${where} ${id} ${kind} が 2 回書かれています`);
      return;
    }
    seen.add(`${id} ${kind}`);
    entries.push({ id, kind, section, line: index + 1 });
  });
  return { entries, problems };
}

/** 欠番の一覧（1 行に 1 つの ID。`#` より後ろはコメント）を読む */
export function parseRetired(text, file = RETIRED_FILE) {
  const ids = new Set();
  const problems = [];
  text.split(/\r?\n/).forEach((raw, index) => {
    const line = raw.replace(/#.*$/, "").trim();
    if (line === "") return;
    if (!ID_FORM.test(line)) problems.push(`${file}:${index + 1} ID の形が違います: ${line}`);
    else ids.add(line);
  });
  return { ids, problems };
}

/** チェックリストに書かれている、ID の形の文字列（角かっこなしでも拾う。仕様の ID かどうかは、検査の側で見分ける） */
export function checklistIds(text) {
  return new Set(idsIn(text, ID_ANYWHERE));
}

// ---- 検査 ----

/**
 * 仕様一覧・テスト・未対応の一覧・チェックリストを突き合わせる。
 * 返すのは { problems: [文], summary }。problems が空なら合格。
 */
export function checkCoverage({ items, tests, pending = [], retired = new Set(), checklist = new Set(), strict = false }) {
  const problems = [];

  const refs = new Map(); // ID → { other, tauri, e2e }（参照しているテストの数）
  for (const test of tests) {
    for (const id of new Set(test.ids)) {
      if (!items.has(id)) {
        problems.push(`${test.file}:${test.line} テスト「${test.name}」が、仕様一覧にない ID [${id}] を参照しています`);
        continue;
      }
      const count = refs.get(id) ?? { other: 0, tauri: 0, e2e: 0 };
      count[test.area] += 1;
      refs.set(id, count);
    }
  }

  const pendingOf = new Map(); // ID → 区分の Set
  for (const entry of pending) {
    if (!items.has(entry.id)) {
      problems.push(`${PENDING_FILE}:${entry.line} ${entry.id} は仕様一覧にありません（一覧から消してください）`);
      continue;
    }
    if (!pendingOf.has(entry.id)) pendingOf.set(entry.id, new Set());
    pendingOf.get(entry.id).add(entry.kind);
  }

  for (const id of retired) {
    if (items.has(id)) problems.push(`${id} は欠番です（${RETIRED_FILE}）。項目には新しい番号を使ってください`);
  }

  // チェックリストは角かっこなしで ID を書くので、仕様の接頭辞でないもの（SHA-256 など）は ID とみなさない
  const prefixes = new Set([...items.keys()].map((id) => id.split("-")[0]));
  for (const id of checklist) {
    if (!prefixes.has(id.split("-")[0])) continue;
    if (!items.has(id)) {
      problems.push(`${CHECKLIST_FILE} が、仕様一覧にない ID ${id} を参照しています`);
    } else if (!items.get(id).kinds.includes("手動")) {
      problems.push(`${CHECKLIST_FILE} に ${id} が載っていますが、仕様一覧の種類に「手動」がありません`);
    }
  }

  let covered = 0;
  const missing = { test: [], tauri: [], e2e: [] };
  for (const item of items.values()) {
    const { id, kinds } = item;
    const count = refs.get(id) ?? { other: 0, tauri: 0, e2e: 0 };
    const outsideE2e = count.other + count.tauri;
    const listed = pendingOf.get(id) ?? new Set();
    const needsTest = kinds.includes("単体") || kinds.includes("結合");
    const needsE2e = kinds.includes("E2E");
    const lacks = [];
    let ok = true;
    const fail = (problem) => {
      ok = false;
      problems.push(problem);
    };

    if (needsTest) {
      if (listed.has("test") && listed.has("tauri")) {
        fail(`${id} が、未対応の一覧に test と tauri の両方で載っています（どちらか一方にしてください）`);
      } else if (listed.has("test")) {
        if (outsideE2e > 0) {
          fail(`${id} は未対応の一覧（test）に載っていますが、参照するテストがあります。一覧から消してください`);
        } else {
          lacks.push("test");
        }
      } else if (outsideE2e === 0) {
        fail(`${id}［${kinds.join("・")}］を参照するテストがありません（${item.file}:${item.line}）`);
      } else if (listed.has("tauri")) {
        if (count.tauri > 0) {
          fail(`${id} は未対応の一覧（tauri）に載っていますが、src-tauri/ のテストからの参照があります。一覧から消してください`);
        } else {
          lacks.push("tauri");
        }
      }
    } else {
      for (const kind of ["test", "tauri"]) {
        if (listed.has(kind)) {
          fail(`${id} は未対応の一覧（${kind}）に載っていますが、種類に「単体」「結合」がありません`);
        }
      }
      if (outsideE2e > 0 && !needsE2e) {
        fail(`${id}［${kinds.join("・")}］をテストが参照しています。仕様一覧の種類に「単体」か「結合」を足してください`);
      }
    }

    if (needsE2e) {
      if (listed.has("e2e")) {
        if (count.e2e > 0) {
          fail(`${id} は未対応の一覧（e2e）に載っていますが、e2e/ のテストからの参照があります。一覧から消してください`);
        } else {
          lacks.push("e2e");
        }
      } else if (count.e2e === 0) {
        fail(`${id}［${kinds.join("・")}］を参照する E2E のテスト（e2e/ の下）がありません（${item.file}:${item.line}）`);
      }
    } else if (listed.has("e2e")) {
      fail(`${id} は未対応の一覧（e2e）に載っていますが、種類に「E2E」がありません`);
    }

    if (kinds.includes("手動") && !checklist.has(id)) {
      fail(`${id}［${kinds.join("・")}］が、リリース前のチェックリスト（${CHECKLIST_FILE}）に載っていません`);
    }

    if (item.marks.includes("実装待ち") && listed.size === 0) {
      fail(`${id} に〔実装待ち〕の印がありますが、未対応の一覧に載っていません（実装とテストがそろったら、印を外してください）`);
    }

    for (const kind of lacks) missing[kind].push(id);
    if ((needsTest || needsE2e) && ok && lacks.length === 0) covered += 1;
  }

  if (strict) {
    for (const kind of PENDING_KINDS) {
      for (const id of missing[kind]) {
        problems.push(`${id} が未対応です（${PENDING_LABEL[kind]}）`);
      }
    }
  }

  const all = [...items.values()];
  const automated = all.filter((item) => item.kinds.some((kind) => ["単体", "結合", "E2E"].includes(kind)));
  const sections = new Map();
  for (const entry of pending) sections.set(entry.section, (sections.get(entry.section) ?? 0) + 1);
  const summary = {
    total: all.length,
    automated: automated.length,
    covered,
    pendingIds: new Set(Object.values(missing).flat()).size,
    pendingLines: { test: missing.test.length, tauri: missing.tauri.length, e2e: missing.e2e.length },
    pendingSections: [...sections].map(([name, count]) => ({ name, count })),
    manual: all.filter((item) => item.kinds.includes("手動")).length,
    manualOnly: all.filter((item) => item.kinds.length === 1 && item.kinds[0] === "手動").length,
    unverified: all.filter((item) => item.kinds.includes("未検証")).length,
    review: all.filter((item) => item.marks.includes("見直し予定")).length,
    waiting: all.filter((item) => item.marks.includes("実装待ち")).length,
    tests: tests.length,
    taggedTests: tests.filter((test) => test.ids.length > 0).length,
  };
  return { problems, summary };
}

const PENDING_LABEL = {
  test: "参照するテストがない",
  tauri: "src-tauri/ のテストからの参照がない",
  e2e: "e2e/ のテストからの参照がない",
};

/** 件数の要約（Markdown の表。ジョブの Summary と標準出力の両方に使う） */
export function formatSummary(summary) {
  const lines = [
    "| 仕様とテストの対応 | 件数 |",
    "| --- | ---: |",
    `| 仕様の項目 | ${summary.total} |`,
    `| 自動テストの対象（種類に 単体・結合・E2E がある項目） | ${summary.automated} |`,
    `| テストあり（求めるテストがすべてそろっている項目） | ${summary.covered} |`,
    `| 未対応（未対応の一覧に載っている項目） | ${summary.pendingIds} |`,
    `| 　参照するテストがない（test） | ${summary.pendingLines.test} |`,
    `| 　本体のコマンドの層のテストがない（tauri） | ${summary.pendingLines.tauri} |`,
    `| 　E2E のテストがない（e2e） | ${summary.pendingLines.e2e} |`,
    `| 手動（リリース前のチェックリストで確かめる項目） | ${summary.manual} |`,
    `| 　うち、手動だけの項目 | ${summary.manualOnly} |`,
    `| 未検証の項目 | ${summary.unverified} |`,
    `| 見直し予定の項目 | ${summary.review} |`,
    `| 実装待ちの項目 | ${summary.waiting} |`,
    `| テスト（うち、仕様の ID が付いているもの） | ${summary.tests}（${summary.taggedTests}） |`,
  ];
  // 未対応の一覧の、理由（見出し）ごとの行数
  if (summary.pendingSections.length > 0) {
    lines.push("", "| 未対応の理由 | 行数 |", "| --- | ---: |");
    for (const section of summary.pendingSections) lines.push(`| ${section.name} | ${section.count} |`);
  }
  return `${lines.join("\n")}\n`;
}

// ---- コマンド ----

function readText(path) {
  try {
    return readFileSync(path, "utf8");
  } catch (error) {
    throw new UsageError(`${path} を読めません: ${error.message}`);
  }
}

/** リポジトリを調べる。返すのは { problems, summary } */
export function checkRepository({ root = ".", strict = false } = {}) {
  const specDir = join(root, SPEC_DIR);
  let specFiles;
  try {
    specFiles = readdirSync(specDir)
      .filter((name) => name.endsWith(".md") && name !== "README.md")
      .sort();
  } catch (error) {
    throw new UsageError(`${specDir} を読めません: ${error.message}`);
  }
  const { items, problems } = collectSpec(
    specFiles.map((name) => parseSpecFile(`${SPEC_DIR}/${name}`, readText(join(specDir, name)))),
  );
  if (items.size === 0) throw new UsageError(`${specDir} に仕様の項目がありません`);

  const pendingPath = join(root, PENDING_FILE);
  const pending = existsSync(pendingPath) ? parsePending(readText(pendingPath)) : { entries: [], problems: [] };
  const retiredPath = join(root, RETIRED_FILE);
  const retired = existsSync(retiredPath) ? parseRetired(readText(retiredPath)) : { ids: new Set(), problems: [] };
  const checklistPath = join(root, CHECKLIST_FILE);
  const checklist = existsSync(checklistPath) ? checklistIds(readText(checklistPath)) : new Set();

  const result = checkCoverage({
    items,
    tests: scanTests(root),
    pending: pending.entries,
    retired: retired.ids,
    checklist,
    strict,
  });
  return {
    problems: [...problems, ...pending.problems, ...retired.problems, ...result.problems],
    summary: result.summary,
  };
}

function main(argv) {
  const options = readOptions(argv, { strict: { flag: true }, root: {} }, USAGE);
  const strict = options.strict === true;
  const { problems, summary } = checkRepository({ root: options.root ?? ".", strict });

  const table = formatSummary(summary);
  process.stdout.write(table);
  if (process.env.GITHUB_STEP_SUMMARY) {
    appendFileSync(process.env.GITHUB_STEP_SUMMARY, `### 仕様とテストの対応\n\n${table}\n`);
  }

  if (problems.length > 0) {
    throw new UsageError(
      `仕様とテストの対応に問題があります（${problems.length} 件）${strict ? "〔--strict: 未対応の一覧を見逃さない〕" : ""}\n` +
        problems.map((problem) => `- ${problem}`).join("\n"),
    );
  }
  if (summary.pendingIds > 0) {
    process.stdout.write(
      `\n未対応の項目が ${summary.pendingIds} 件あります（${PENDING_FILE}）。` +
        "この一覧は減らすだけにしてください。テストを書いたら行を消し、検査を通すために行を足すことはしません。\n",
    );
  }
}

if (isMain(import.meta.url)) await run(main);
