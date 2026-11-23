// Windows のインストール状態を調べる・変える道具（本物のインストーラを使うテスト用）。
//
// インストーラ（NSIS、installMode: currentUser）は、利用者ごとの場所（%LOCALAPPDATA%\Kiroscribe）に入れ、
// 「アプリと機能」の一覧に出す情報を HKCU\...\Uninstall\Kiroscribe に書く。
// ここでは、その情報・実行ファイルのバージョン・ショートカットを読んで、インストールの結果を確かめる。

import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

export const PRODUCT_NAME = "Kiroscribe";
export const UNINSTALL_KEY = `HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${PRODUCT_NAME}`;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * `reg query` の出力から、値の一覧を取り出す（キーがなければ null）。
 * 出力の 1 行は「    名前    REG_SZ    値」の形（区切りは空白 4 つ）。
 */
export function parseRegQuery(output) {
  const values = {};
  for (const line of output.split(/\r?\n/)) {
    const match = /^\s{4}(.+?)\s{4}(REG_[A-Z_]+)\s{4}(.*)$/.exec(line) ?? /^\s{4}(.+?)\s{4}(REG_[A-Z_]+)$/.exec(line);
    if (!match) continue;
    const [, name, type, raw = ""] = match;
    values[name] = type === "REG_DWORD" ? Number.parseInt(raw, 16) : raw;
  }
  return values;
}

/** レジストリのキーの値を読む。キーがなければ null */
export function readRegistry(key) {
  const result = spawnSync("reg", ["query", key], { encoding: "utf8", windowsHide: true });
  if (result.status !== 0) return null;
  return parseRegQuery(result.stdout);
}

/** 「C:\path」のように引用符で囲まれた値から、引用符を外す */
export function unquote(value) {
  return typeof value === "string" ? value.replace(/^"(.*)"$/, "$1") : value;
}

/**
 * インストールの情報（「アプリと機能」の一覧に出る内容）。インストールされていなければ null。
 * { displayName, displayVersion, publisher, installLocation, uninstaller, exe }
 */
export function installed() {
  const values = readRegistry(UNINSTALL_KEY);
  if (!values) return null;
  const installLocation = unquote(values.InstallLocation ?? "");
  return {
    displayName: values.DisplayName,
    displayVersion: values.DisplayVersion,
    publisher: values.Publisher,
    installLocation,
    uninstaller: unquote(values.UninstallString ?? ""),
    exe: installLocation && values.MainBinaryName ? path.join(installLocation, values.MainBinaryName) : null,
  };
}

/** Windows の起動時に起動するプログラムの一覧（利用者ごと）。タスクマネージャーの「スタートアップ アプリ」に出る */
export const RUN_KEY = "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run";

/** 自動起動に登録されているコマンド（なければ null） */
export function autostartCommand() {
  return readRegistry(RUN_KEY)?.[PRODUCT_NAME] ?? null;
}

/** 自動起動の登録を書く（アプリが登録したのと同じ状態を作る） */
export function setAutostartCommand(command) {
  const result = spawnSync("reg", ["add", RUN_KEY, "/v", PRODUCT_NAME, "/t", "REG_SZ", "/d", command, "/f"], {
    encoding: "utf8",
    windowsHide: true,
  });
  if (result.status !== 0) throw new Error(`自動起動の登録を書けません: ${result.stderr}`);
}

/** 自動起動の登録を消す（なければ何もしない） */
export function removeAutostartCommand() {
  spawnSync("reg", ["delete", RUN_KEY, "/v", PRODUCT_NAME, "/f"], { windowsHide: true });
}

/**
 * 自動起動に登録されたコマンドを、実行ファイルと引数に分ける（Windows のコマンドラインの決まりのうち、ここで要る分だけ）。
 * 実行ファイルは、引用符で囲まれていてもいなくてもよい（空白を含むパスは、引用符がないと分け方があいまいになる）
 */
export function splitCommand(command, exe) {
  const quoted = /^"([^"]+)"(?:\s+(.*))?$/.exec(command);
  if (quoted) return { file: quoted[1], args: quoted[2] ? quoted[2].split(/\s+/) : [] };
  if (exe && command.toLowerCase().startsWith(exe.toLowerCase())) {
    const rest = command.slice(exe.length).trim();
    return { file: command.slice(0, exe.length), args: rest ? rest.split(/\s+/) : [] };
  }
  const [file, ...args] = command.split(/\s+/);
  return { file, args };
}

/** 実行ファイルに埋め込まれたバージョン（ファイルのプロパティの「製品バージョン」） */
export function fileVersion(exe) {
  const result = spawnSync(
    "powershell.exe",
    ["-NoProfile", "-NonInteractive", "-Command", "(Get-Item -LiteralPath $env:KIROSCRIBE_TARGET).VersionInfo.ProductVersion"],
    { encoding: "utf8", windowsHide: true, env: { ...process.env, KIROSCRIBE_TARGET: exe } },
  );
  if (result.status !== 0) throw new Error(`${exe} のバージョンを読めません: ${result.stderr}`);
  return result.stdout.trim();
}

/** スタートメニューとデスクトップのショートカット（利用者ごとの場所） */
export function shortcuts() {
  const appData = process.env.APPDATA ?? path.join(os.homedir(), "AppData", "Roaming");
  return {
    startMenu: path.join(appData, "Microsoft", "Windows", "Start Menu", "Programs", `${PRODUCT_NAME}.lnk`),
    desktop: path.join(os.homedir(), "Desktop", `${PRODUCT_NAME}.lnk`),
  };
}

/** 動いているプロセスのうち、実行ファイル名が name のものの ID */
export function processIds(name) {
  const result = spawnSync("tasklist", ["/FI", `IMAGENAME eq ${name}`, "/FO", "CSV", "/NH"], {
    encoding: "utf8",
    windowsHide: true,
  });
  if (result.status !== 0) return [];
  return result.stdout
    .split(/\r?\n/)
    .map((line) => /^"([^"]+)","(\d+)"/.exec(line))
    .filter((match) => match && match[1].toLowerCase() === name.toLowerCase())
    .map((match) => Number(match[2]));
}

export function isAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

/** プロセスを止める（もう終わっていれば何もしない） */
export function kill(pid) {
  try {
    process.kill(pid, "SIGKILL");
  } catch {
    // 既に終了している
  }
}

/** 条件を満たすまで待つ（fn が真を返したら、その値を返す） */
export async function waitFor(fn, { timeout = 120_000, interval = 500, message }) {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await fn();
    if (value) return value;
    if (Date.now() >= deadline) throw new Error(`${timeout / 1000} 秒待っても、次のようになりません: ${message}`);
    await sleep(interval);
  }
}

/** プログラムを起動して、終了するまで待つ。終了コードを返す */
function runToEnd(file, args, { timeout }) {
  return new Promise((resolve, reject) => {
    const child = spawn(file, args, { stdio: "ignore", windowsHide: true });
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      reject(new Error(`${path.basename(file)} が ${timeout / 1000} 秒たっても終わりません`));
    }, timeout);
    child.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.once("exit", (code) => {
      clearTimeout(timer);
      resolve(code);
    });
  });
}

/**
 * インストーラを、画面を出さずに（/S）実行する。利用者がダブルクリックして入れるのと同じ処理が走る
 * （画面の操作だけを省く）。終わったら、インストールの情報を返す
 */
export async function install(setup) {
  const code = await runToEnd(setup, ["/S"], { timeout: 5 * 60_000 });
  if (code !== 0) throw new Error(`${path.basename(setup)} /S が終了コード ${code} で終わりました`);
  const info = installed();
  if (!info) throw new Error(`${path.basename(setup)} を実行しましたが、インストールの情報（${UNINSTALL_KEY}）がありません`);
  return info;
}

/**
 * アンインストールする（画面を出さずに）。インストールされていなければ何もしない。
 * NSIS のアンインストーラは、自分を一時フォルダに写して起動し直し、元のプロセスはすぐ終わるので、
 * インストールの情報が消えるまで待つ
 */
export async function uninstall() {
  const info = installed();
  if (!info) return;
  // 動いているアプリがあれば止める（アンインストーラは、アプリが動いていると止まってしまう）
  for (const pid of processIds(path.basename(info.exe ?? "kiroscribe.exe"))) kill(pid);
  if (info.uninstaller && fs.existsSync(info.uninstaller)) {
    const code = await runToEnd(info.uninstaller, ["/S"], { timeout: 5 * 60_000 });
    if (code !== 0) throw new Error(`アンインストーラが終了コード ${code} で終わりました`);
  }
  await waitFor(() => installed() === null, { timeout: 120_000, message: "インストールの情報が消える" });
  if (info.exe) await waitFor(() => !fs.existsSync(info.exe), { timeout: 60_000, message: `${info.exe} が消える` });
}
