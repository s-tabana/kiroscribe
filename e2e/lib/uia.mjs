// Windows の UI オートメーション（画面の部品を外から読み、操作する OS の仕組み）で、アプリが出した OS の画面を確かめる。
// WebDriver が届かない画面（アプリが起動しきる前に出すダイアログなど）を、利用者が見るとおりに読む・押すために使う。
//
// Windows PowerShell（5.1。.NET Framework の UI オートメーションのライブラリが使える）から呼ぶ。Windows でだけ使える。

import { spawnSync } from "node:child_process";

const PRELUDE = `
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$AE = [System.Windows.Automation.AutomationElement]
$Scope = [System.Windows.Automation.TreeScope]
$targetPid = [int]$env:KIROSCRIBE_UIA_PID
$cond = New-Object System.Windows.Automation.PropertyCondition($AE::ProcessIdProperty, $targetPid)
$windows = $AE::RootElement.FindAll($Scope::Children, $cond)
`;

function powershell(script, env) {
  const result = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", PRELUDE + script], {
    encoding: "utf8",
    windowsHide: true,
    timeout: 60_000,
    env: { ...process.env, ...env },
  });
  if (result.status !== 0) {
    throw new Error(`UI オートメーションに失敗しました（終了コード ${result.status}）: ${result.stderr || result.stdout}`);
  }
  return result.stdout.trim();
}

/**
 * プロセスが出している、画面に見えるウィンドウ（トップレベル）の一覧。
 * 1 件は { name: 題名, className, texts: [中の文字], buttons: [ボタンの名前] }。
 * 隠しているウィンドウ（閉じてトレイにいるメインウィンドウなど）は含まない
 */
export function visibleWindows(pid) {
  const out = powershell(
    `
$result = @()
foreach ($w in $windows) {
  $texts = @(); $buttons = @()
  foreach ($e in $w.FindAll($Scope::Descendants, [System.Windows.Automation.Condition]::TrueCondition)) {
    $type = $e.Current.ControlType.ProgrammaticName
    if ($type -eq 'ControlType.Button') { $buttons += $e.Current.Name }
    elseif ($type -eq 'ControlType.Text') { $texts += $e.Current.Name }
  }
  if ($w.Current.IsOffscreen) { continue }
  $result += [pscustomobject]@{ name = $w.Current.Name; className = $w.Current.ClassName; texts = $texts; buttons = $buttons }
}
ConvertTo-Json -InputObject @($result) -Depth 4 -Compress
`,
    { KIROSCRIBE_UIA_PID: String(pid) },
  );
  return out ? JSON.parse(out) : [];
}

/** プロセスのウィンドウの中で、名前が name のボタンを押す。押せたら true */
export function pressButton(pid, name) {
  const out = powershell(
    `
foreach ($w in $windows) {
  $byName = New-Object System.Windows.Automation.PropertyCondition($AE::NameProperty, $env:KIROSCRIBE_UIA_BUTTON)
  $button = $w.FindFirst($Scope::Descendants, $byName)
  if ($button -ne $null) {
    $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    'pressed'
    exit 0
  }
}
'missing'
`,
    { KIROSCRIBE_UIA_PID: String(pid), KIROSCRIBE_UIA_BUTTON: name },
  );
  return out === "pressed";
}
