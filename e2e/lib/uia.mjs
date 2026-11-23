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
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class KiroscribeWindow {
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
}
"@
# 画面に見えているか（隠した内部用のウィンドウ（多重起動の判定・イベントの受け口など）を除く）
# 見えていても、16 ピクセル四方以下のもの（Tao のイベントの受け口など、内部用のウィンドウ）は数えない
function Is-Shown($el) {
  $r = $el.Current.BoundingRectangle
  if ($r.Width -le 16 -and $r.Height -le 16) { return $false }
  $hwnd = $el.Current.NativeWindowHandle
  if ($hwnd -eq 0) { return -not $el.Current.IsOffscreen }
  return [KiroscribeWindow]::IsWindowVisible([IntPtr]$hwnd)
}
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
 * 1 件は { name: 題名, className, texts: [中の文字], buttons: [ボタンの名前], others: [そのほかの部品（種類:名前）] }。
 * 隠しているウィンドウ（閉じてトレイにいるメインウィンドウ、内部用のウィンドウなど）は含まない
 */
export function visibleWindows(pid) {
  const out = powershell(
    `
$result = @()
foreach ($w in $windows) {
  if (-not (Is-Shown $w)) { continue }
  $texts = @(); $buttons = @(); $others = @()
  foreach ($e in $w.FindAll($Scope::Descendants, [System.Windows.Automation.Condition]::TrueCondition)) {
    $type = $e.Current.ControlType.ProgrammaticName
    $invokable = [bool]$e.GetCurrentPropertyValue($AE::IsInvokePatternAvailableProperty)
    if ($type -eq 'ControlType.Button' -or ($invokable -and $e.Current.Name)) { $buttons += $e.Current.Name }
    elseif ($type -eq 'ControlType.Text') { $texts += $e.Current.Name }
    else { $others += ($type + ':' + $e.Current.Name) }
  }
  $result += [pscustomobject]@{ name = $w.Current.Name; className = $w.Current.ClassName; texts = $texts; buttons = $buttons; others = $others }
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
    $pattern = $null
    if ($button.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) {
      $pattern.Invoke()
    } else {
      # 押す操作を受け付けない部品（タスク ダイアログのボタンなど）は、本物のマウスでクリックする
      Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class KiroscribeClick {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
}
"@
      $r = $button.Current.BoundingRectangle
      [KiroscribeClick]::SetCursorPos([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2)) | Out-Null
      Start-Sleep -Milliseconds 150
      [KiroscribeClick]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
      Start-Sleep -Milliseconds 60
      [KiroscribeClick]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    }
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

// ---- タスクトレイ ----
// 通知領域のアイコンは、エクスプローラー（タスクバー）が持つ。Windows 11 では、新しいアプリのアイコンは
// 「隠れているインジケーター」（あふれた分の一覧）に入るので、タスクバーで見つからなければ、その一覧を開いて探す。
// クリックは、利用者と同じく本物のマウスの操作（カーソルを動かして押す）で行う。

const TRAY = `
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class KiroscribeMouse {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
  public static void Click(int x, int y, bool right) {
    SetCursorPos(x, y);
    System.Threading.Thread.Sleep(150);
    mouse_event(right ? 0x0008u : 0x0002u, 0, 0, 0, UIntPtr.Zero);
    System.Threading.Thread.Sleep(60);
    mouse_event(right ? 0x0010u : 0x0004u, 0, 0, 0, UIntPtr.Zero);
  }
}
"@
$True_ = [System.Windows.Automation.Condition]::TrueCondition
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class KiroscribeMenu {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern int GetMenuItemCount(IntPtr menu);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetMenuString(IntPtr menu, uint item, StringBuilder text, int max, uint flags);
  [DllImport("user32.dll")] public static extern uint GetMenuState(IntPtr menu, uint item, uint flags);
  [DllImport("user32.dll")] public static extern bool GetMenuItemRect(IntPtr hwnd, IntPtr menu, uint item, out RECT rect);
}
"@
# 見えているポップアップのメニュー（#32768）を、Windows の API で直接読む（UI オートメーションでは項目が見えないことがある）。
# 項目ごとに 名前・チェック・区切り線か・画面の位置 を返す。メニューが出ていなければ $null
function Read-OpenMenu {
  foreach ($w in $AE::RootElement.FindAll($Scope::Children, $True_)) {
    if ($w.Current.ClassName -ne '#32768' -or -not (Is-Shown $w)) { continue }
    $hwnd = [IntPtr]$w.Current.NativeWindowHandle
    $menu = [KiroscribeMenu]::SendMessage($hwnd, 0x01E1, [IntPtr]::Zero, [IntPtr]::Zero)  # MN_GETHMENU
    if ($menu -eq [IntPtr]::Zero) { continue }
    $count = [KiroscribeMenu]::GetMenuItemCount($menu)
    if ($count -le 0) { continue }
    $items = @()
    for ($i = 0; $i -lt $count; $i++) {
      $sb = New-Object System.Text.StringBuilder 512
      [KiroscribeMenu]::GetMenuString($menu, [uint32]$i, $sb, 512, 0x400) | Out-Null  # MF_BYPOSITION
      $state = [KiroscribeMenu]::GetMenuState($menu, [uint32]$i, 0x400)
      $rect = New-Object KiroscribeMenu+RECT
      [KiroscribeMenu]::GetMenuItemRect([IntPtr]::Zero, $menu, [uint32]$i, [ref]$rect) | Out-Null
      $items += [pscustomobject]@{
        name = ($sb.ToString() -replace '&', '')
        checked = (($state -band 0x8) -ne 0)
        separator = (($state -band 0x800) -ne 0)
        x = [int](($rect.Left + $rect.Right) / 2); y = [int](($rect.Top + $rect.Bottom) / 2)
      }
    }
    return ,$items
  }
  return $null
}
function Named($name) { New-Object System.Windows.Automation.PropertyCondition($AE::NameProperty, $name) }
function Taskbar { $AE::RootElement.FindFirst($Scope::Children, (New-Object System.Windows.Automation.PropertyCondition($AE::ClassNameProperty, 'Shell_TrayWnd'))) }
function Click-Element($el, $right) {
  $r = $el.Current.BoundingRectangle
  [KiroscribeMouse]::Click([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2), $right)
}
# 名前が $name の、画面に見えているアイコンを探す（タスクバー → あふれた分の一覧の順）。見つからなければ $null
function Find-TrayIcons($name) {
  $found = @()
  foreach ($w in $AE::RootElement.FindAll($Scope::Children, $True_)) {
    foreach ($e in $w.FindAll($Scope::Descendants, (Named $name))) {
      if (-not $e.Current.IsOffscreen) { $found += $e }
    }
  }
  return ,$found
}
function Open-Overflow {
  $bar = Taskbar
  if ($bar -eq $null) { return }
  foreach ($e in $bar.FindAll($Scope::Descendants, $True_)) {
    $id = $e.Current.AutomationId; $n = $e.Current.Name
    if ($id -eq 'SystemTrayIcon' -or $id -eq '1502' -or $n -match 'Hidden Icons|Notification Chevron|隠れているインジケーター') {
      Click-Element $e $false
      Start-Sleep -Milliseconds 800
      return
    }
  }
}
function Find-TrayIcon($name) {
  $icons = Find-TrayIcons $name
  if ($icons.Count -eq 0) { Open-Overflow; $icons = Find-TrayIcons $name }
  if ($icons.Count -eq 0) { return $null }
  return $icons[0]
}
# 見つからないときに、調べるための材料（タスクバーと、あふれた分の一覧にある名前）
function Describe-Tray {
  $names = @()
  foreach ($w in $AE::RootElement.FindAll($Scope::Children, $True_)) {
    $cls = $w.Current.ClassName
    if ($cls -match 'Shell_TrayWnd|NotifyIconOverflowWindow|TopLevelWindowForOverflowXamlIsland|Xaml') {
      foreach ($e in $w.FindAll($Scope::Descendants, $True_)) {
        if ($e.Current.Name) { $names += ($cls + ' / ' + $e.Current.ControlType.ProgrammaticName + ' / ' + $e.Current.AutomationId + ' / ' + $e.Current.Name) }
      }
    }
  }
  return $names
}
`;

function trayScript(body, env) {
  return powershell(TRAY + body, { KIROSCRIBE_UIA_PID: "0", ...env });
}

/** 通知領域に、名前（ツールチップ）が name のアイコンがいくつ見えているか。見えなければ、調べるための材料も返す */
export function trayIcons(name) {
  const out = trayScript(
    `
$icons = Find-TrayIcons $env:KIROSCRIBE_TRAY_NAME
if ($icons.Count -eq 0) { Open-Overflow; $icons = Find-TrayIcons $env:KIROSCRIBE_TRAY_NAME }
$tray = @(); if ($icons.Count -eq 0) { $tray = Describe-Tray }
ConvertTo-Json -InputObject ([pscustomobject]@{ count = $icons.Count; tray = @($tray) }) -Depth 3 -Compress
`,
    { KIROSCRIBE_TRAY_NAME: name },
  );
  return JSON.parse(out);
}

/** 通知領域のアイコンを、マウスでクリックする（button: "left" | "right"）。押せたら true */
export function clickTrayIcon(name, button = "left") {
  const out = trayScript(
    `
$icon = Find-TrayIcon $env:KIROSCRIBE_TRAY_NAME
if ($icon -eq $null) { 'missing'; exit 0 }
Click-Element $icon ($env:KIROSCRIBE_TRAY_BUTTON -eq 'right')
'clicked'
`,
    { KIROSCRIBE_TRAY_NAME: name, KIROSCRIBE_TRAY_BUTTON: button },
  );
  return out === "clicked";
}

/**
 * プロセスが出しているメニュー（右クリックのメニュー）の項目。1 件は { name, checked }（checked は分からなければ null）。
 * メニューが出ていなければ null
 */
export function openMenu(pid) {
  const out = trayScript(
    `
$items = Read-OpenMenu
if ($null -eq $items) { 'null'; exit 0 }
ConvertTo-Json -Compress -InputObject @($items | Where-Object { -not $_.separator } | ForEach-Object { [pscustomobject]@{ name = $_.name; checked = $_.checked } })
`,
    { KIROSCRIBE_UIA_PID: String(pid) },
  );
  return out === "null" ? null : JSON.parse(out);
}

/** 出ているメニューの項目 name を選ぶ（利用者と同じく、マウスでクリックする）。選べたら true */
export function chooseMenuItem(name) {
  const out = trayScript(
    `
$items = Read-OpenMenu
if ($null -eq $items) { 'missing'; exit 0 }
$item = $items | Where-Object { $_.name -eq $env:KIROSCRIBE_MENU_ITEM } | Select-Object -First 1
if ($null -eq $item) { 'missing'; exit 0 }
[KiroscribeMouse]::Click($item.x, $item.y, $false)
'chosen'
`,
    { KIROSCRIBE_MENU_ITEM: name },
  );
  return out === "chosen";
}

/** プロセスのウィンドウ（題名 title）を最小化する */
export function minimizeWindow(pid, title) {
  const out = powershell(
    `
foreach ($w in $windows) {
  if ($w.Current.Name -ne $env:KIROSCRIBE_UIA_TITLE) { continue }
  $w.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).SetWindowVisualState([System.Windows.Automation.WindowVisualState]::Minimized)
  'minimized'; exit 0
}
'missing'
`,
    { KIROSCRIBE_UIA_PID: String(pid), KIROSCRIBE_UIA_TITLE: title },
  );
  return out === "minimized";
}

/**
 * プロセスのウィンドウのうち、幅が maxWidth 以下のもの（ポップアップなど）を、本物のマウスで
 * 左上から (grabX, grabY) の所をつかんで (dx, dy) だけドラッグする。動かす前と後の位置を返す: { before, after }（[左, 上, 幅, 高さ]）
 */
export function dragWindow(pid, { maxWidth = 500, grabX = 60, grabY = 16, dx = -200, dy = -150 } = {}) {
  const out = powershell(
    `
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class KiroscribeDrag {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
}
"@
function Rect($el) { $r = $el.Current.BoundingRectangle; @([int]$r.X, [int]$r.Y, [int]$r.Width, [int]$r.Height) }
$target = $null
foreach ($w in $windows) {
  if ((Is-Shown $w) -and $w.Current.BoundingRectangle.Width -le [int]$env:KIROSCRIBE_MAX_WIDTH) { $target = $w; break }
}
if ($target -eq $null) { 'null'; exit 0 }
$before = Rect $target
$x = $before[0] + [int]$env:KIROSCRIBE_GRAB_X; $y = $before[1] + [int]$env:KIROSCRIBE_GRAB_Y
$dx = [int]$env:KIROSCRIBE_DX; $dy = [int]$env:KIROSCRIBE_DY
[KiroscribeDrag]::SetCursorPos($x, $y) | Out-Null
Start-Sleep -Milliseconds 200
[KiroscribeDrag]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 200
for ($i = 1; $i -le 20; $i++) {
  [KiroscribeDrag]::SetCursorPos($x + [int]($dx * $i / 20), $y + [int]($dy * $i / 20)) | Out-Null
  Start-Sleep -Milliseconds 30
}
Start-Sleep -Milliseconds 200
[KiroscribeDrag]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 500
ConvertTo-Json -Compress -InputObject ([pscustomobject]@{ before = $before; after = (Rect $target) })
`,
    {
      KIROSCRIBE_UIA_PID: String(pid),
      KIROSCRIBE_MAX_WIDTH: String(maxWidth),
      KIROSCRIBE_GRAB_X: String(grabX),
      KIROSCRIBE_GRAB_Y: String(grabY),
      KIROSCRIBE_DX: String(dx),
      KIROSCRIBE_DY: String(dy),
    },
  );
  return out === "null" ? null : JSON.parse(out);
}

/** 調べるための材料: 画面のいちばん上のウィンドウの一覧（種類・名前・プロセス・見えているか・位置） */
export function describeTopWindows() {
  return powershell(
    `
$lines = @()
foreach ($w in $AE::RootElement.FindAll($Scope::Children, [System.Windows.Automation.Condition]::TrueCondition)) {
  $r = $w.Current.BoundingRectangle
  $lines += ('{0} | {1} | pid {2} | 見える {3} | {4},{5} {6}x{7}' -f $w.Current.ClassName, $w.Current.Name, $w.Current.ProcessId, (Is-Shown $w), [int]$r.X, [int]$r.Y, [int]$r.Width, [int]$r.Height)
}
$lines -join "\`n"
`,
    { KIROSCRIBE_UIA_PID: "0" },
  );
}
