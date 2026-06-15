//! 连接配置、设置和密钥的持久化层。
//!
//! 公开的连接配置和设置 JSON 保持可迁移、可人工检查。密钥通过操作系统
//! 凭据环和本地加密保管库分开存储，因此配置导入导出可以迁移凭据，
//! 又不会把明文写入 `profiles.json`。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use keyring::Entry;
use serde::{Deserialize, Serialize};
use shell_core::{AuthConfig, ConnectionProfile, CredentialsRef, Result, ShellError};

const EXPORT_BUNDLE_VERSION: u32 = 1;
const SECRETS_FILE_NAME: &str = "secrets.json";
const SETTINGS_FILE_NAME: &str = "settings.json";
const APP_PASSWORD_SERVICE: &str = "dev.shell.app";
const APP_PASSWORD_ACCOUNT: &str = "master-password";

#[cfg(windows)]
const SECRET_FORMAT: &str = "windows-dpapi-v1";
#[cfg(not(windows))]
const SECRET_FORMAT: &str = "keyring-metadata-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProfilesDocument {
    pub profiles: Vec<ConnectionProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RendererBackend {
    #[default]
    Cairo,
    #[serde(alias = "ngl")]
    Gl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AppLanguage {
    #[default]
    ZhCn,
    EnUs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpensshCompatibilitySettings {
    /// 总开关。只有启用后才会应用下面的兼容性算法组；默认不影响现代 OpenSSH 路径。
    #[serde(default)]
    pub enabled: bool,
    /// OpenSSH 8.8+ 默认禁用 RSA/SHA-1；仅在连接旧服务器时启用。
    #[serde(default = "default_openssh_rsa_sha1_compatibility")]
    pub rsa_sha1: bool,
    #[serde(default)]
    pub dss_host_key: bool,
    #[serde(default)]
    pub legacy_kex: bool,
    #[serde(default)]
    pub legacy_ciphers_macs: bool,
    #[serde(default = "default_legacy_openssh_compatibility")]
    pub legacy_openssh: bool,
    #[serde(default = "default_regional_crypto_compatibility")]
    pub regional_crypto: bool,
    #[serde(default)]
    pub weak_kex: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinToolsPathPriority {
    #[default]
    SystemFirst,
    ToolchainFirst,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltinToolsSettings {
    #[serde(default = "default_builtin_tools_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub inject_into_system_shells: bool,
    #[serde(default = "default_builtin_tools_use_for_ssh_client")]
    pub use_for_ssh_client: bool,
    #[serde(default)]
    pub path_priority: BuiltinToolsPathPriority,
}

fn default_builtin_tools_enabled() -> bool {
    true
}

fn default_builtin_tools_use_for_ssh_client() -> bool {
    true
}

impl Default for BuiltinToolsSettings {
    fn default() -> Self {
        Self {
            enabled: default_builtin_tools_enabled(),
            inject_into_system_shells: false,
            use_for_ssh_client: default_builtin_tools_use_for_ssh_client(),
            path_priority: BuiltinToolsPathPriority::default(),
        }
    }
}

fn default_legacy_openssh_compatibility() -> bool {
    true
}

fn default_openssh_rsa_sha1_compatibility() -> bool {
    true
}

fn default_regional_crypto_compatibility() -> bool {
    false
}

impl Default for OpensshCompatibilitySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            rsa_sha1: default_openssh_rsa_sha1_compatibility(),
            dss_host_key: false,
            legacy_kex: false,
            legacy_ciphers_macs: false,
            legacy_openssh: default_legacy_openssh_compatibility(),
            regional_crypto: default_regional_crypto_compatibility(),
            weak_kex: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    /// 终端视图使用的 Pango 字体描述。
    pub terminal_font: String,
    #[serde(default)]
    pub renderer_backend: RendererBackend,
    #[serde(default = "default_semantic_highlighting")]
    pub semantic_highlighting: bool,
    #[serde(default)]
    pub language: AppLanguage,
    #[serde(default)]
    pub openssh_compatibility: OpensshCompatibilitySettings,
    #[serde(default)]
    pub builtin_tools: BuiltinToolsSettings,
}

fn default_semantic_highlighting() -> bool {
    true
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            terminal_font:
                "Cascadia Mono, Microsoft YaHei UI, SimSun-ExtB, Segoe UI Emoji, Segoe UI Symbol 13"
                    .to_string(),
            renderer_backend: RendererBackend::default(),
            semantic_highlighting: default_semantic_highlighting(),
            language: AppLanguage::default(),
            openssh_compatibility: OpensshCompatibilitySettings::default(),
            builtin_tools: BuiltinToolsSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfilesExportBundle {
    pub version: u32,
    pub secret_format: String,
    pub profiles: Vec<ConnectionProfile>,
    pub secrets: Vec<ExportedSecret>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedSecret {
    pub service: String,
    pub account: String,
    pub encrypted_value: String,
}

#[derive(Debug, Clone)]
pub struct ProfileStore {
    /// `profiles.json`：不含密钥的连接配置元数据。
    path: PathBuf,
    /// `settings.json`：全局 UI/终端设置。
    settings_path: PathBuf,
    /// 与连接配置文件同目录的加密密钥镜像。
    secret_vault: SecretVault,
}

#[derive(Debug, Clone)]
struct SecretVault {
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
struct SecretsDocument {
    secrets: BTreeMap<String, StoredSecret>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredSecret {
    encrypted_value: String,
}

impl ProfileStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let settings_path = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(SETTINGS_FILE_NAME);
        let secret_vault = SecretVault::for_profiles_path(&path);
        Self {
            path,
            settings_path,
            secret_vault,
        }
    }

    pub fn load(&self) -> Result<ProfilesDocument> {
        load_profiles(&self.path)
    }

    pub fn save(&self, document: &ProfilesDocument) -> Result<()> {
        save_profiles(&self.path, document)
    }

    pub fn load_settings(&self) -> Result<AppSettings> {
        load_settings(&self.settings_path)
    }

    pub fn save_settings(&self, settings: &AppSettings) -> Result<()> {
        save_settings(&self.settings_path, settings)
    }

    pub fn export_bundle(&self, document: &ProfilesDocument, path: &Path) -> Result<()> {
        let bundle = ProfilesExportBundle {
            version: EXPORT_BUNDLE_VERSION,
            secret_format: SECRET_FORMAT.to_string(),
            profiles: document.profiles.clone(),
            secrets: self.secret_vault.export_records(document)?,
        };
        save_export_bundle(path, &bundle)
    }

    pub fn import_bundle(&self, path: &Path) -> Result<ProfilesDocument> {
        let bundle = load_export_bundle(path)?;
        if bundle.version != EXPORT_BUNDLE_VERSION {
            return Err(ShellError::Storage(format!(
                "Unsupported import bundle version: {}",
                bundle.version
            )));
        }
        if !bundle.secrets.is_empty() && bundle.secret_format != SECRET_FORMAT {
            return Err(ShellError::Storage(format!(
                "This export uses secret format '{}' but this build supports '{}'",
                bundle.secret_format, SECRET_FORMAT
            )));
        }

        self.secret_vault.import_records(&bundle.secrets)?;
        let document = ProfilesDocument {
            profiles: bundle.profiles,
        };
        self.save(&document)?;
        Ok(document)
    }
}

impl SecretVault {
    #[must_use]
    fn for_profiles_path(path: &Path) -> Self {
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        Self {
            path: base.join(SECRETS_FILE_NAME),
        }
    }

    #[must_use]
    fn default() -> Self {
        let root = dirs_next::config_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            path: root.join("shell").join(SECRETS_FILE_NAME),
        }
    }

    fn store(&self, service: &str, account: &str, secret: &str) -> Result<()> {
        // 保持本地 vault 加密；明文只在用户连接或为同一系统账号导出时短暂存在于内存。
        let mut document = self.load_document()?;
        document.secrets.insert(
            secret_key(service, account),
            StoredSecret {
                encrypted_value: encrypt_secret(secret)?,
            },
        );
        self.save_document(&document)
    }

    fn load(&self, service: &str, account: &str) -> Result<Option<String>> {
        let document = self.load_document()?;
        let Some(record) = document.secrets.get(&secret_key(service, account)) else {
            return Ok(None);
        };
        decrypt_secret(&record.encrypted_value).map(Some)
    }

    fn delete(&self, service: &str, account: &str) -> Result<()> {
        if !self.path.exists() {
            return Ok(());
        }

        let mut document = self.load_document()?;
        if document
            .secrets
            .remove(&secret_key(service, account))
            .is_some()
        {
            self.save_document(&document)?;
        }
        Ok(())
    }

    fn export_records(&self, document: &ProfilesDocument) -> Result<Vec<ExportedSecret>> {
        let mut exported = Vec::new();
        for (service, account) in password_secret_refs(document) {
            if let Some(record) = self.ensure_record(&service, &account)? {
                exported.push(ExportedSecret {
                    service,
                    account,
                    encrypted_value: record.encrypted_value,
                });
            }
        }
        Ok(exported)
    }

    fn import_records(&self, records: &[ExportedSecret]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }

        let mut document = self.load_document()?;
        for record in records {
            let password = decrypt_secret(&record.encrypted_value)?;
            document.secrets.insert(
                secret_key(&record.service, &record.account),
                StoredSecret {
                    encrypted_value: record.encrypted_value.clone(),
                },
            );
            let _ = store_secret_in_keyring(&record.service, &record.account, &password);
        }
        self.save_document(&document)
    }

    fn ensure_record(&self, service: &str, account: &str) -> Result<Option<StoredSecret>> {
        let key = secret_key(service, account);
        let mut document = self.load_document()?;
        if let Some(record) = document.secrets.get(&key) {
            return Ok(Some(record.clone()));
        }

        let Some(password) = load_secret_from_keyring(service, account)? else {
            return Ok(None);
        };
        let record = StoredSecret {
            encrypted_value: encrypt_secret(&password)?,
        };
        document.secrets.insert(key, record.clone());
        self.save_document(&document)?;
        Ok(Some(record))
    }

    fn load_document(&self) -> Result<SecretsDocument> {
        if !self.path.exists() {
            return Ok(SecretsDocument::default());
        }

        let content =
            fs::read_to_string(&self.path).map_err(|err| ShellError::Storage(err.to_string()))?;
        serde_json::from_str(&content).map_err(|err| ShellError::Storage(err.to_string()))
    }

    fn save_document(&self, document: &SecretsDocument) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|err| ShellError::Storage(err.to_string()))?;
        }

        let content = serde_json::to_string_pretty(document)
            .map_err(|err| ShellError::Storage(err.to_string()))?;
        fs::write(&self.path, content).map_err(|err| ShellError::Storage(err.to_string()))
    }
}

pub fn load_profiles(path: &Path) -> Result<ProfilesDocument> {
    if !path.exists() {
        return Ok(ProfilesDocument::default());
    }

    let content = fs::read_to_string(path).map_err(|err| ShellError::Storage(err.to_string()))?;
    serde_json::from_str(&content).map_err(|err| ShellError::Storage(err.to_string()))
}

pub fn save_profiles(path: &Path, document: &ProfilesDocument) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| ShellError::Storage(err.to_string()))?;
    }

    let content = serde_json::to_string_pretty(document)
        .map_err(|err| ShellError::Storage(err.to_string()))?;
    fs::write(path, content).map_err(|err| ShellError::Storage(err.to_string()))
}

pub fn load_settings(path: &Path) -> Result<AppSettings> {
    if !path.exists() {
        return Ok(AppSettings::default());
    }

    let content = fs::read_to_string(path).map_err(|err| ShellError::Storage(err.to_string()))?;
    serde_json::from_str(&content).map_err(|err| ShellError::Storage(err.to_string()))
}

pub fn save_settings(path: &Path, settings: &AppSettings) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| ShellError::Storage(err.to_string()))?;
    }

    let content = serde_json::to_string_pretty(settings)
        .map_err(|err| ShellError::Storage(err.to_string()))?;
    fs::write(path, content).map_err(|err| ShellError::Storage(err.to_string()))
}

pub fn store_secret(service: &str, account: &str, secret: &str) -> Result<CredentialsRef> {
    let vault = SecretVault::default();
    let vault_result = vault.store(service, account, secret);
    let keyring_result = store_secret_in_keyring(service, account, secret);

    if vault_result.is_err() && keyring_result.is_err() {
        return Err(ShellError::Storage(join_storage_errors(&[
            vault_result.err().unwrap(),
            keyring_result.err().unwrap(),
        ])));
    }

    Ok(CredentialsRef::SystemKeychain {
        service: service.to_string(),
        account: account.to_string(),
    })
}

pub fn load_secret(reference: &CredentialsRef) -> Result<Option<String>> {
    let CredentialsRef::SystemKeychain { service, account } = reference else {
        return Ok(None);
    };

    let vault = SecretVault::default();
    let mut vault_error = None;
    match vault.load(service, account) {
        Ok(Some(password)) => return Ok(Some(password)),
        Ok(None) => {}
        Err(err) => vault_error = Some(err),
    }

    match load_secret_from_keyring(service, account) {
        Ok(Some(password)) => {
            let _ = vault.store(service, account, &password);
            Ok(Some(password))
        }
        Ok(None) => {
            if let Some(err) = vault_error {
                Err(err)
            } else {
                Ok(None)
            }
        }
        Err(err) => {
            if let Some(vault_err) = vault_error {
                Err(ShellError::Storage(join_storage_errors(&[vault_err, err])))
            } else {
                Err(err)
            }
        }
    }
}

pub fn delete_secret(reference: &CredentialsRef) -> Result<()> {
    let CredentialsRef::SystemKeychain { service, account } = reference else {
        return Ok(());
    };

    let vault = SecretVault::default();
    let vault_result = vault.delete(service, account);
    let keyring_result = delete_secret_from_keyring(service, account);

    if vault_result.is_err() && keyring_result.is_err() {
        return Err(ShellError::Storage(join_storage_errors(&[
            vault_result.err().unwrap(),
            keyring_result.err().unwrap(),
        ])));
    }

    Ok(())
}

pub fn app_password_configured() -> Result<bool> {
    load_secret(&app_password_reference()).map(|password| password.is_some())
}

pub fn set_app_password(password: &str) -> Result<()> {
    if password.is_empty() {
        return Err(ShellError::InvalidConfig(
            "Application password must not be empty".to_string(),
        ));
    }

    store_secret(APP_PASSWORD_SERVICE, APP_PASSWORD_ACCOUNT, password).map(|_| ())
}

pub fn verify_app_password(password: &str) -> Result<bool> {
    let Some(stored_password) = load_secret(&app_password_reference())? else {
        return Ok(false);
    };

    Ok(constant_time_eq(
        stored_password.as_bytes(),
        password.as_bytes(),
    ))
}

fn app_password_reference() -> CredentialsRef {
    CredentialsRef::SystemKeychain {
        service: APP_PASSWORD_SERVICE.to_string(),
        account: APP_PASSWORD_ACCOUNT.to_string(),
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut diff = left.len() ^ right.len();
    for index in 0..max_len {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(left_byte ^ right_byte);
    }
    diff == 0
}

fn save_export_bundle(path: &Path, bundle: &ProfilesExportBundle) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| ShellError::Storage(err.to_string()))?;
    }

    let content =
        serde_json::to_string_pretty(bundle).map_err(|err| ShellError::Storage(err.to_string()))?;
    fs::write(path, content).map_err(|err| ShellError::Storage(err.to_string()))
}

fn load_export_bundle(path: &Path) -> Result<ProfilesExportBundle> {
    let content = fs::read_to_string(path).map_err(|err| ShellError::Storage(err.to_string()))?;
    serde_json::from_str(&content).map_err(|err| ShellError::Storage(err.to_string()))
}

fn password_secret_refs(document: &ProfilesDocument) -> BTreeSet<(String, String)> {
    let mut refs = BTreeSet::new();
    for profile in &document.profiles {
        if let AuthConfig::Password {
            password_ref: CredentialsRef::SystemKeychain { service, account },
            ..
        } = &profile.auth
        {
            refs.insert((service.clone(), account.clone()));
        }
    }
    refs
}

fn secret_key(service: &str, account: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{service}\n{account}"))
}

fn store_secret_in_keyring(service: &str, account: &str, secret: &str) -> Result<()> {
    let entry = Entry::new(service, account).map_err(|err| ShellError::Storage(err.to_string()))?;
    entry
        .set_password(secret)
        .map_err(|err| ShellError::Storage(err.to_string()))
}

fn load_secret_from_keyring(service: &str, account: &str) -> Result<Option<String>> {
    let entry = Entry::new(service, account).map_err(|err| ShellError::Storage(err.to_string()))?;
    match entry.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(ShellError::Storage(err.to_string())),
    }
}

fn delete_secret_from_keyring(service: &str, account: &str) -> Result<()> {
    let entry = Entry::new(service, account).map_err(|err| ShellError::Storage(err.to_string()))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(ShellError::Storage(err.to_string())),
    }
}

fn join_storage_errors(errors: &[ShellError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(windows)]
fn encrypt_secret(secret: &str) -> Result<String> {
    use std::ptr;
    use std::slice;

    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptProtectData};

    let input = secret.as_bytes();
    let input_len = u32::try_from(input.len())
        .map_err(|_| ShellError::Storage("Secret is too large to encrypt".to_string()))?;
    let input_blob = CRYPT_INTEGER_BLOB {
        cbData: input_len,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut output_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };

    let status = unsafe {
        CryptProtectData(
            &input_blob,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            0,
            &mut output_blob,
        )
    };
    if status == 0 {
        return Err(ShellError::Storage(format!(
            "Windows secret encryption failed: {}",
            unsafe { GetLastError() }
        )));
    }

    let encrypted = unsafe {
        let bytes = slice::from_raw_parts(output_blob.pbData, output_blob.cbData as usize).to_vec();
        let _ = LocalFree(output_blob.pbData.cast());
        bytes
    };
    Ok(base64::engine::general_purpose::STANDARD.encode(encrypted))
}

#[cfg(not(windows))]
fn encrypt_secret(secret: &str) -> Result<String> {
    let _ = secret;
    Err(ShellError::Storage(
        "Encrypted secret vault is only supported on Windows in this build".to_string(),
    ))
}

#[cfg(windows)]
fn decrypt_secret(encrypted_value: &str) -> Result<String> {
    use std::ptr;
    use std::slice;

    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptUnprotectData};

    let encrypted = base64::engine::general_purpose::STANDARD
        .decode(encrypted_value)
        .map_err(|err| ShellError::Storage(err.to_string()))?;
    let input_len = u32::try_from(encrypted.len())
        .map_err(|_| ShellError::Storage("Encrypted secret is too large to decrypt".to_string()))?;
    let input_blob = CRYPT_INTEGER_BLOB {
        cbData: input_len,
        pbData: encrypted.as_ptr() as *mut u8,
    };
    let mut output_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };

    let status = unsafe {
        CryptUnprotectData(
            &input_blob,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            0,
            &mut output_blob,
        )
    };
    if status == 0 {
        return Err(ShellError::Storage(format!(
            "Windows secret decryption failed: {}",
            unsafe { GetLastError() }
        )));
    }

    let decrypted = unsafe {
        let bytes = slice::from_raw_parts(output_blob.pbData, output_blob.cbData as usize).to_vec();
        let _ = LocalFree(output_blob.pbData.cast());
        bytes
    };
    String::from_utf8(decrypted).map_err(|err| ShellError::Storage(err.to_string()))
}

#[cfg(not(windows))]
fn decrypt_secret(encrypted_value: &str) -> Result<String> {
    let _ = encrypted_value;
    Err(ShellError::Storage(
        "Encrypted secret vault is only supported on Windows in this build".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use shell_core::ProtocolKind;

    use super::*;

    fn unique_temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "shell-storage-{label}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn saves_and_loads_profiles() {
        let root = unique_temp_dir("profiles");
        let path = root.join("profiles.json");
        let store = ProfileStore::new(&path);
        let document = ProfilesDocument {
            profiles: vec![ConnectionProfile::local_shell("Local")],
        };

        store.save(&document).unwrap();
        let loaded = store.load().unwrap();

        assert_eq!(loaded.profiles.len(), 1);
        assert_eq!(loaded.profiles[0].name, "Local");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn saves_and_loads_settings() {
        let root = unique_temp_dir("settings");
        let settings_path = root.join("settings.json");
        let settings = AppSettings {
            terminal_font: "JetBrains Mono 13".to_string(),
            renderer_backend: RendererBackend::Gl,
            semantic_highlighting: false,
            language: AppLanguage::EnUs,
            openssh_compatibility: OpensshCompatibilitySettings {
                enabled: true,
                rsa_sha1: true,
                dss_host_key: true,
                legacy_kex: true,
                legacy_ciphers_macs: true,
                legacy_openssh: true,
                regional_crypto: true,
                weak_kex: true,
            },
            builtin_tools: BuiltinToolsSettings {
                enabled: true,
                inject_into_system_shells: true,
                use_for_ssh_client: true,
                path_priority: BuiltinToolsPathPriority::ToolchainFirst,
            },
        };

        save_settings(&settings_path, &settings).unwrap();
        let loaded = load_settings(&settings_path).unwrap();

        assert_eq!(loaded, settings);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn loads_legacy_settings_without_builtin_tools() {
        let settings: AppSettings = serde_json::from_str(
            r#"{
                "terminal_font": "Consolas 12",
                "renderer_backend": "cairo",
                "semantic_highlighting": true,
                "language": "zh_cn",
                "openssh_compatibility": { "enabled": false }
            }"#,
        )
        .unwrap();

        assert_eq!(settings.builtin_tools, BuiltinToolsSettings::default());
    }

    #[test]
    fn loads_legacy_ngl_renderer_setting_as_gl() {
        let settings: AppSettings = serde_json::from_str(
            r#"{
                "terminal_font": "Consolas 12",
                "renderer_backend": "ngl",
                "semantic_highlighting": true,
                "language": "zh_cn"
            }"#,
        )
        .unwrap();

        assert_eq!(settings.renderer_backend, RendererBackend::Gl);
        assert_eq!(
            serde_json::to_value(&settings).unwrap()["renderer_backend"],
            "gl"
        );
    }

    #[cfg(windows)]
    #[test]
    fn vault_roundtrip_and_bundle_import_export() {
        let root = unique_temp_dir("vault");
        let path = root.join("profiles.json");
        let export_path = root.join("backup").join("profiles-export.json");
        let store = ProfileStore::new(&path);
        let vault = SecretVault::for_profiles_path(&path);
        let service = format!("dev.shell.test.{}", unique_temp_dir("service").display());
        let account = "alice";
        let password = "p@ssw0rd!";

        vault.store(&service, account, password).unwrap();
        assert_eq!(
            vault.load(&service, account).unwrap().as_deref(),
            Some(password)
        );

        let mut profile = ConnectionProfile::new("SSH", ProtocolKind::Ssh);
        profile.host = Some("127.0.0.1".to_string());
        profile.port = Some(22);
        profile.auth = AuthConfig::Password {
            username: account.to_string(),
            password_ref: CredentialsRef::SystemKeychain {
                service: service.clone(),
                account: account.to_string(),
            },
        };
        let document = ProfilesDocument {
            profiles: vec![profile.clone()],
        };

        store.save(&document).unwrap();
        store.export_bundle(&document, &export_path).unwrap();

        let imported = store.import_bundle(&export_path).unwrap();
        assert_eq!(imported.profiles, vec![profile]);
        assert_eq!(
            vault.load(&service, account).unwrap().as_deref(),
            Some(password)
        );

        let _ = delete_secret_from_keyring(&service, account);
        let _ = fs::remove_dir_all(root);
    }
}
