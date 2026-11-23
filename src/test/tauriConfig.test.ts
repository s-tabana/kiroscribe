import { describe, expect, it } from "vitest";
import tauriConfig from "../../src-tauri/tauri.conf.json";

// アプリの設定ファイルのうち、利用者への約束にかかわる部分が変わっていないことを確かめる。
// 意図して変えるときは、仕様一覧の項目と、このテストの期待値を一緒に直す。

const capabilities = import.meta.glob<{ windows: string[]; permissions: unknown[] }>(
  "../../src-tauri/capabilities/*.json",
  { eager: true, import: "default" },
);

describe("画面（WebView）に許すこと", () => {
  it("[SEC-006] Content Security Policy は、アプリ自身と本体との通信だけを許す", () => {
    const csp = tauriConfig.app.security.csp;
    expect(csp).toBe(
      "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; connect-src ipc: http://ipc.localhost",
    );

    const directives = new Map(
      csp.split(";").map((directive) => {
        const [name, ...sources] = directive.trim().split(/\s+/);
        return [name, sources] as const;
      }),
    );
    // 外部の接続先・スクリプト・画像を許す指定がない
    const sources = [...directives.values()].flat();
    expect(sources.filter((source) => /^(https?:|wss?:|\*)/.test(source))).toEqual(["http://ipc.localhost"]);
    expect(sources).not.toContain("'unsafe-eval'");
    expect(directives.get("default-src")).toEqual(["'self'"]);
    expect(directives.has("script-src")).toBe(false); // default-src（'self'）に従う
    expect(directives.get("connect-src")).toEqual(["ipc:", "http://ipc.localhost"]);
    expect(directives.get("img-src")).toEqual(["'self'", "data:"]);
    // CSP を外したり、画面に Tauri の API をまるごと渡したりする設定がない
    const security = tauriConfig.app.security as Record<string, unknown>;
    expect(Object.keys(security)).toEqual(["csp"]);
    expect((tauriConfig.app as Record<string, unknown>).withGlobalTauri).toBeUndefined();
  });

  it("[SEC-007] 画面に渡す権限は、ウィンドウの基本操作・ドラッグ・保存先と確認のダイアログ・クリップボードへの書き込みだけ", () => {
    const files = Object.keys(capabilities);
    expect(files).toEqual(["../../src-tauri/capabilities/default.json"]);
    const capability = capabilities[files[0]];

    expect(capability.windows).toEqual(["main", "meeting-popup"]);
    expect([...capability.permissions].sort()).toEqual([
      "clipboard-manager:allow-write-text",
      "core:default",
      "core:window:allow-start-dragging",
      "dialog:allow-ask",
      "dialog:allow-message",
      "dialog:allow-save",
    ]);
    // ファイルの読み書き・プログラムの実行・更新の実行・外部の URL を開く権限は渡さない（本体のコマンドを通す）
    for (const permission of capability.permissions) {
      expect(typeof permission).toBe("string");
      expect(permission).not.toMatch(/^(fs|shell|opener|updater|http|process|autostart):/);
    }
  });
});

describe("アプリ内の更新の設定", () => {
  it("[REL-007][UPD-003] 更新用のファイルを作り、決まった取得元・1 行の公開鍵・操作の要らないインストールの方式を使う", () => {
    expect(tauriConfig.bundle.createUpdaterArtifacts).toBe(true);
    const updater = tauriConfig.plugins.updater;
    expect(updater.endpoints).toEqual([
      "https://github.com/s-tabana/kiroscribe/releases/latest/download/latest.json",
    ]);
    expect(updater.windows.installMode).toBe("passive");
    expect(updater.pubkey).toMatch(/^[A-Za-z0-9+/=]{100,}$/);
    // 公開鍵は、minisign の公開鍵を base64 にしたもの
    expect(atob(updater.pubkey)).toMatch(/^untrusted comment: minisign public key: [0-9A-F]+\n\S+\n$/);
  });
});
