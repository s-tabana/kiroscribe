import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, test } from "node:test";
import { fileURLToPath } from "node:url";
import { cargoVersion, checkVersion } from "./check-version.mjs";

const SCRIPT = fileURLToPath(new URL("./check-version.mjs", import.meta.url));

const dirs = [];
afterEach(() => {
  for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

/** package.json・Cargo.toml・tauri.conf.json だけを持つ、仮のリポジトリを作る */
function makeRepo({ pkg = "0.2.0", cargo = "0.2.0", tauri = "../package.json" } = {}) {
  const root = mkdtempSync(join(tmpdir(), "kiroscribe-version-"));
  dirs.push(root);
  writeFileSync(join(root, "package.json"), JSON.stringify({ name: "kiroscribe", version: pkg }));
  writeFileSync(
    join(root, "Cargo.toml"),
    `[workspace]\nmembers = ["src-tauri"]\n\n[workspace.package]\nversion = "${cargo}"\nedition = "2021"\n\n[workspace.dependencies]\nserde = { version = "1" }\n`,
  );
  mkdirSync(join(root, "src-tauri"));
  const config = { productName: "Kiroscribe" };
  if (tauri !== null) config.version = tauri;
  writeFileSync(join(root, "src-tauri", "tauri.conf.json"), JSON.stringify(config));
  return root;
}

test("3 か所のバージョンが同じなら、そのバージョンを返す", () => {
  assert.equal(checkVersion({ root: makeRepo() }), "0.2.0");
});

test("タグが「v + バージョン」なら通す", () => {
  assert.equal(checkVersion({ root: makeRepo(), tag: "v0.2.0" }), "0.2.0");
});

test("タグとバージョンが違えばエラーにする", () => {
  const root = makeRepo();
  assert.throws(() => checkVersion({ root, tag: "v0.3.0" }), /タグは v0\.3\.0 です（v0\.2\.0 のはず）/);
  assert.throws(() => checkVersion({ root, tag: "0.2.0" }), /タグは 0\.2\.0 です/);
});

test("Cargo.toml を上げ忘れていればエラーにする", () => {
  assert.throws(
    () => checkVersion({ root: makeRepo({ cargo: "0.1.0" }) }),
    /Cargo\.toml のバージョンは 0\.1\.0 です/,
  );
});

test("tauri.conf.json にバージョンを直接書いた場合も比べる", () => {
  assert.equal(checkVersion({ root: makeRepo({ tauri: "0.2.0" }) }), "0.2.0");
  assert.throws(
    () => checkVersion({ root: makeRepo({ tauri: "0.1.0" }) }),
    /tauri\.conf\.json のバージョンは 0\.1\.0 です/,
  );
});

test("tauri.conf.json にバージョンがなければ、Cargo.toml との一致だけを見る", () => {
  assert.equal(checkVersion({ root: makeRepo({ tauri: null }) }), "0.2.0");
});

test("食い違いはまとめて知らせる", () => {
  const root = makeRepo({ cargo: "0.1.0", tauri: "0.1.5" });
  assert.throws(
    () => checkVersion({ root, tag: "v9.9.9" }),
    (error) =>
      /Cargo\.toml/.test(error.message) &&
      /tauri\.conf\.json/.test(error.message) &&
      /タグ/.test(error.message),
  );
});

test("Cargo.toml は [workspace.package] の version だけを読む（依存の version は読まない）", () => {
  assert.equal(
    cargoVersion('[workspace.dependencies]\nserde = { version = "1" }\n\n[workspace.package]\r\nversion = "1.2.3" # 版\r\n'),
    "1.2.3",
  );
  assert.equal(cargoVersion('[package]\nname = "x"\nversion = "0.9.0"\n'), "0.9.0");
  assert.equal(cargoVersion('[dependencies]\nversion = "9"\n'), null);
});

test("コマンドとして実行すると、バージョンだけを標準出力に書く", () => {
  const root = makeRepo();
  const ok = spawnSync(process.execPath, [SCRIPT, "--root", root, "--tag", "v0.2.0"], {
    encoding: "utf8",
  });
  assert.equal(ok.status, 0, ok.stderr);
  assert.equal(ok.stdout, "0.2.0\n");

  const bad = spawnSync(process.execPath, [SCRIPT, "--root", root, "--tag", "v0.2.1"], {
    encoding: "utf8",
    env: { ...process.env, GITHUB_ACTIONS: "" },
  });
  assert.equal(bad.status, 1);
  assert.equal(bad.stdout, "");
  assert.match(bad.stderr, /エラー: バージョンが食い違っています/);
});
