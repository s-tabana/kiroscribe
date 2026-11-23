//! API キーの保存。
//!
//! Windows では「Windows 資格情報マネージャー」に保存し、平文ファイルには残さない。
//! Windows 以外（開発用の Linux など）は対象外プラットフォームのため、アプリデータフォルダの
//! ファイル（Unix では 0600）に保存する簡易実装で代替する。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// 資格情報マネージャー上のサービス名。
pub const SERVICE_NAME: &str = "Kiroscribe";

/// 改名前（Kiroku）のサービス名。旧アプリで保存したキーの引き継ぎにだけ使う。
pub const LEGACY_SERVICE_NAME: &str = "Kiroku";

/// Windows 以外で使うファイルの名前（アプリデータフォルダの直下）。
#[cfg(any(not(windows), feature = "e2e"))]
const SECRETS_FILE: &str = "secrets.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// 文字起こし（OpenAI）
    OpenAi,
    /// 議事録生成・チャット（Anthropic / Claude）
    Anthropic,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::OpenAi, Provider::Anthropic];

    /// 資格情報マネージャー上のアカウント名。
    pub fn account(self) -> &'static str {
        match self {
            Provider::OpenAi => "openai-api-key",
            Provider::Anthropic => "anthropic-api-key",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Provider::OpenAi => "OpenAI",
            Provider::Anthropic => "Anthropic",
        }
    }
}

pub trait SecretStore: Send + Sync {
    fn get(&self, provider: Provider) -> Result<Option<String>>;
    fn set(&self, provider: Provider, value: &str) -> Result<()>;
    fn delete(&self, provider: Provider) -> Result<()>;
    /// 保存先の説明（設定画面に表示する）。
    fn description(&self) -> &'static str;
}

/// 画面に返すのは「設定済みかどうか」だけで、キーの値そのものは返さない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyStatus {
    pub openai: bool,
    pub anthropic: bool,
    pub storage: String,
}

pub fn api_key_status(store: &dyn SecretStore) -> Result<ApiKeyStatus> {
    Ok(ApiKeyStatus {
        openai: store.get(Provider::OpenAi)?.is_some(),
        anthropic: store.get(Provider::Anthropic)?.is_some(),
        storage: store.description().to_string(),
    })
}

/// キーを保存する。前後の空白を除き、空文字なら削除扱いにする。
pub fn set_api_key(store: &dyn SecretStore, provider: Provider, value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() {
        store.delete(provider)
    } else {
        store.set(provider, value)
    }
}

/// API 呼び出し前にキーを取り出す。未設定ならユーザ向けのエラーにする。
pub fn require_api_key(store: &dyn SecretStore, provider: Provider) -> Result<String> {
    store
        .get(provider)?
        .filter(|key| !key.is_empty())
        .ok_or(CoreError::MissingApiKey(provider.label()))
}

/// 旧ストアにあって新ストアにないキーを、新ストアへ保存し直す。引き継いだキーの数を返す。
///
/// 新ストアに既にあるキーは上書きしない。旧ストアのキーは消さない。
pub fn migrate_secrets(legacy: &dyn SecretStore, current: &dyn SecretStore) -> Result<usize> {
    let mut migrated = 0;
    for provider in Provider::ALL {
        if current.get(provider)?.is_some() {
            continue;
        }
        if let Some(value) = legacy.get(provider)?.filter(|key| !key.is_empty()) {
            current.set(provider, &value)?;
            migrated += 1;
        }
    }
    Ok(migrated)
}

/// テスト用のメモリ上のストア。
#[derive(Default)]
pub struct MemorySecretStore {
    values: Mutex<HashMap<Provider, String>>,
}

impl SecretStore for MemorySecretStore {
    fn get(&self, provider: Provider) -> Result<Option<String>> {
        Ok(self.values.lock().unwrap().get(&provider).cloned())
    }

    fn set(&self, provider: Provider, value: &str) -> Result<()> {
        self.values
            .lock()
            .unwrap()
            .insert(provider, value.to_string());
        Ok(())
    }

    fn delete(&self, provider: Provider) -> Result<()> {
        self.values.lock().unwrap().remove(&provider);
        Ok(())
    }

    fn description(&self) -> &'static str {
        "メモリ（テスト用）"
    }
}

/// Windows 以外の開発環境向け：JSON ファイルに保存する。
pub struct FileSecretStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileSecretStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    fn read_all(&self) -> Result<HashMap<Provider, String>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(serde_json::from_str(&text)?),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(err) => Err(err.into()),
        }
    }

    fn write_all(&self, values: &HashMap<Provider, String>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, serde_json::to_vec(values)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

impl SecretStore for FileSecretStore {
    fn get(&self, provider: Provider) -> Result<Option<String>> {
        let _guard = self.lock.lock().unwrap();
        Ok(self.read_all()?.get(&provider).cloned())
    }

    fn set(&self, provider: Provider, value: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap();
        let mut values = self.read_all()?;
        values.insert(provider, value.to_string());
        self.write_all(&values)
    }

    fn delete(&self, provider: Provider) -> Result<()> {
        let _guard = self.lock.lock().unwrap();
        let mut values = self.read_all()?;
        if values.remove(&provider).is_some() {
            self.write_all(&values)?;
        }
        Ok(())
    }

    fn description(&self) -> &'static str {
        "アプリデータフォルダのファイル（開発用）"
    }
}

#[cfg(windows)]
mod windows_store {
    use std::sync::OnceLock;

    use super::*;

    fn secret_error(err: keyring_core::Error) -> CoreError {
        CoreError::Secret(err.to_string())
    }

    /// Windows 資格情報マネージャーに保存するストア。
    pub struct CredentialManagerStore {
        service: String,
    }

    impl CredentialManagerStore {
        pub fn new() -> Result<Self> {
            Self::with_service(SERVICE_NAME)
        }

        /// サービス名を指定して作る（テストで本物のキーと衝突させないため）。
        pub fn with_service(service: &str) -> Result<Self> {
            static INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
            INIT.get_or_init(|| {
                let store =
                    windows_native_keyring_store::Store::new().map_err(|e| e.to_string())?;
                keyring_core::set_default_store(store);
                Ok(())
            })
            .clone()
            .map_err(CoreError::Secret)?;
            Ok(Self {
                service: service.to_string(),
            })
        }

        fn entry(&self, provider: Provider) -> Result<keyring_core::Entry> {
            keyring_core::Entry::new(&self.service, provider.account()).map_err(secret_error)
        }
    }

    impl SecretStore for CredentialManagerStore {
        fn get(&self, provider: Provider) -> Result<Option<String>> {
            match self.entry(provider)?.get_password() {
                Ok(value) => Ok(Some(value)),
                Err(keyring_core::Error::NoEntry) => Ok(None),
                Err(err) => Err(secret_error(err)),
            }
        }

        fn set(&self, provider: Provider, value: &str) -> Result<()> {
            self.entry(provider)?
                .set_password(value)
                .map_err(secret_error)
        }

        fn delete(&self, provider: Provider) -> Result<()> {
            match self.entry(provider)?.delete_credential() {
                Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
                Err(err) => Err(secret_error(err)),
            }
        }

        fn description(&self) -> &'static str {
            "Windows 資格情報マネージャー"
        }
    }
}

#[cfg(windows)]
pub use windows_store::CredentialManagerStore;

/// 実行環境に応じたストアを返す。`data_dir` は Windows 以外のフォールバック先。
///
/// E2E テスト用のビルド（feature `e2e`）では、Windows でも資格情報マネージャーに触れず、ファイルに保存する。
pub fn platform_store(data_dir: &Path) -> Result<Box<dyn SecretStore>> {
    #[cfg(all(windows, not(feature = "e2e")))]
    {
        let _ = data_dir;
        Ok(Box::new(CredentialManagerStore::new()?))
    }
    #[cfg(any(not(windows), feature = "e2e"))]
    {
        Ok(Box::new(FileSecretStore::new(data_dir.join(SECRETS_FILE))))
    }
}

/// 改名前（Kiroku）のアプリがキーを保存していたストア。
///
/// Windows では資格情報マネージャーの旧サービス名、Windows 以外では旧アプリのデータフォルダのファイル。
/// 旧アプリのデータフォルダを特定できないときは `None`。
pub fn legacy_platform_store(data_dir: &Path) -> Result<Option<Box<dyn SecretStore>>> {
    #[cfg(all(windows, not(feature = "e2e")))]
    {
        let _ = data_dir;
        Ok(Some(Box::new(CredentialManagerStore::with_service(
            LEGACY_SERVICE_NAME,
        )?)))
    }
    #[cfg(any(not(windows), feature = "e2e"))]
    {
        Ok(crate::legacy::legacy_data_dir(data_dir).map(|dir| {
            Box::new(FileSecretStore::new(dir.join(SECRETS_FILE))) as Box<dyn SecretStore>
        }))
    }
}

/// 改名前（Kiroku）のアプリで保存した API キーを、いまのストアへ引き継ぐ。引き継いだキーの数を返す。
pub fn migrate_legacy_api_keys(data_dir: &Path, current: &dyn SecretStore) -> Result<usize> {
    match legacy_platform_store(data_dir)? {
        Some(legacy) => migrate_secrets(legacy.as_ref(), current),
        None => Ok(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exercise(store: &dyn SecretStore) {
        assert_eq!(
            api_key_status(store).unwrap(),
            ApiKeyStatus {
                openai: false,
                anthropic: false,
                storage: store.description().to_string()
            }
        );
        assert!(matches!(
            require_api_key(store, Provider::Anthropic).unwrap_err(),
            CoreError::MissingApiKey("Anthropic")
        ));

        set_api_key(store, Provider::Anthropic, "  sk-ant-test  ").unwrap();
        assert_eq!(
            require_api_key(store, Provider::Anthropic).unwrap(),
            "sk-ant-test"
        );
        let status = api_key_status(store).unwrap();
        assert!(status.anthropic);
        assert!(!status.openai);

        // 空文字で保存すると削除になる
        set_api_key(store, Provider::Anthropic, "   ").unwrap();
        assert!(!api_key_status(store).unwrap().anthropic);
        // 存在しないキーの削除はエラーにしない
        store.delete(Provider::OpenAi).unwrap();
    }

    // [KEY-001][KEY-002]
    #[test]
    fn memory_store_behaviour() {
        exercise(&MemorySecretStore::default());
    }

    // [KEY-005][KEY-006]
    #[test]
    fn file_store_behaviour_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("secrets.json");
        let store = FileSecretStore::new(path.clone());
        exercise(&store);

        store.set(Provider::OpenAi, "sk-openai").unwrap();
        // 別インスタンスから読める（永続化されている）
        let reopened = FileSecretStore::new(path.clone());
        assert_eq!(
            reopened.get(Provider::OpenAi).unwrap().as_deref(),
            Some("sk-openai")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    /// 本物の資格情報マネージャーを使う（Windows の CI で実行される）。
    /// 利用者のキーを上書きしないよう、テスト専用のサービス名を使う。
    // [KEY-004]
    #[cfg(windows)]
    #[test]
    fn credential_manager_store_behaviour() {
        let service = format!("Kiroscribe-test-{}", uuid::Uuid::new_v4());
        let store = CredentialManagerStore::with_service(&service).unwrap();
        exercise(&store);
        store.set(Provider::OpenAi, "sk-openai").unwrap();
        assert_eq!(
            store.get(Provider::OpenAi).unwrap().as_deref(),
            Some("sk-openai")
        );
        store.delete(Provider::OpenAi).unwrap();
        assert_eq!(store.get(Provider::OpenAi).unwrap(), None);
    }

    // [DAT-008]
    #[test]
    fn migrate_copies_only_keys_missing_from_the_current_store() {
        let legacy = MemorySecretStore::default();
        let current = MemorySecretStore::default();
        // 旧にも新にも何もなければ何もしない
        assert_eq!(migrate_secrets(&legacy, &current).unwrap(), 0);
        assert_eq!(current.get(Provider::OpenAi).unwrap(), None);

        legacy.set(Provider::OpenAi, "sk-old-openai").unwrap();
        legacy.set(Provider::Anthropic, "sk-old-anthropic").unwrap();
        // 新ストアに既にあるキーは上書きしない
        current.set(Provider::Anthropic, "sk-new-anthropic").unwrap();

        assert_eq!(migrate_secrets(&legacy, &current).unwrap(), 1);
        assert_eq!(
            current.get(Provider::OpenAi).unwrap().as_deref(),
            Some("sk-old-openai")
        );
        assert_eq!(
            current.get(Provider::Anthropic).unwrap().as_deref(),
            Some("sk-new-anthropic")
        );
        // 旧ストアのキーは消さない
        assert_eq!(
            legacy.get(Provider::OpenAi).unwrap().as_deref(),
            Some("sk-old-openai")
        );
        // もう一度実行しても、引き継ぐものはない
        assert_eq!(migrate_secrets(&legacy, &current).unwrap(), 0);
    }

    /// Windows 以外: 旧アプリのデータフォルダ（`dev.kiroku.desktop`）のファイルから引き継ぐ。
    // [DAT-008]
    #[cfg(not(windows))]
    #[test]
    fn legacy_api_keys_move_from_the_legacy_data_dir() {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("dev.kiroscribe.desktop");
        let legacy_file = root.path().join("dev.kiroku.desktop").join(SECRETS_FILE);
        let current = platform_store(&data_dir).unwrap();

        // 旧アプリのファイルがなければ何もしない（新しいファイルも作らない）
        assert_eq!(migrate_legacy_api_keys(&data_dir, current.as_ref()).unwrap(), 0);
        assert!(!data_dir.join(SECRETS_FILE).exists());

        FileSecretStore::new(legacy_file.clone())
            .set(Provider::OpenAi, "sk-old-openai")
            .unwrap();
        assert_eq!(migrate_legacy_api_keys(&data_dir, current.as_ref()).unwrap(), 1);
        assert_eq!(
            require_api_key(current.as_ref(), Provider::OpenAi).unwrap(),
            "sk-old-openai"
        );
        assert_eq!(current.get(Provider::Anthropic).unwrap(), None);
        // 旧アプリのファイルは残る
        assert_eq!(
            FileSecretStore::new(legacy_file)
                .get(Provider::OpenAi)
                .unwrap()
                .as_deref(),
            Some("sk-old-openai")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(data_dir.join(SECRETS_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    /// 本物の資格情報マネージャーで、サービス名をまたぐ引き継ぎを確かめる（Windows の CI で実行される）。
    // [DAT-008]
    #[cfg(windows)]
    #[test]
    fn credential_manager_keys_move_between_services() {
        let id = uuid::Uuid::new_v4();
        let legacy =
            CredentialManagerStore::with_service(&format!("Kiroscribe-test-legacy-{id}")).unwrap();
        let current =
            CredentialManagerStore::with_service(&format!("Kiroscribe-test-{id}")).unwrap();
        legacy.set(Provider::OpenAi, "sk-old-openai").unwrap();

        assert_eq!(migrate_secrets(&legacy, &current).unwrap(), 1);
        assert_eq!(
            current.get(Provider::OpenAi).unwrap().as_deref(),
            Some("sk-old-openai")
        );
        assert_eq!(current.get(Provider::Anthropic).unwrap(), None);
        assert_eq!(
            legacy.get(Provider::OpenAi).unwrap().as_deref(),
            Some("sk-old-openai")
        );

        legacy.delete(Provider::OpenAi).unwrap();
        current.delete(Provider::OpenAi).unwrap();
    }

    // [KEY-008][IPC-004]
    #[test]
    fn provider_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&Provider::OpenAi).unwrap(),
            "\"openai\""
        );
        assert_eq!(
            serde_json::from_str::<Provider>("\"anthropic\"").unwrap(),
            Provider::Anthropic
        );
    }
}
