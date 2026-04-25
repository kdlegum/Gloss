use crate::llm::LlmProvider;
use serde::Serialize;
use sqlx::SqlitePool;

const OPENAI_API_KEY: &str = "openai_api_key";
const GEMINI_API_KEY: &str = "gemini_api_key";
const DEEPSEEK_API_KEY: &str = "deepseek_api_key";
const ZAI_API_KEY: &str = "zai_api_key";
const AI_SETUP_COMPLETE: &str = "ai_setup_complete";

#[derive(Serialize)]
pub struct AiSettingsState {
    pub running_on_android: bool,
    pub setup_complete: bool,
    pub openai_api_key_set: bool,
    pub gemini_api_key_set: bool,
    pub deepseek_api_key_set: bool,
    pub zai_api_key_set: bool,
}

pub async fn state(pool: &SqlitePool) -> Result<AiSettingsState, String> {
    Ok(AiSettingsState {
        running_on_android: cfg!(target_os = "android"),
        setup_complete: get_setting(pool, AI_SETUP_COMPLETE)
            .await?
            .as_deref()
            .is_some_and(|value| value == "true"),
        openai_api_key_set: setting_has_value(pool, OPENAI_API_KEY).await?,
        gemini_api_key_set: setting_has_value(pool, GEMINI_API_KEY).await?,
        deepseek_api_key_set: setting_has_value(pool, DEEPSEEK_API_KEY).await?,
        zai_api_key_set: setting_has_value(pool, ZAI_API_KEY).await?,
    })
}

pub async fn api_key_for_provider(
    pool: &SqlitePool,
    provider: LlmProvider,
) -> Result<Option<String>, String> {
    match provider {
        LlmProvider::OpenAI => get_setting(pool, OPENAI_API_KEY).await,
        LlmProvider::Gemini => get_setting(pool, GEMINI_API_KEY).await,
        LlmProvider::DeepSeek => get_setting(pool, DEEPSEEK_API_KEY).await,
        LlmProvider::Zai => get_setting(pool, ZAI_API_KEY).await,
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
    if clear_openai_api_key {
        delete_setting(pool, OPENAI_API_KEY).await?;
    } else if let Some(api_key) = normalize_secret(openai_api_key) {
        set_setting(pool, OPENAI_API_KEY, &api_key).await?;
    }

    if clear_gemini_api_key {
        delete_setting(pool, GEMINI_API_KEY).await?;
    } else if let Some(api_key) = normalize_secret(gemini_api_key) {
        set_setting(pool, GEMINI_API_KEY, &api_key).await?;
    }

    if clear_deepseek_api_key {
        delete_setting(pool, DEEPSEEK_API_KEY).await?;
    } else if let Some(api_key) = normalize_secret(deepseek_api_key) {
        set_setting(pool, DEEPSEEK_API_KEY, &api_key).await?;
    }

    if clear_zai_api_key {
        delete_setting(pool, ZAI_API_KEY).await?;
    } else if let Some(api_key) = normalize_secret(zai_api_key) {
        set_setting(pool, ZAI_API_KEY, &api_key).await?;
    }

    if setup_complete {
        set_setting(pool, AI_SETUP_COMPLETE, "true").await?;
    }

    state(pool).await
}

async fn setting_has_value(pool: &SqlitePool, key: &str) -> Result<bool, String> {
    Ok(get_setting(pool, key)
        .await?
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty()))
}

async fn get_setting(pool: &SqlitePool, key: &str) -> Result<Option<String>, String> {
    sqlx::query_scalar("SELECT value FROM app_settings WHERE setting_key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())
}

async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO app_settings (setting_key, value, updated_at) VALUES (?, ?, CURRENT_TIMESTAMP) \
         ON CONFLICT(setting_key) DO UPDATE SET value = excluded.value, updated_at = CURRENT_TIMESTAMP",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;
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
