#[cfg(target_os = "android")]
mod platform {
    use serde::{Deserialize, Serialize};
    use std::sync::OnceLock;
    use tauri::plugin::{Builder, PluginHandle, TauriPlugin};

    const PLUGIN_IDENTIFIER: &str = "com.gloss.sync";
    static PLUGIN_HANDLE: OnceLock<PluginHandle<tauri::Wry>> = OnceLock::new();

    pub fn init() -> TauriPlugin<tauri::Wry> {
        Builder::new("gloss-local-sync")
            .setup(|_app, api| {
                let handle = api.register_android_plugin(PLUGIN_IDENTIFIER, "LocalSyncPlugin")?;
                let _ = PLUGIN_HANDLE.set(handle);
                Ok(())
            })
            .build()
    }

    fn handle() -> Result<&'static PluginHandle<tauri::Wry>, String> {
        PLUGIN_HANDLE
            .get()
            .ok_or_else(|| "Android local sync bridge was not initialised".to_string())
    }

    fn run<T: for<'de> Deserialize<'de>, P: Serialize>(
        command: &str,
        payload: P,
    ) -> Result<T, String> {
        handle()?
            .run_mobile_plugin(command, payload)
            .map_err(|e| e.to_string())
    }

    #[derive(Serialize)]
    struct EmptyPayload {}

    #[derive(Serialize)]
    struct TreePayload<'a> {
        #[serde(rename = "treeUri")]
        tree_uri: &'a str,
    }

    #[derive(Serialize)]
    struct RelativePayload<'a> {
        #[serde(rename = "treeUri")]
        tree_uri: &'a str,
        #[serde(rename = "relativePath")]
        relative_path: &'a str,
    }

    #[derive(Serialize)]
    struct TextPayload<'a> {
        #[serde(rename = "treeUri")]
        tree_uri: &'a str,
        #[serde(rename = "relativePath")]
        relative_path: &'a str,
        contents: &'a str,
    }

    #[derive(Serialize)]
    struct CopyPathToTreePayload<'a> {
        #[serde(rename = "treeUri")]
        tree_uri: &'a str,
        #[serde(rename = "sourcePath")]
        source_path: &'a str,
        #[serde(rename = "destRelativePath")]
        dest_relative_path: &'a str,
    }

    #[derive(Serialize)]
    struct CopyTreeToPathPayload<'a> {
        #[serde(rename = "treeUri")]
        tree_uri: &'a str,
        #[serde(rename = "sourceRelativePath")]
        source_relative_path: &'a str,
        #[serde(rename = "destPath")]
        dest_path: &'a str,
    }

    #[derive(Deserialize, Clone)]
    pub struct PickFolderResponse {
        #[serde(rename = "treeUri")]
        pub tree_uri: String,
        pub label: String,
    }

    #[derive(Deserialize)]
    struct ExistsResponse {
        exists: bool,
        #[serde(default, rename = "isDir")]
        is_dir: bool,
        #[serde(default)]
        size: Option<u64>,
    }

    #[derive(Deserialize)]
    struct TextResponse {
        contents: String,
    }

    #[derive(Deserialize, Clone)]
    pub struct TreeEntry {
        pub path: String,
        #[serde(rename = "isDir")]
        pub is_dir: bool,
        #[serde(default)]
        pub size: Option<u64>,
    }

    #[derive(Deserialize)]
    struct ListResponse {
        entries: Vec<TreeEntry>,
    }

    #[derive(Deserialize)]
    struct HashResponse {
        hash: String,
    }

    pub fn pick_folder() -> Result<PickFolderResponse, String> {
        run("pickSyncFolder", EmptyPayload {})
    }

    pub fn folder_ready(tree_uri: &str) -> Result<bool, String> {
        let response: ExistsResponse = run("folderReady", TreePayload { tree_uri })?;
        Ok(response.exists && response.is_dir)
    }

    pub fn ensure_dir(tree_uri: &str, relative_path: &str) -> Result<(), String> {
        let _: serde_json::Value = run(
            "ensureDir",
            RelativePayload {
                tree_uri,
                relative_path,
            },
        )?;
        Ok(())
    }

    pub fn exists(tree_uri: &str, relative_path: &str) -> Result<Option<(bool, u64)>, String> {
        let response: ExistsResponse = run(
            "exists",
            RelativePayload {
                tree_uri,
                relative_path,
            },
        )?;
        if response.exists {
            Ok(Some((response.is_dir, response.size.unwrap_or(0))))
        } else {
            Ok(None)
        }
    }

    pub fn read_text(tree_uri: &str, relative_path: &str) -> Result<String, String> {
        let response: TextResponse = run(
            "readTextFile",
            RelativePayload {
                tree_uri,
                relative_path,
            },
        )?;
        Ok(response.contents)
    }

    pub fn write_text_atomic(
        tree_uri: &str,
        relative_path: &str,
        contents: &str,
    ) -> Result<(), String> {
        let _: serde_json::Value = run(
            "writeTextFileAtomic",
            TextPayload {
                tree_uri,
                relative_path,
                contents,
            },
        )?;
        Ok(())
    }

    pub fn list_recursive(tree_uri: &str, relative_path: &str) -> Result<Vec<TreeEntry>, String> {
        let response: ListResponse = run(
            "listRecursive",
            RelativePayload {
                tree_uri,
                relative_path,
            },
        )?;
        Ok(response.entries)
    }

    pub fn delete_file(tree_uri: &str, relative_path: &str) -> Result<(), String> {
        let _: serde_json::Value = run(
            "deleteFile",
            RelativePayload {
                tree_uri,
                relative_path,
            },
        )?;
        Ok(())
    }

    pub fn copy_path_to_tree(
        tree_uri: &str,
        source_path: &str,
        dest_relative_path: &str,
    ) -> Result<(), String> {
        let _: serde_json::Value = run(
            "copyPathToTree",
            CopyPathToTreePayload {
                tree_uri,
                source_path,
                dest_relative_path,
            },
        )?;
        Ok(())
    }

    pub fn copy_tree_to_path(
        tree_uri: &str,
        source_relative_path: &str,
        dest_path: &str,
    ) -> Result<(), String> {
        let _: serde_json::Value = run(
            "copyTreeToPath",
            CopyTreeToPathPayload {
                tree_uri,
                source_relative_path,
                dest_path,
            },
        )?;
        Ok(())
    }

    pub fn hash_file(tree_uri: &str, relative_path: &str) -> Result<String, String> {
        let response: HashResponse = run(
            "hashFile",
            RelativePayload {
                tree_uri,
                relative_path,
            },
        )?;
        Ok(response.hash)
    }
}

#[cfg(target_os = "android")]
pub use platform::*;

#[cfg(not(target_os = "android"))]
pub fn init() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("gloss-local-sync").build()
}
