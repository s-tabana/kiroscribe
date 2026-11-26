// 本物の音声デバイスを使うテストの道具（Windows だけ）。
//
// CI では、仮想のオーディオケーブル（VB-CABLE。.github/workflows/e2e.yml で入れる）を使う。
// 「CABLE Input」（出力デバイス）に流した音が、「CABLE Output」（入力デバイス＝マイク）から聞こえる。
// どちらも既定のデバイスになるので、既定の出力に音を流すと、アプリには
// マイク（自分）と PC から流れる音声（相手）の両方として届く。

import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";

/** 発話に似た音（音節ごとに大きさの変わる声の高さの音と、文の間の無音）の WAV（16 ビット・モノラル） */
export function speechLikeWav({ seconds = 12, rate = 48_000 } = {}) {
  const count = Math.round(seconds * rate);
  const data = Buffer.alloc(count * 2);
  for (let i = 0; i < count; i++) {
    const t = i / rate;
    // 0.6 秒の無音のあと、1.8 秒の発話と 1.0 秒の無音をくり返す
    const inCycle = (t - 0.6) % 2.8;
    if (t < 0.6 || inCycle >= 1.8) continue;
    const syllables = 0.55 + 0.45 * Math.sin(2 * Math.PI * 4 * t);
    const voice =
      (Math.sin(2 * Math.PI * 170 * t) + 0.5 * Math.sin(2 * Math.PI * 340 * t) + 0.3 * Math.sin(2 * Math.PI * 680 * t)) / 1.8;
    data.writeInt16LE(Math.round(voice * syllables * 0.45 * 32767), i * 2);
  }
  const header = Buffer.alloc(44);
  header.write("RIFF", 0, "latin1");
  header.writeUInt32LE(36 + data.length, 4);
  header.write("WAVE", 8, "latin1");
  header.write("fmt ", 12, "latin1");
  header.writeUInt32LE(16, 16);
  header.writeUInt16LE(1, 20); // PCM
  header.writeUInt16LE(1, 22); // モノラル
  header.writeUInt32LE(rate, 24);
  header.writeUInt32LE(rate * 2, 28);
  header.writeUInt16LE(2, 32);
  header.writeUInt16LE(16, 34);
  header.write("data", 36, "latin1");
  header.writeUInt32LE(data.length, 40);
  return Buffer.concat([header, data]);
}

/** 16 ビットの PCM の WAV の、音の大きさ（RMS。0〜1） */
export function wavRms(wav) {
  if (wav.length < 44 || wav.readUInt16LE(34) !== 16) return 0;
  let sum = 0;
  let n = 0;
  for (let offset = 44; offset + 1 < wav.length; offset += 2) {
    const v = wav.readInt16LE(offset) / 32768;
    sum += v * v;
    n += 1;
  }
  return n === 0 ? 0 : Math.sqrt(sum / n);
}

function powershell(script, env = {}) {
  const result = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script], {
    encoding: "utf8",
    windowsHide: true,
    timeout: 120_000,
    env: { ...process.env, ...env },
  });
  if (result.status !== 0) throw new Error(`PowerShell が失敗しました（終了コード ${result.status}）: ${result.stderr || result.stdout}`);
  return result.stdout.trim();
}

/** WAV のファイルを、既定の出力デバイスで鳴らす（鳴らし終わると解決する Promise） */
export function playWav(file) {
  return new Promise((resolve, reject) => {
    const child = spawn(
      "powershell.exe",
      ["-NoProfile", "-NonInteractive", "-Command", "(New-Object System.Media.SoundPlayer $env:KIROSCRIBE_WAV).PlaySync()"],
      { env: { ...process.env, KIROSCRIBE_WAV: file }, stdio: ["ignore", "ignore", "pipe"], windowsHide: true },
    );
    let stderr = "";
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.once("error", reject);
    child.once("exit", (code) => (code === 0 ? resolve() : reject(new Error(`音を鳴らせません（終了コード ${code}）: ${stderr}`))));
  });
}

/** 発話に似た音の WAV を作って鳴らす */
export function playSpeech(dir, seconds = 12) {
  const file = `${dir}\\speech-${seconds}s.wav`;
  if (!fs.existsSync(file)) fs.writeFileSync(file, speechLikeWav({ seconds }));
  return playWav(file);
}

/** 音声の入出力デバイス（Windows のデバイスマネージャーの「オーディオの入力および出力」）の一覧。調べるための材料 */
export function audioEndpoints() {
  const out = powershell(
    "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; " +
      "ConvertTo-Json -Compress -InputObject @(Get-PnpDevice -Class AudioEndpoint -ErrorAction SilentlyContinue | " +
      "ForEach-Object { [pscustomobject]@{ name = $_.FriendlyName; status = $_.Status; id = $_.InstanceId } })",
  );
  return out ? JSON.parse(out) : [];
}

/**
 * デバイスを有効・無効にする（利用者がデバイスを抜く・挿すのと同じく、OS からデバイスがなくなる・戻る）。
 * namePattern は、デバイスマネージャーでの名前のワイルドカード（例: "*VB-Audio Virtual Cable*"）。
 * className は "MEDIA"（デバイスそのもの）か "AudioEndpoint"（入力・出力の片方）
 */
export function setDeviceEnabled(namePattern, enabled, className = "MEDIA") {
  const out = powershell(
    `
$ErrorActionPreference = 'Stop'
$devices = @(Get-PnpDevice -Class $env:KIROSCRIBE_CLASS | Where-Object { $_.FriendlyName -like $env:KIROSCRIBE_NAME })
if ($devices.Count -eq 0) { 'missing'; exit 0 }
foreach ($d in $devices) {
  if ($env:KIROSCRIBE_ENABLE -eq '1') { Enable-PnpDevice -InstanceId $d.InstanceId -Confirm:$false }
  else { Disable-PnpDevice -InstanceId $d.InstanceId -Confirm:$false }
}
'done'
`,
    { KIROSCRIBE_CLASS: className, KIROSCRIBE_NAME: namePattern, KIROSCRIBE_ENABLE: enabled ? "1" : "0" },
  );
  if (out !== "done") throw new Error(`デバイスが見つかりません: ${className} ${namePattern}`);
}

/** 仮想のオーディオケーブル（デバイスそのもの）の名前 */
export const CABLE = "*VB-Audio Virtual Cable*";

/** 出力デバイス（PC の音声）としての仮想のケーブルの名前 */
export const CABLE_SPEAKERS = "Speakers (VB-Audio*";

/**
 * Windows のプライバシー設定の「デスクトップ アプリがマイクにアクセスできるようにする」を切り替える（"Allow" か "Deny"）。
 * 利用者が設定画面で切り替えるのと同じ値を書く
 */
export function setDesktopMicAccess(value) {
  powershell(
    `
$p = 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\microphone\\NonPackaged'
if (-not (Test-Path $p)) { New-Item -Path $p -Force | Out-Null }
Set-ItemProperty -Path $p -Name Value -Value $env:KIROSCRIBE_CONSENT
`,
    { KIROSCRIBE_CONSENT: value },
  );
}

/**
 * Windows の音声サービス（Audiosrv）を止める・動かす。止めると、開いている音声デバイスはすべて使えなくなる
 * （録音中にデバイスで問題が起きたときと同じく、アプリにエラーが届く）。管理者の権限が要る（CI のランナーにはある）
 */
export function setAudioServiceRunning(running) {
  powershell(running ? "Start-Service -Name Audiosrv" : "Stop-Service -Name Audiosrv -Force");
}
