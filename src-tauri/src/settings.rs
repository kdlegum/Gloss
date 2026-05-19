use crate::llm::LlmProvider;
use serde::Serialize;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const OPENAI_API_KEY: &str = "openai_api_key";
const GEMINI_API_KEY: &str = "gemini_api_key";
const DEEPSEEK_API_KEY: &str = "deepseek_api_key";
const ZAI_API_KEY: &str = "zai_api_key";
const AI_SETUP_COMPLETE: &str = "ai_setup_complete";
const LOCAL_SYNC_FOLDER_PATH: &str = "local_sync_folder_path";
const LOCAL_SYNC_LAST_FOLDER_PATH: &str = "local_sync_last_folder_path";
const AI_LOCAL_ONLY_DB_KEYS: [&str; 5] = [
    OPENAI_API_KEY,
    GEMINI_API_KEY,
    DEEPSEEK_API_KEY,
    ZAI_API_KEY,
    AI_SETUP_COMPLETE,
];
pub const LOCAL_ONLY_DB_KEYS: [&str; 7] = [
    OPENAI_API_KEY,
    GEMINI_API_KEY,
    DEEPSEEK_API_KEY,
    ZAI_API_KEY,
    AI_SETUP_COMPLETE,
    LOCAL_SYNC_FOLDER_PATH,
    LOCAL_SYNC_LAST_FOLDER_PATH,
];

static LOCAL_SETTINGS_PATH: OnceLock<PathBuf> = OnceLock::new();
static LOCAL_SETTINGS_WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
#[serde(default)]
pub struct LocalSettings {
    pub device_id: Option<String>,
    pub sync_folder_kind: Option<String>,
    pub sync_folder_path: Option<String>,
    pub last_sync_folder_path: Option<String>,
    pub sync_folder_tree_uri: Option<String>,
    pub last_sync_folder_tree_uri: Option<String>,
    pub sync_folder_label: Option<String>,
    pub last_sync_folder_label: Option<String>,
    pub sync_dirty: bool,
    pub last_exported_revision: Option<i64>,
    pub last_imported_revision: Option<i64>,
    pub ai_setup_complete: bool,
    pub api_keys: HashMap<String, String>,
}

#[derive(Serialize)]
pub struct AiSettingsState {
    pub running_on_android: bool,
    pub setup_complete: bool,
    pub openai_api_key_set: bool,
    pub gemini_api_key_set: bool,
    pub deepseek_api_key_set: bool,
    pub zai_api_key_set: bool,
}

pub async fn init_local_store(data_dir: &Path, pool: &SqlitePool) -> Result<(), String> {
    let path = data_dir.join("gloss-local-settings.json");
    let _ = LOCAL_SETTINGS_PATH.set(path);
    let mut local = read_local_settings()?;
    ensure_device_id(&mut local);

    for key in LOCAL_ONLY_DB_KEYS {
        if let Some(value) = get_setting(pool, key).await? {
            let value = value.trim();
            if !value.is_empty() {
                match key {
                    AI_SETUP_COMPLETE => {
                        if value == "true" {
                            local.ai_setup_complete = true;
                        }
                    }
                    OPENAI_API_KEY | GEMINI_API_KEY | DEEPSEEK_API_KEY | ZAI_API_KEY => {
                        local
                            .api_keys
                            .entry(key.to_string())
                            .or_insert_with(|| value.to_string());
                    }
                    LOCAL_SYNC_FOLDER_PATH => {
                        if local
                            .sync_folder_path
                            .as_deref()
                            .unwrap_or_default()
                            .is_empty()
                        {
                            local.sync_folder_path = Some(value.to_string());
                        }
                    }
                    LOCAL_SYNC_LAST_FOLDER_PATH => {
                        if local
                            .last_sync_folder_path
                            .as_deref()
                            .unwrap_or_default()
                            .is_empty()
                        {
                            local.last_sync_folder_path = Some(value.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
        if AI_LOCAL_ONLY_DB_KEYS.contains(&key) {
            delete_setting(pool, key).await?;
        }
    }

    write_local_settings(&local)?;
    save_sync_folder_fallback(
        pool,
        local.sync_folder_path.as_deref(),
        local.last_sync_folder_path.as_deref(),
    )
    .await
}

pub fn read_local_settings() -> Result<LocalSettings, String> {
    let path = local_settings_path()?;
    match read_local_settings_file(&path) {
        Ok(Some(settings)) => Ok(settings),
        Ok(None) => read_local_settings_backup(&path),
        Err(main_err) => match read_local_settings_backup(&path) {
            Ok(settings) if !settings.is_default() => Ok(settings),
            _ => Err(main_err),
        },
    }
}

pub fn write_local_settings(settings: &LocalSettings) -> Result<(), String> {
    let _guard = LOCAL_SETTINGS_WRITE_LOCK
        .lock()
        .map_err(|_| "local settings lock was poisoned".to_string())?;
    write_local_settings_unlocked(settings)
}

fn write_local_settings_unlocked(settings: &LocalSettings) -> Result<(), String> {
    let path = local_settings_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    let backup = local_settings_backup_path(&path);
    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    if path.exists() {
        let _ = std::fs::copy(&path, &backup);
    }
    std::fs::copy(&tmp, &path).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&tmp);
    Ok(())
}

pub fn update_local_settings<F>(update: F) -> Result<LocalSettings, String>
where
    F: FnOnce(&mut LocalSettings),
{
    let _guard = LOCAL_SETTINGS_WRITE_LOCK
        .lock()
        .map_err(|_| "local settings lock was poisoned".to_string())?;
    let mut settings = read_local_settings()?;
    ensure_device_id(&mut settings);
    update(&mut settings);
    write_local_settings_unlocked(&settings)?;
    Ok(settings)
}

pub fn local_device_id() -> Result<String, String> {
    let settings = update_local_settings(|settings| {
        ensure_device_id(settings);
    })?;
    settings
        .device_id
        .ok_or_else(|| "local device id was not initialised".to_string())
}

pub async fn sync_folder_fallback(
    pool: &SqlitePool,
) -> Result<(Option<String>, Option<String>), String> {
    Ok((
        normalize_optional_setting(get_setting(pool, LOCAL_SYNC_FOLDER_PATH).await?),
        normalize_optional_setting(get_setting(pool, LOCAL_SYNC_LAST_FOLDER_PATH).await?),
    ))
}

pub async fn save_sync_folder_fallback(
    pool: &SqlitePool,
    sync_folder_path: Option<&str>,
    last_sync_folder_path: Option<&str>,
) -> Result<(), String> {
    set_optional_setting(pool, LOCAL_SYNC_FOLDER_PATH, sync_folder_path).await?;
    set_optional_setting(pool, LOCAL_SYNC_LAST_FOLDER_PATH, last_sync_folder_path).await?;
    Ok(())
}

fn local_settings_path() -> Result<PathBuf, String> {
    LOCAL_SETTINGS_PATH
        .get()
        .cloned()
        .ok_or_else(|| "local settings store was not initialised".to_string())
}

fn local_settings_backup_path(path: &Path) -> PathBuf {
    path.with_file_name("gloss-local-settings.backup.json")
}

fn read_local_settings_backup(path: &Path) -> Result<LocalSettings, String> {
    match read_local_settings_file(&local_settings_backup_path(path)) {
        Ok(settings) => Ok(settings.unwrap_or_default()),
        Err(err) => {
            log::warn!(target: "gloss_lib::settings", "failed to read local settings backup: {err}");
            Ok(LocalSettings::default())
        }
    }
}

fn read_local_settings_file(path: &Path) -> Result<Option<LocalSettings>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.to_string()),
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| format!("failed to read local settings at {}: {e}", path.display()))
}

fn ensure_device_id(settings: &mut LocalSettings) {
    let needs_id = settings
        .device_id
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .is_empty();
    if needs_id {
        settings.device_id = Some(make_local_id("device"));
    }
}

impl LocalSettings {
    fn is_default(&self) -> bool {
        self.device_id.is_none()
            && self.sync_folder_kind.is_none()
            && self.sync_folder_path.is_none()
            && self.last_sync_folder_path.is_none()
            && self.sync_folder_tree_uri.is_none()
            && self.last_sync_folder_tree_uri.is_none()
            && self.sync_folder_label.is_none()
            && self.last_sync_folder_label.is_none()
            && !self.sync_dirty
            && self.last_exported_revision.is_none()
            && self.last_imported_revision.is_none()
            && !self.ai_setup_complete
            && self.api_keys.is_empty()
    }
}

pub fn make_local_id(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{nanos:x}-{:x}", std::process::id())
}

pub async fn state(_pool: &SqlitePool) -> Result<AiSettingsState, String> {
    let local = update_local_settings(|settings| {
        ensure_device_id(settings);
    })?;
    Ok(AiSettingsState {
        running_on_android: cfg!(target_os = "android"),
        setup_complete: local.ai_setup_complete,
        openai_api_key_set: local_secret_has_value(&local, OPENAI_API_KEY),
        gemini_api_key_set: local_secret_has_value(&local, GEMINI_API_KEY),
        deepseek_api_key_set: local_secret_has_value(&local, DEEPSEEK_API_KEY),
        zai_api_key_set: local_secret_has_value(&local, ZAI_API_KEY),
    })
}

pub async fn api_key_for_provider(
    _pool: &SqlitePool,
    provider: LlmProvider,
) -> Result<Option<String>, String> {
    let local = read_local_settings()?;
    match provider {
        LlmProvider::OpenAI => Ok(local_secret(&local, OPENAI_API_KEY)),
        LlmProvider::Gemini => Ok(local_secret(&local, GEMINI_API_KEY)),
        LlmProvider::DeepSeek => Ok(local_secret(&local, DEEPSEEK_API_KEY)),
        LlmProvider::Zai => Ok(local_secret(&local, ZAI_API_KEY)),
        LlmProvider::Ollama => Ok(None),
    }
}

pub async fn save_ai_api_keys(
    pool: &SqlitePool,
    openai_api_key: Option<String>,
    gemini_api_key: Option<String>,
    deepseek_api_key: Option<String>,
    zai_api_key: Option<String>,
    clear_openai_api_key: bool,
    clear_gemini_api_key: bool,
    clear_deepseek_api_key: bool,
    clear_zai_api_key: bool,
    setup_complete: bool,
) -> Result<AiSettingsState, String> {
    update_local_settings(|local| {
        set_or_clear_local_secret(
            local,
            OPENAI_API_KEY,
            normalize_secret(openai_api_key),
            clear_openai_api_key,
        );
        set_or_clear_local_secret(
            local,
            GEMINI_API_KEY,
            normalize_secret(gemini_api_key),
            clear_gemini_api_key,
        );
        set_or_clear_local_secret(
            local,
            DEEPSEEK_API_KEY,
            normalize_secret(deepseek_api_key),
            clear_deepseek_api_key,
        );
        set_or_clear_local_secret(
            local,
            ZAI_API_KEY,
            normalize_secret(zai_api_key),
            clear_zai_api_key,
        );
        if setup_complete {
            local.ai_setup_complete = true;
        }
    })?;

    for key in LOCAL_ONLY_DB_KEYS {
        delete_setting(pool, key).await?;
    }

    state(pool).await
}

fn local_secret_has_value(local: &LocalSettings, key: &str) -> bool {
    local_secret(local, key)
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
}

fn local_secret(local: &LocalSettings, key: &str) -> Option<String> {
    local
        .api_keys
        .get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn set_or_clear_local_secret(
    local: &mut LocalSettings,
    key: &str,
    value: Option<String>,
    clear: bool,
) {
    if clear {
        local.api_keys.remove(key);
    } else if let Some(value) = value {
        local.api_keys.insert(key.to_string(), value);
    }
}

async fn get_setting(pool: &SqlitePool, key: &str) -> Result<Option<String>, String> {
    sqlx::query_scalar("SELECT value FROM app_settings WHERE setting_key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())
}

async fn set_optional_setting(
    pool: &SqlitePool,
    key: &str,
    value: Option<&str>,
) -> Result<(), String> {
    let value = value.map(str::trim).filter(|value| !value.is_empty());
    if let Some(value) = value {
        sqlx::query(
            "INSERT INTO app_settings (setting_key, value, updated_at) \
             VALUES (?, ?, CURRENT_TIMESTAMP) \
             ON CONFLICT(setting_key) DO UPDATE \
             SET value = excluded.value, updated_at = CURRENT_TIMESTAMP",
        )
        .bind(key)
        .bind(value)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    } else {
        delete_setting(pool, key).await?;
    }
    Ok(())
}

async fn delete_setting(pool: &SqlitePool, key: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM app_settings WHERE setting_key = ?")
        .bind(key)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn normalize_secret(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_optional_setting(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
