import { useCallback, useEffect, useState } from "react";
import { errorMessage, type Backend } from "../lib/backend";
import { NO_UPDATE, type UpdateStatus } from "../lib/types";

export interface UpdaterMessage {
  kind: "ok" | "error";
  text: string;
}

export interface Updater {
  status: UpdateStatus;
  /** 手動の確認中 */
  checking: boolean;
  /** ダウンロードの進み具合（0〜100）。大きさが分からなければ null */
  percent: number | null;
  /** 直前の操作の結果（最新だった・確認できなかった・更新できなかった） */
  message: UpdaterMessage | null;
  check(): Promise<void>;
  install(): Promise<void>;
}

export function downloadPercent(status: UpdateStatus): number | null {
  if (!status.totalBytes) return null;
  return Math.min(100, Math.floor((status.downloadedBytes * 100) / status.totalBytes));
}

/**
 * アプリ内更新の状態と操作。状態は Rust 側が持ち、ここはイベントで追いかける。
 * ウィンドウを閉じてトレイにいる間に見つかった更新も分かるよう、ウィンドウが前面に戻ったら読み直す。
 */
export function useUpdater(backend: Backend): Updater {
  const [status, setStatus] = useState<UpdateStatus>(NO_UPDATE);
  const [checking, setChecking] = useState(false);
  const [message, setMessage] = useState<UpdaterMessage | null>(null);

  useEffect(() => {
    let active = true;
    const refresh = () =>
      void backend
        .getUpdateStatus()
        .then((next) => active && setStatus(next))
        .catch(() => undefined);
    const unsubscribe = backend.onUpdateStatus((next) => active && setStatus(next));
    refresh();
    window.addEventListener("focus", refresh);
    return () => {
      active = false;
      unsubscribe();
      window.removeEventListener("focus", refresh);
    };
  }, [backend]);

  const check = useCallback(async () => {
    setChecking(true);
    setMessage(null);
    try {
      const next = await backend.checkForUpdate();
      setStatus(next);
      if (!next.available) setMessage({ kind: "ok", text: "最新のバージョンです" });
    } catch (err) {
      setMessage({ kind: "error", text: errorMessage(err) });
    } finally {
      setChecking(false);
    }
  }, [backend]);

  const { available, ready } = status;
  const install = useCallback(async () => {
    if (!available) return;
    setMessage(null);
    try {
      // 更新するとアプリが終了するので、録音中は始めない（Rust 側でも確かめている）
      const recording = await backend.getRecordingStatus();
      if (recording.state !== "idle") {
        setMessage({ kind: "error", text: "録音を停止してから更新してください" });
        return;
      }
      const ok = await backend.confirm(
        ready
          ? `Kiroscribe ${available.version} に更新します。\nアプリを終了し、新しいバージョンで起動し直します。`
          : `Kiroscribe ${available.version} に更新します。\nダウンロードが終わるとアプリを終了し、新しいバージョンで起動し直します。`,
        "アプリの更新",
        "info",
      );
      if (!ok) return;
      await backend.installUpdate();
    } catch (err) {
      setMessage({ kind: "error", text: errorMessage(err) });
    }
  }, [backend, available, ready]);

  return { status, checking, percent: downloadPercent(status), message, check, install };
}

/**
 * 自動で更新したあとの知らせ（更新後のバージョン）と、それを消す関数。
 * 自動の更新ではウィンドウを出さずに起動し直すので、次にウィンドウを開いたときに 1 回だけ受け取る。
 */
export function useUpdateNotice(backend: Backend): [string | null, () => void] {
  const [version, setVersion] = useState<string | null>(null);

  useEffect(() => {
    // 受け取れるのは 1 回だけなので、受け取ったものは必ず表示に回す
    const take = () =>
      void backend
        .takeUpdateNotice()
        .then((next) => next && setVersion(next))
        .catch(() => undefined);
    take();
    return backend.onMainShown(take);
  }, [backend]);

  const dismiss = useCallback(() => setVersion(null), []);
  return [version, dismiss];
}
