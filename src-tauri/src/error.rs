use serde::Serialize;

/// フロントエンドへ返すエラー。メッセージ文字列としてシリアライズされ、`invoke` の reject 値になる。
#[derive(Debug)]
pub struct CommandError(pub String);

impl Serialize for CommandError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl From<kiroscribe_core::CoreError> for CommandError {
    fn from(err: kiroscribe_core::CoreError) -> Self {
        CommandError(err.to_string())
    }
}

impl From<String> for CommandError {
    fn from(message: String) -> Self {
        CommandError(message)
    }
}

pub type CmdResult<T> = Result<T, CommandError>;
