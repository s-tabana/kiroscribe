// アプリを起動できなかったときに、原因を調べるための情報を集める。
// CI ではスクリーンショットやログのファイルを開けないことがあるので、ジョブのログ（標準エラー）だけで追えるようにする。

import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { appBinary, appEnvironment, freePort, tauriDriverBinary } from "./app.mjs";
import { request, waitForServer } from "./webdriver.mjs";

const IS_WINDOWS = process.platform === "win32";

/** コマンドを実行して、出力（標準出力と標準エラー）を返す。失敗しても例外にしない */
function run(command, args, { timeout = 20_000 } = {}) {
  try {
    const result = spawnSync(command, args, { encoding: "utf8", timeout, windowsHide: true });
    if (result.error) return `（実行できません: ${result.error.message}）`;
    return `${result.stdout ?? ""}${result.stderr ?? ""}`.trim() || `（出力なし。終了コード ${result.status}）`;
  } catch (err) {
    return `（実行できません: ${err.message}）`;
  }
}

/** ネイティブのドライバー（Windows: msedgedriver、Linux: WebKitWebDriver）の場所 */
export function nativeDriver() {
  return process.env.KIROSCRIBE_E2E_NATIVE_DRIVER ?? (IS_WINDOWS ? "msedgedriver.exe" : "WebKitWebDriver");
}

/** WebView2 ランタイムのバージョン（Windows のレジストリから） */
function webview2Version() {
  const client = "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
  const keys = [
    `HKLM\\SOFTWARE\\WOW6432Node\\Microsoft\\EdgeUpdate\\Clients\\${client}`,
    `HKLM\\SOFTWARE\\Microsoft\\EdgeUpdate\\Clients\\${client}`,
    `HKCU\\SOFTWARE\\Microsoft\\EdgeUpdate\\Clients\\${client}`,
  ];
  const found = [];
  for (const key of keys) {
    const version = /\bpv\s+REG_SZ\s+(\S+)/.exec(run("reg", ["query", key, "/v", "pv"]))?.[1];
    if (version) found.push(`${version}（${key.slice(0, 4)}）`);
  }
  return found.length > 0 ? found.join("、") : "（レジストリに見つかりません）";
}

/** 使っている道具のバージョン（1 行ずつ） */
export function describeTools() {
  const lines = [
    `OS: ${process.platform} ${process.arch}、Node ${process.version}`,
    `アプリ: ${appBinary()}（${fs.existsSync(appBinary()) ? `${fs.statSync(appBinary()).size} バイト` : "ありません"}）`,
    `tauri-driver: ${tauriDriverBinary()}`,
    `ネイティブのドライバー: ${nativeDriver()}${IS_WINDOWS ? ` → ${run(nativeDriver(), ["--version"]).split("\n")[0]}` : ""}`,
  ];
  if (IS_WINDOWS) lines.push(`WebView2 ランタイム: ${webview2Version()}`);
  return lines.join("\n");
}

/** いま動いている、アプリ・ウェブビュー・ドライバーのプロセスと、その起動引数 */
export function processSnapshot() {
  if (IS_WINDOWS) {
    const script =
      "Get-CimInstance Win32_Process | Where-Object { $_.Name -match 'kiroscribe|msedgewebview2|msedgedriver|tauri-driver' } | " +
      "ForEach-Object { '{0} [親 {1}] {2} :: {3}' -f $_.ProcessId, $_.ParentProcessId, $_.Name, $_.CommandLine }";
    return run("powershell", ["-NoProfile", "-NonInteractive", "-Command", script], { timeout: 30_000 });
  }
  const all = run("ps", ["-eo", "pid,ppid,args"]);
  return all
    .split("\n")
    .filter((line) => /kiroscribe|WebKit|tauri-driver/.test(line) && !line.includes("ps -eo"))
    .join("\n");
}

/** ファイルの中身（長ければ、先頭の head 行と末尾の tail 行だけ） */
export function fileExcerpt(file, { head = 0, tail = 300 } = {}) {
  try {
    const all = fs.readFileSync(file, "utf8").trimEnd().split("\n");
    if (all.length <= head + tail) return all.join("\n");
    const skipped = all.length - head - tail;
    return [...all.slice(0, head), `（途中の ${skipped} 行は省略）`, ...all.slice(-tail)].join("\n");
  } catch (err) {
    return `（読めません: ${err.message}）`;
  }
}

/**
 * データフォルダの中の、WebView2 のデバッグ用のポートを書いたファイル（DevToolsActivePort）と、その中身。
 * msedgedriver は、アプリのウェブビューのデバッグ用のポートにつないで操作する。
 */
export function devToolsPorts(workspace) {
  const found = [];
  const folders = [];
  const walk = (dir, depth) => {
    let entries;
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        if (entry.name === "EBWebView") folders.push(full);
        if (depth < 4) walk(full, depth + 1);
      } else if (entry.name === "DevToolsActivePort") {
        let content;
        try {
          content = fs.readFileSync(full, "utf8").trim().replaceAll("\n", " / ");
        } catch (err) {
          content = `（読めません: ${err.message}）`;
        }
        found.push(`${full}: ${content}`);
      }
    }
  };
  walk(workspace.dir, 0);
  return [
    `ウェブビューのデータフォルダ（EBWebView）: ${folders.length > 0 ? folders.join("、") : "（作業フォルダの中にはありません）"}`,
    `DevToolsActivePort: ${found.length > 0 ? found.join("、") : "（ありません）"}`,
  ].join("\n");
}

/**
 * Windows: tauri-driver を通さずに、msedgedriver を詳しいログ付きで起動して、同じ内容でセッションを作ってみる。
 * msedgedriver がどこで止まっているか（アプリの起動、デバッグ用のポートへの接続、ページの発見）が、ログから分かる。
 * 返すのは、試した結果と、msedgedriver のログ。
 */
export async function probeNativeDriver(workspace) {
  if (!IS_WINDOWS) return "（Windows 以外では行いません）";
  const port = await freePort();
  const logFile = workspace.file("msedgedriver-verbose.log");
  const driver = spawn(nativeDriver(), [`--port=${port}`, "--verbose", `--log-path=${logFile}`], {
    env: { ...appEnvironment(workspace), TAURI_WEBVIEW_AUTOMATION: "true" },
    stdio: "ignore",
    windowsHide: true,
  });
  const lines = [];
  try {
    const baseUrl = `http://127.0.0.1:${port}`;
    await waitForServer(baseUrl);
    const capabilities = {
      alwaysMatch: {
        browserName: "webview2",
        "ms:edgeChromium": true,
        "ms:edgeOptions": { binary: appBinary(), args: [] },
      },
    };
    lines.push(`送った内容: ${JSON.stringify({ capabilities })}`);
    const started = Date.now();
    const snapshot = setTimeout(
      () => lines.push(`20 秒後のプロセス:\n${processSnapshot()}\n20 秒後のファイル:\n${devToolsPorts(workspace)}`),
      20_000,
    );
    try {
      const value = await request(baseUrl, "POST", "/session", { capabilities }, 150_000);
      lines.push(`結果: セッションを作れました（${Date.now() - started} ミリ秒）。${JSON.stringify(value.capabilities ?? {}).slice(0, 600)}`);
      await request(baseUrl, "DELETE", `/session/${value.sessionId}`, undefined, 20_000).catch(() => {});
    } catch (err) {
      lines.push(`結果: セッションを作れませんでした（${Date.now() - started} ミリ秒）: ${err.message}`);
    } finally {
      clearTimeout(snapshot);
    }
  } catch (err) {
    lines.push(`msedgedriver を起動できませんでした: ${err.message}`);
  } finally {
    driver.kill();
  }
  lines.push(`--- ${path.basename(logFile)} ---\n${fileExcerpt(logFile, { head: 200, tail: 300 })}`);
  return lines.join("\n");
}
