//! Windows の起動時の自動起動（利用者ごとの「スタートアップ アプリ」）の登録。
//!
//! Kiroscribe は利用者ごとにインストールするので、自動起動も利用者ごとの場所
//! （`HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`）にだけ登録する。
//! 管理者として動いていても、全ユーザー共通の場所（`HKEY_LOCAL_MACHINE`）には書かない
//! （書くと、ほかの利用者のサインインでも起動しようとし、アンインストールしても残る）。
//! 実行ファイルのパスは引用符で囲む（利用者のフォルダの名前に空白があっても、取り違えないように）。
//! タスクマネージャーの「スタートアップ アプリ」で無効にした設定（StartupApproved）には触れない。

use std::path::Path;

/// 登録する値（コマンドライン）。実行ファイルのパスを引用符で囲み、引数を続ける。
pub fn command_line(exe: &Path, args: &[&str]) -> String {
    let mut command = format!("\"{}\"", exe.display());
    for arg in args {
        command.push(' ');
        command.push_str(arg);
    }
    command
}

#[cfg(windows)]
mod registry {
    use std::path::Path;

    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE};
    use winreg::RegKey;

    use super::command_line;
    use crate::error::{CoreError, Result};

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    fn error(context: &str, err: std::io::Error) -> CoreError {
        CoreError::Other(format!("{context}: {err}"))
    }

    /// 利用者ごとの自動起動に、`name` の名前で登録する（`enabled` が false なら消す）。
    pub fn set_enabled(name: &str, exe: &Path, args: &[&str], enabled: bool) -> Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if enabled {
            let (run, _) = hkcu
                .create_subkey(RUN_KEY)
                .map_err(|e| error("自動起動の登録先を開けません", e))?;
            run.set_value(name, &command_line(exe, args))
                .map_err(|e| error("自動起動に登録できません", e))?;
        } else if let Ok(run) = hkcu.open_subkey_with_flags(RUN_KEY, KEY_READ | KEY_WRITE) {
            match run.delete_value(name) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(error("自動起動の登録を消せません", e)),
            }
        }
        remove_machine_wide(name, exe);
        Ok(())
    }

    /// 利用者ごとの自動起動に登録されているコマンド（なければ `None`）。
    pub fn registered(name: &str) -> Option<String> {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(RUN_KEY)
            .ok()?
            .get_value::<String, _>(name)
            .ok()
    }

    /// 以前の版が、管理者として動いたときに全ユーザー共通の場所に書いた登録を消す。
    /// このアプリの実行ファイルを指すものだけを消す。権限がなければ（ふつうの起動では）何もしない。
    fn remove_machine_wide(name: &str, exe: &Path) {
        let Ok(run) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(RUN_KEY, KEY_READ | KEY_WRITE) else {
            return;
        };
        let Ok(value) = run.get_value::<String, _>(name) else { return };
        let target = exe.to_string_lossy().to_lowercase();
        if value.to_lowercase().contains(&target) {
            let _ = run.delete_value(name);
        }
    }
}

#[cfg(windows)]
pub use registry::{registered, set_enabled};

#[cfg(test)]
mod tests {
    use super::*;

    /// 実行ファイルのパスは引用符で囲む（空白を含む利用者のフォルダでも、取り違えない）。
    // [TRY-009]
    #[test]
    fn the_command_quotes_the_executable() {
        let exe = Path::new(r"C:\Users\Taro Yamada\AppData\Local\Kiroscribe\kiroscribe.exe");
        assert_eq!(
            command_line(exe, &["--autostart"]),
            r#""C:\Users\Taro Yamada\AppData\Local\Kiroscribe\kiroscribe.exe" --autostart"#
        );
        assert_eq!(command_line(Path::new("/opt/k"), &[]), r#""/opt/k""#);
    }

    /// 本物のレジストリ（利用者ごとの場所）に登録・解除する。テスト用の名前を使う。
    // [TRY-009]
    #[cfg(windows)]
    #[test]
    fn registers_and_unregisters_for_the_current_user() {
        let name = format!("Kiroscribe-test-{}", uuid::Uuid::new_v4());
        let exe = Path::new(r"C:\Users\Taro Yamada\AppData\Local\Kiroscribe\kiroscribe.exe");
        assert_eq!(registered(&name), None);
        set_enabled(&name, exe, &["--autostart"], true).unwrap();
        let checked = std::panic::catch_unwind(|| {
            assert_eq!(registered(&name), Some(command_line(exe, &["--autostart"])));
        });
        set_enabled(&name, exe, &["--autostart"], false).unwrap();
        if let Err(panic) = checked {
            std::panic::resume_unwind(panic);
        }
        assert_eq!(registered(&name), None);
        // 登録していないものを消しても、エラーにしない
        set_enabled(&name, exe, &["--autostart"], false).unwrap();
    }
}
