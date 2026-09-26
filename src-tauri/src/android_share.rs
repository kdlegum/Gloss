#[cfg(target_os = "android")]
mod platform {
    use serde::{Deserialize, Serialize};
    use std::sync::OnceLock;
    use tauri::plugin::{Builder, PluginHandle, TauriPlugin};

    const PLUGIN_IDENTIFIER: &str = "com.gloss.app";
    static PLUGIN_HANDLE: OnceLock<PluginHandle<tauri::Wry>> = OnceLock::new();

    pub fn init() -> TauriPlugin<tauri::Wry> {
        Builder::new("gloss-share")
            .setup(|_app, api| {
                let handle = api.register_android_plugin(PLUGIN_IDENTIFIER, "SharePlugin")?;
                let _ = PLUGIN_HANDLE.set(handle);
                Ok(())
            })
            .build()
    }

    fn handle() -> Result<&'static PluginHandle<tauri::Wry>, String> {
        PLUGIN_HANDLE
            .get()
            .ok_or_else(|| "Android share bridge not initialised".to_string())
    }

    #[derive(Serialize)]
    struct EmptyPayload {}

    #[derive(Deserialize)]
    pub struct PendingShare {
        pub uri: Option<String>,
        #[serde(rename = "fileName")]
        pub file_name: Option<String>,
    }

    pub fn consume_pending_share() -> Result<PendingShare, String> {
        handle()?
            .run_mobile_plugin("consumePendingShare", EmptyPayload {})
            .map_err(|e| e.to_string())
    }
}

#[cfg(not(target_os = "android"))]
mod platform {
    use tauri::plugin::{Builder, TauriPlugin};

    pub fn init() -> TauriPlugin<tauri::Wry> {
        Builder::new("gloss-share").build()
    }

    pub struct PendingShare {
        pub uri: Option<String>,
        pub file_name: Option<String>,
    }

    pub fn consume_pending_share() -> Result<PendingShare, String> {
        Ok(PendingShare {
            uri: None,
            file_name: None,
        })
    }
}

pub use platform::*;
