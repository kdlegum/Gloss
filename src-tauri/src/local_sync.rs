#[cfg(target_os = "android")]
use crate::android_sync;
use crate::settings;
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::Manager;
#[cfg(desktop)]
use tauri_plugin_dialog::DialogExt;
#[cfg(desktop)]
use tauri_plugin_fs::FilePath;

const SYNC_SCHEMA_VERSION: i64 = 1;
const MANIFEST_FILE: &str = ".gloss-sync.json";
const SNAPSHOT_RELATIVE_PATH: &str = "library/gloss.snapshot.db";
const LEASE_SECONDS: i64 = 10 * 60;
const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;
static LOCAL_DIRTY_EPOCH: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize)]
pub struct SyncState {
    pub setup_status: String,
    pub enabled: bool,
    pub folder_kind: String,
    pub folder_path: Option<String>,
    pub folder_label: Option<String>,
    pub running_on_android: bool,
    pub folder_ready: bool,
    pub device_id: String,
    pub dirty: bool,
    pub last_exported_revision: Option<i64>,
    pub last_imported_revision: Option<i64>,
    pub remote_snapshot: Option<SyncSnapshotInfo>,
    pub devices: Vec<SyncDevicePresence>,
    pub conflicts: Vec<SyncConflict>,
    pub missing_pdfs: Vec<String>,
    pub read_only: bool,
    pub active_writer: Option<SyncDevicePresence>,
    pub message: String,
}

#[derive(Serialize)]
pub struct AutoSyncResult {
    pub action: String,
    pub message: Option<String>,
    pub state: SyncState,
}

#[derive(Serialize, Clone)]
pub struct SyncSnapshotInfo {
    pub revision: i64,
    pub hash: String,
    pub updated_at_unix: i64,
    pub updated_by: String,
    pub path: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SyncDevicePresence {
    pub device_id: String,
    pub device_name: String,
    pub active_writer: bool,
    pub last_seen_unix: i64,
    pub lease_expires_unix: i64,
}

#[derive(Serialize)]
pub struct SyncConflict {
    pub id: String,
    pub path: String,
    pub kind: String,
}

#[derive(Serialize, Deserialize)]
struct SyncManifest {
    schema_version: i64,
    library_id: String,
    current_revision: i64,
    snapshot_hash: Option<String>,
    updated_at_unix: i64,
    updated_by: Option<String>,
}

#[derive(Deserialize)]
struct SnapshotDocumentRow {
    file_path: String,
    title: String,
    document_mode: String,
}

#[derive(Clone)]
enum SyncFolder {
    Path(PathBuf),
    #[cfg(target_os = "android")]
    AndroidTree {
        tree_uri: String,
        label: String,
    },
}

#[derive(Clone)]
struct SyncFileEntry {
    path: String,
    is_dir: bool,
}

impl SyncFolder {
    fn from_local(local: &settings::LocalSettings) -> Option<Self> {
        #[cfg(target_os = "android")]
        {
            if local.sync_folder_kind.as_deref() == Some("android_tree") {
                let tree_uri = local
                    .sync_folder_tree_uri
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())?;
                return Some(Self::AndroidTree {
                    tree_uri: tree_uri.to_string(),
                    label: local
                        .sync_folder_label
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .unwrap_or("Gloss Sync")
                        .to_string(),
                });
            }
        }

        local
            .sync_folder_path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|path| Self::Path(PathBuf::from(path)))
    }

    fn from_last_local(local: &settings::LocalSettings) -> Option<Self> {
        #[cfg(target_os = "android")]
        {
            if let Some(tree_uri) = local
                .last_sync_folder_tree_uri
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                return Some(Self::AndroidTree {
                    tree_uri: tree_uri.to_string(),
                    label: local
                        .last_sync_folder_label
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .unwrap_or("Gloss Sync")
                        .to_string(),
                });
            }
        }

        local
            .last_sync_folder_path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|path| Self::Path(PathBuf::from(path)))
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Path(_) => "path",
            #[cfg(target_os = "android")]
            Self::AndroidTree { .. } => "android_tree",
        }
    }

    fn display_path(&self) -> String {
        match self {
            Self::Path(path) => path.to_string_lossy().to_string(),
            #[cfg(target_os = "android")]
            Self::AndroidTree { tree_uri, .. } => tree_uri.clone(),
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Path(path) => path.to_string_lossy().to_string(),
            #[cfg(target_os = "android")]
            Self::AndroidTree { label, .. } => label.clone(),
        }
    }

    fn is_ready(&self) -> Result<bool, String> {
        match self {
            Self::Path(path) => Ok(path.is_dir()),
            #[cfg(target_os = "android")]
            Self::AndroidTree { tree_uri, .. } => android_sync::folder_ready(tree_uri),
        }
    }

    fn ensure_dir(&self, relative: &str) -> Result<(), String> {
        match self {
            Self::Path(path) => {
                std::fs::create_dir_all(path.join(relative)).map_err(|e| e.to_string())
            }
            #[cfg(target_os = "android")]
            Self::AndroidTree { tree_uri, .. } => android_sync::ensure_dir(tree_uri, relative),
        }
    }

    fn file_exists(&self, relative: &str) -> Result<bool, String> {
        Ok(self.file_entry(relative)?.is_some())
    }

    fn file_entry(&self, relative: &str) -> Result<Option<SyncFileEntry>, String> {
        match self {
            Self::Path(path) => {
                let candidate = path.join(relative);
                match std::fs::metadata(&candidate) {
                    Ok(metadata) => Ok(Some(SyncFileEntry {
                        path: relative.to_string(),
                        is_dir: metadata.is_dir(),
                    })),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(err) => Err(err.to_string()),
                }
            }
            #[cfg(target_os = "android")]
            Self::AndroidTree { tree_uri, .. } => Ok(android_sync::exists(tree_uri, relative)?
                .map(|(is_dir, _)| SyncFileEntry {
                    path: relative.to_string(),
                    is_dir,
                })),
        }
    }

    fn read_text(&self, relative: &str) -> Result<Option<String>, String> {
        match self {
            Self::Path(path) => {
                let path = path.join(relative);
                match std::fs::read_to_string(path) {
                    Ok(contents) => Ok(Some(contents)),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(err) => Err(err.to_string()),
                }
            }
            #[cfg(target_os = "android")]
            Self::AndroidTree { tree_uri, .. } => {
                if !android_sync::exists(tree_uri, relative)?.is_some_and(|(is_dir, _)| !is_dir) {
                    return Ok(None);
                }
                android_sync::read_text(tree_uri, relative).map(Some)
            }
        }
    }

    fn write_text_atomic(&self, relative: &str, contents: &str) -> Result<(), String> {
        match self {
            Self::Path(path) => {
                let path = path.join(relative);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let tmp = sibling_tmp_path(&path);
                std::fs::write(&tmp, contents).map_err(|e| e.to_string())?;
                publish_path_file(&tmp, &path)
            }
            #[cfg(target_os = "android")]
            Self::AndroidTree { tree_uri, .. } => {
                android_sync::write_text_atomic(tree_uri, relative, contents)
            }
        }
    }

    fn delete_file(&self, relative: &str) -> Result<(), String> {
        let Some(entry) = self.file_entry(relative)? else {
            return Ok(());
        };
        if entry.is_dir {
            return Err(format!("refusing to delete sync directory {relative}"));
        }
        match self {
            Self::Path(root) => {
                let path = root.join(relative);
                match std::fs::remove_file(&path) {
                    Ok(()) => Ok(()),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(err) => Err(err.to_string()),
                }
            }
            #[cfg(target_os = "android")]
            Self::AndroidTree { tree_uri, .. } => android_sync::delete_file(tree_uri, relative),
        }
    }

    fn list_recursive(&self, relative: &str) -> Result<Vec<SyncFileEntry>, String> {
        match self {
            Self::Path(root) => {
                let start = root.join(relative);
                let mut entries = Vec::new();
                collect_file_entries(root, &start, &mut entries)?;
                Ok(entries)
            }
            #[cfg(target_os = "android")]
            Self::AndroidTree { tree_uri, .. } => {
                if !relative.trim_matches('/').is_empty()
                    && !android_sync::exists(tree_uri, relative)?.is_some_and(|(is_dir, _)| is_dir)
                {
                    return Ok(Vec::new());
                }
                Ok(android_sync::list_recursive(tree_uri, relative)?
                    .into_iter()
                    .map(|entry| SyncFileEntry {
                        path: entry.path,
                        is_dir: entry.is_dir,
                    })
                    .collect())
            }
        }
    }

    fn copy_app_file_to_sync(
        &self,
        source: &Path,
        destination_relative: &str,
    ) -> Result<(), String> {
        match self {
            Self::Path(root) => copy_path_file_if_changed(source, &root.join(destination_relative)),
            #[cfg(target_os = "android")]
            Self::AndroidTree { tree_uri, .. } => {
                let source_len = source.metadata().map_err(|e| e.to_string())?.len();
                if android_sync::exists(tree_uri, destination_relative)?
                    .is_some_and(|(is_dir, size)| !is_dir && size == source_len)
                {
                    return Ok(());
                }
                android_sync::copy_path_to_tree(
                    tree_uri,
                    &source.to_string_lossy(),
                    destination_relative,
                )
            }
        }
    }

    fn publish_app_file_to_sync(
        &self,
        source: &Path,
        destination_relative: &str,
    ) -> Result<(), String> {
        match self {
            Self::Path(root) => {
                let destination = root.join(destination_relative);
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let tmp = sibling_tmp_path(&destination);
                std::fs::copy(source, &tmp).map_err(|e| e.to_string())?;
                publish_path_file(&tmp, &destination)
            }
            #[cfg(target_os = "android")]
            Self::AndroidTree { tree_uri, .. } => android_sync::copy_path_to_tree(
                tree_uri,
                &source.to_string_lossy(),
                destination_relative,
            ),
        }
    }

    fn copy_sync_file_to_app(
        &self,
        source_relative: &str,
        destination: &Path,
    ) -> Result<(), String> {
        match self {
            Self::Path(root) => copy_path_file_if_changed(&root.join(source_relative), destination),
            #[cfg(target_os = "android")]
            Self::AndroidTree { tree_uri, .. } => android_sync::copy_tree_to_path(
                tree_uri,
                source_relative,
                &destination.to_string_lossy(),
            ),
        }
    }

    fn hash_file(&self, relative: &str) -> Result<String, String> {
        match self {
            Self::Path(root) => hash_path_file(&root.join(relative)),
            #[cfg(target_os = "android")]
            Self::AndroidTree { tree_uri, .. } => android_sync::hash_file(tree_uri, relative),
        }
    }

    fn snapshot_display_path(&self) -> String {
        match self {
            Self::Path(root) => root
                .join(SNAPSHOT_RELATIVE_PATH)
                .to_string_lossy()
                .to_string(),
            #[cfg(target_os = "android")]
            Self::AndroidTree { label, .. } => format!("{label}/{SNAPSHOT_RELATIVE_PATH}"),
        }
    }
}

pub fn mark_dirty() {
    LOCAL_DIRTY_EPOCH.fetch_add(1, Ordering::SeqCst);
    if let Err(err) = settings::update_local_settings(|local| {
        if SyncFolder::from_local(local).is_some() {
            local.sync_dirty = true;
        }
    }) {
        log::warn!(target: "gloss_lib::sync", "failed to mark sync dirty: {err}");
    }
}

pub fn guard_write() -> Result<(), String> {
    let local = settings::read_local_settings()?;
    let Some(folder) = SyncFolder::from_local(&local) else {
        return Ok(());
    };
    if !folder.is_ready()? {
        return Ok(());
    }
    let device_id = local
        .device_id
        .ok_or_else(|| "local device id was not initialised".to_string())?;
    let now = now_unix();
    if let Some(writer) = read_devices(&folder)?.into_iter().find(|device| {
        device.device_id != device_id && device.active_writer && device.lease_expires_unix > now
    }) {
        return Err(format!(
            "{} is currently the active Gloss editor. Open Local sync and take the editing lease before changing this library.",
            writer.device_name
        ));
    }
    Ok(())
}

pub async fn get_sync_state(app: tauri::AppHandle, pool: &SqlitePool) -> Result<SyncState, String> {
    let mut local = settings::read_local_settings()?;
    let (fallback_folder_path, fallback_last_folder_path) =
        settings::sync_folder_fallback(pool).await?;
    let mut local_changed = false;
    if local
        .sync_folder_path
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        if let Some(folder_path) = fallback_folder_path.clone() {
            local.sync_folder_path = Some(folder_path);
            local_changed = true;
        }
    }
    if local
        .last_sync_folder_path
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        if let Some(folder_path) = fallback_last_folder_path
            .clone()
            .or_else(|| fallback_folder_path.clone())
        {
            local.last_sync_folder_path = Some(folder_path);
            local_changed = true;
        }
    }
    if local_changed {
        if let Err(err) = settings::write_local_settings(&local) {
            log::warn!(
                target: "gloss_lib::sync",
                "failed to restore local sync settings from fallback: {}",
                err
            );
        }
    }

    let device_id = match local
        .device_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    {
        Some(device_id) => device_id,
        None => {
            let sync_folder_path = local.sync_folder_path.clone();
            let last_sync_folder_path = local.last_sync_folder_path.clone();
            local = settings::update_local_settings(|stored| {
                if stored
                    .sync_folder_path
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    stored.sync_folder_path = sync_folder_path.clone();
                }
                if stored
                    .last_sync_folder_path
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    stored.last_sync_folder_path = last_sync_folder_path.clone();
                }
            })?;
            local
                .device_id
                .clone()
                .ok_or_else(|| "local device id was not initialised".to_string())?
        }
    };

    let Some(folder) = SyncFolder::from_local(&local) else {
        let last_folder = SyncFolder::from_last_local(&local);
        return Ok(SyncState {
            setup_status: "not_configured".to_string(),
            enabled: false,
            folder_kind: last_folder
                .as_ref()
                .map(SyncFolder::kind)
                .unwrap_or("path")
                .to_string(),
            folder_path: last_folder.as_ref().map(SyncFolder::display_path),
            folder_label: last_folder.as_ref().map(SyncFolder::label),
            running_on_android: cfg!(target_os = "android"),
            folder_ready: false,
            device_id,
            dirty: local.sync_dirty,
            last_exported_revision: local.last_exported_revision,
            last_imported_revision: local.last_imported_revision,
            remote_snapshot: None,
            devices: Vec::new(),
            conflicts: Vec::new(),
            missing_pdfs: Vec::new(),
            read_only: false,
            active_writer: None,
            message: "Choose a local folder that Syncthing will share between devices.".to_string(),
        });
    };

    let folder_ready = folder.is_ready()?;
    let manifest = if folder_ready {
        read_manifest(&folder)?
    } else {
        None
    };
    if folder_ready {
        if let Some(manifest) = manifest.as_ref() {
            if try_adopt_matching_remote_revision(&app, pool, &folder, manifest).await? {
                local = settings::update_local_settings(|_| {})?;
            }
        }
    }
    let devices = if folder_ready {
        read_devices(&folder)?
    } else {
        Vec::new()
    };
    let conflicts = if folder_ready {
        detect_conflicts(&folder)?
    } else {
        Vec::new()
    };
    let missing_pdfs = if folder_ready {
        missing_sync_pdfs_for_current_db(&folder, pool).await?
    } else {
        Vec::new()
    };

    let now = now_unix();
    let active_writer = devices
        .iter()
        .find(|device| {
            device.device_id != device_id && device.active_writer && device.lease_expires_unix > now
        })
        .cloned();
    let remote_snapshot = manifest.as_ref().and_then(|manifest| {
        manifest
            .snapshot_hash
            .as_ref()
            .map(|hash| SyncSnapshotInfo {
                revision: manifest.current_revision,
                hash: hash.clone(),
                updated_at_unix: manifest.updated_at_unix,
                updated_by: manifest.updated_by.clone().unwrap_or_default(),
                path: folder.snapshot_display_path(),
            })
    });
    let local_tip = local
        .last_exported_revision
        .max(local.last_imported_revision)
        .unwrap_or(0);
    let remote_newer = remote_snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.revision > local_tip);

    let setup_status = if !folder_ready {
        "folder_missing"
    } else if remote_snapshot.is_some() {
        "ready"
    } else {
        "initialised"
    };
    let message = sync_message(
        setup_status,
        local.sync_dirty,
        remote_newer,
        active_writer.as_ref(),
        conflicts.len(),
        missing_pdfs.len(),
    );

    // Keep stale temp outputs from looking like app problems after a crash.
    if folder_ready {
        let _ = write_stignore(&folder);
        let _ = app.path().app_data_dir();
    }

    Ok(SyncState {
        setup_status: setup_status.to_string(),
        enabled: true,
        folder_kind: folder.kind().to_string(),
        folder_path: Some(folder.display_path()),
        folder_label: Some(folder.label()),
        running_on_android: cfg!(target_os = "android"),
        folder_ready,
        device_id,
        dirty: local.sync_dirty,
        last_exported_revision: local.last_exported_revision,
        last_imported_revision: local.last_imported_revision,
        remote_snapshot,
        devices,
        conflicts,
        missing_pdfs,
        read_only: active_writer.is_some(),
        active_writer,
        message,
    })
}

pub async fn choose_sync_folder(app: tauri::AppHandle) -> Result<String, String> {
    #[cfg(desktop)]
    {
        let picked = app.dialog().file().blocking_pick_folder();
        return store_picked_sync_folder(picked);
    }

    #[cfg(target_os = "android")]
    {
        let _ = app;
        let picked = android_sync::pick_folder()?;
        settings::update_local_settings(|local| {
            local.sync_folder_kind = Some("android_tree".to_string());
            local.sync_folder_tree_uri = Some(picked.tree_uri.clone());
            local.last_sync_folder_tree_uri = Some(picked.tree_uri.clone());
            local.sync_folder_label = Some(picked.label.clone());
            local.last_sync_folder_label = Some(picked.label.clone());
            local.sync_folder_path = None;
        })?;
        Ok(picked.tree_uri)
    }

    #[cfg(all(mobile, not(target_os = "android")))]
    {
        let _ = app;
        Err("Folder picking is not available on this mobile platform yet.".to_string())
    }
}

#[cfg(desktop)]
fn store_picked_sync_folder(picked: Option<FilePath>) -> Result<String, String> {
    match picked {
        Some(FilePath::Path(path)) => {
            let folder = normalize_folder_path(&path.to_string_lossy())?;
            settings::update_local_settings(|local| {
                let folder_path = folder.to_string_lossy().to_string();
                local.sync_folder_kind = Some("path".to_string());
                local.sync_folder_path = Some(folder_path.clone());
                local.last_sync_folder_path = Some(folder_path);
                local.sync_folder_tree_uri = None;
                local.sync_folder_label = None;
            })?;
            Ok(folder.to_string_lossy().to_string())
        }
        Some(FilePath::Url(url)) => Err(format!(
            "Gloss needs a filesystem folder path for Syncthing, but got {url}"
        )),
        None => Err("cancelled".to_string()),
    }
}

pub async fn enable_sync_folder(
    app: tauri::AppHandle,
    pool: &SqlitePool,
    folder_path: String,
) -> Result<SyncState, String> {
    let folder = folder_from_enable_input(&folder_path)?;
    initialise_sync_folder(&folder)?;

    let had_snapshot = read_manifest(&folder)?
        .and_then(|manifest| manifest.snapshot_hash)
        .is_some();

    settings::update_local_settings(|local| match &folder {
        SyncFolder::Path(path) => {
            let folder_path = path.to_string_lossy().to_string();
            local.sync_folder_kind = Some("path".to_string());
            local.sync_folder_path = Some(folder_path.clone());
            local.last_sync_folder_path = Some(folder_path);
            local.sync_folder_tree_uri = None;
            local.sync_folder_label = None;
        }
        #[cfg(target_os = "android")]
        SyncFolder::AndroidTree { tree_uri, label } => {
            local.sync_folder_kind = Some("android_tree".to_string());
            local.sync_folder_tree_uri = Some(tree_uri.clone());
            local.last_sync_folder_tree_uri = Some(tree_uri.clone());
            local.sync_folder_label = Some(label.clone());
            local.last_sync_folder_label = Some(label.clone());
            local.sync_folder_path = None;
        }
    })?;
    save_sync_folder_fallback_for_backend(pool, &folder).await?;

    if !had_snapshot {
        sync_now(app.clone(), pool, true).await?;
    } else if let Some(manifest) = read_manifest(&folder)? {
        let _ = try_adopt_matching_remote_revision(&app, pool, &folder, &manifest).await?;
    }

    get_sync_state(app, pool).await
}

pub async fn disable_sync(app: tauri::AppHandle, pool: &SqlitePool) -> Result<SyncState, String> {
    let local = settings::update_local_settings(|local| {
        local.sync_folder_kind = None;
        local.sync_folder_path = None;
        local.sync_folder_tree_uri = None;
        local.sync_folder_label = None;
        local.sync_dirty = false;
        local.last_exported_revision = None;
        local.last_imported_revision = None;
    })?;
    settings::save_sync_folder_fallback(pool, None, local.last_sync_folder_path.as_deref()).await?;
    get_sync_state(app, pool).await
}

pub async fn take_sync_editing_lease(
    app: tauri::AppHandle,
    pool: &SqlitePool,
) -> Result<SyncState, String> {
    let folder = configured_folder()?;
    initialise_sync_folder(&folder)?;
    claim_editing_lease(&folder)?;
    get_sync_state(app, pool).await
}

pub async fn sync_now(
    app: tauri::AppHandle,
    pool: &SqlitePool,
    force: bool,
) -> Result<SyncState, String> {
    let folder = configured_folder()?;
    initialise_sync_folder(&folder)?;
    let conflicts = detect_conflicts(&folder)?;
    if !conflicts.is_empty() && !force {
        return Err("Syncthing conflict files are present. Resolve them before publishing a new Gloss snapshot.".to_string());
    }

    let manifest = ensure_manifest(&folder)?;
    let _ = try_adopt_matching_remote_revision(&app, pool, &folder, &manifest).await?;
    let local = settings::read_local_settings()?;
    let local_tip = local
        .last_exported_revision
        .max(local.last_imported_revision)
        .unwrap_or(0);
    if local.sync_dirty && manifest.current_revision > local_tip && !force {
        create_local_conflict_backup(&app, &folder, pool).await?;
        return Err("The sync folder has a newer snapshot and this device has local edits. A recovery DB was saved under conflicts/; import or resolve before syncing.".to_string());
    }

    let dirty_epoch_before_export = LOCAL_DIRTY_EPOCH.load(Ordering::SeqCst);
    normalize_document_pdfs(&app, pool).await?;
    copy_local_pdfs_to_sync(&app, &folder, pool).await?;
    let revision = std::cmp::max(manifest.current_revision, local_tip) + 1;
    export_snapshot(&app, &folder, pool, revision).await?;
    if force {
        claim_editing_lease(&folder)?;
    } else {
        write_device_presence(&folder, true)?;
    }

    let dirty_epoch_after_export = LOCAL_DIRTY_EPOCH.load(Ordering::SeqCst);
    settings::update_local_settings(|local| {
        local.sync_dirty = dirty_epoch_after_export != dirty_epoch_before_export;
        local.last_exported_revision = Some(revision);
    })?;

    get_sync_state(app, pool).await
}

pub async fn import_latest_sync_snapshot(
    app: tauri::AppHandle,
    pool: &SqlitePool,
    ignore_conflicts: bool,
) -> Result<SyncState, String> {
    let folder = configured_folder()?;
    initialise_sync_folder(&folder)?;
    let conflicts = detect_conflicts(&folder)?;
    if !conflicts.is_empty() && !ignore_conflicts {
        return Err(
            "Syncthing conflict files are present. Resolve them before importing.".to_string(),
        );
    }

    let manifest = ensure_manifest(&folder)?;
    if manifest.snapshot_hash.is_none() {
        return Err("This Gloss sync folder does not have a snapshot yet.".to_string());
    }
    let local = settings::read_local_settings()?;
    let local_tip = local
        .last_exported_revision
        .max(local.last_imported_revision)
        .unwrap_or(0);
    if local.sync_dirty && manifest.current_revision > local_tip {
        create_local_conflict_backup(&app, &folder, pool).await?;
        return Err("This device has unsynced edits and the sync folder is newer. A recovery DB was saved under conflicts/.".to_string());
    }
    if !ignore_conflicts && !local.sync_dirty && manifest.current_revision <= local_tip {
        return get_sync_state(app, pool).await;
    }

    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let staged_path = data_dir.join("gloss-sync-import.tmp.db");
    if staged_path.exists() {
        std::fs::remove_file(&staged_path).map_err(|e| e.to_string())?;
    }
    folder
        .copy_sync_file_to_app(SNAPSHOT_RELATIVE_PATH, &staged_path)
        .map_err(|e| format!("failed to stage sync snapshot: {e}"))?;

    let staged_url = format!("sqlite://{}?mode=rwc", staged_path.display());
    let staged_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&staged_url)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::migrate!("./migrations")
        .run(&staged_pool)
        .await
        .map_err(|e| e.to_string())?;

    let snapshot_docs = load_snapshot_document_rows(&staged_pool).await?;
    let missing = missing_pdfs_for_snapshot_rows(&folder, &snapshot_docs)?;
    if !missing.is_empty() {
        staged_pool.close().await;
        let _ = std::fs::remove_file(&staged_path);
        return Err(format!(
            "Waiting for Syncthing to finish: missing {} PDF file(s): {}",
            missing.len(),
            missing.join(", ")
        ));
    }

    copy_sync_pdfs_to_local(&app, &folder, &snapshot_docs)?;
    staged_pool.close().await;
    crate::replace_main_database_from_staged_import(pool, &staged_path).await?;
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(|e| e.to_string())?;
    settings::init_local_store(&data_dir, pool).await?;
    let _ = std::fs::remove_file(&staged_path);

    settings::update_local_settings(|local| {
        local.sync_dirty = false;
        local.last_imported_revision = Some(manifest.current_revision);
    })?;
    save_sync_folder_fallback_for_backend(pool, &folder).await?;
    write_device_presence(&folder, false)?;
    get_sync_state(app, pool).await
}

pub async fn resolve_sync_conflict(
    app: tauri::AppHandle,
    pool: &SqlitePool,
    resolution: String,
) -> Result<SyncState, String> {
    let folder = configured_folder()?;
    initialise_sync_folder(&folder)?;
    match resolution.trim() {
        "keep_local" => {
            sync_now(app.clone(), pool, true).await?;
            clear_resolved_conflicts(&folder)?;
            get_sync_state(app, pool).await
        }
        "use_remote" => {
            let was_dirty = settings::read_local_settings()?.sync_dirty;
            settings::update_local_settings(|local| {
                local.sync_dirty = false;
            })?;
            match import_latest_sync_snapshot(app.clone(), pool, true).await {
                Ok(_) => {
                    clear_resolved_conflicts(&folder)?;
                    get_sync_state(app, pool).await
                }
                Err(err) => {
                    if was_dirty {
                        let _ = settings::update_local_settings(|local| {
                            local.sync_dirty = true;
                        });
                    }
                    Err(err)
                }
            }
        }
        other => Err(format!("unknown sync conflict resolution {other:?}")),
    }
}

pub async fn auto_sync_once(
    app: tauri::AppHandle,
    pool: &SqlitePool,
    allow_import: bool,
) -> Result<AutoSyncResult, String> {
    let local = settings::read_local_settings()?;
    let Some(folder) = SyncFolder::from_local(&local) else {
        return auto_sync_result(app, pool, "idle", None).await;
    };
    if !folder.is_ready()? {
        return auto_sync_result(app, pool, "idle", None).await;
    }

    initialise_sync_folder(&folder)?;
    let conflicts = detect_conflicts(&folder)?;
    if !conflicts.is_empty() {
        return auto_sync_result(
            app,
            pool,
            "blocked",
            Some("Auto sync is paused until sync conflicts are resolved.".to_string()),
        )
        .await;
    }

    let manifest = ensure_manifest(&folder)?;
    let local = settings::read_local_settings()?;
    let local_tip = local
        .last_exported_revision
        .max(local.last_imported_revision)
        .unwrap_or(0);
    let remote_revision = if manifest.snapshot_hash.is_some() {
        Some(manifest.current_revision)
    } else {
        None
    };
    let remote_newer = remote_revision.is_some_and(|revision| revision > local_tip);

    if remote_newer && local.sync_dirty {
        return auto_sync_result(
            app,
            pool,
            "blocked",
            Some(
                "Auto sync is paused because this device and the sync folder both have newer changes."
                    .to_string(),
            ),
        )
        .await;
    }

    if remote_newer && !allow_import {
        return auto_sync_result(
            app,
            pool,
            "waiting",
            Some(
                "A newer synced snapshot is ready; it will import from the library screen."
                    .to_string(),
            ),
        )
        .await;
    }

    if remote_newer {
        if local_tip == 0 && local_library_has_documents(pool).await? {
            return auto_sync_result(
                app,
                pool,
                "blocked",
                Some("Auto import is paused because this device already has a local library. Open Local sync to choose which copy to keep.".to_string()),
            )
            .await;
        }
        let state = import_latest_sync_snapshot(app, pool, false).await?;
        return Ok(AutoSyncResult {
            action: "imported".to_string(),
            message: Some(format!(
                "Imported synced revision {}.",
                state
                    .last_imported_revision
                    .or_else(|| state
                        .remote_snapshot
                        .as_ref()
                        .map(|snapshot| snapshot.revision))
                    .unwrap_or_default()
            )),
            state,
        });
    }

    if local.sync_dirty {
        let now = now_unix();
        let device_id = local
            .device_id
            .clone()
            .ok_or_else(|| "local device id was not initialised".to_string())?;
        if let Some(writer) = read_devices(&folder)?.into_iter().find(|device| {
            device.device_id != device_id && device.active_writer && device.lease_expires_unix > now
        }) {
            return auto_sync_result(
                app,
                pool,
                "waiting",
                Some(format!(
                    "Auto sync is waiting for {}'s editing lease to expire.",
                    writer.device_name
                )),
            )
            .await;
        }

        let state = sync_now(app, pool, false).await?;
        return Ok(AutoSyncResult {
            action: "exported".to_string(),
            message: Some(format!(
                "Published synced revision {}.",
                state
                    .remote_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.revision)
                    .unwrap_or_default()
            )),
            state,
        });
    }

    auto_sync_result(app, pool, "idle", None).await
}

async fn auto_sync_result(
    app: tauri::AppHandle,
    pool: &SqlitePool,
    action: &str,
    message: Option<String>,
) -> Result<AutoSyncResult, String> {
    Ok(AutoSyncResult {
        action: action.to_string(),
        message,
        state: get_sync_state(app, pool).await?,
    })
}

async fn save_sync_folder_fallback_for_backend(
    pool: &SqlitePool,
    folder: &SyncFolder,
) -> Result<(), String> {
    match folder {
        SyncFolder::Path(path) => {
            let folder_path = path.to_string_lossy().to_string();
            settings::save_sync_folder_fallback(pool, Some(&folder_path), Some(&folder_path)).await
        }
        #[cfg(target_os = "android")]
        SyncFolder::AndroidTree { .. } => {
            settings::save_sync_folder_fallback(pool, None, None).await
        }
    }
}

async fn export_snapshot(
    app: &tauri::AppHandle,
    folder: &SyncFolder,
    pool: &SqlitePool,
    revision: i64,
) -> Result<(), String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let tmp_db = data_dir.join(format!(
        "gloss-sync-export-{}.tmp.db",
        settings::local_device_id()?
    ));
    if tmp_db.exists() {
        std::fs::remove_file(&tmp_db).map_err(|e| e.to_string())?;
    }

    sqlx::query("VACUUM INTO ?")
        .bind(tmp_db.to_string_lossy().to_string())
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;

    let tmp_url = format!("sqlite://{}?mode=rwc", tmp_db.display());
    let tmp_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&tmp_url)
        .await
        .map_err(|e| e.to_string())?;
    for key in settings::LOCAL_ONLY_DB_KEYS {
        sqlx::query("DELETE FROM app_settings WHERE setting_key = ?")
            .bind(key)
            .execute(&tmp_pool)
            .await
            .map_err(|e| e.to_string())?;
    }
    tmp_pool.close().await;

    folder.publish_app_file_to_sync(&tmp_db, SNAPSHOT_RELATIVE_PATH)?;
    let hash = folder.hash_file(SNAPSHOT_RELATIVE_PATH)?;
    let _ = std::fs::remove_file(&tmp_db);
    let mut manifest = ensure_manifest(folder)?;
    manifest.current_revision = revision;
    manifest.snapshot_hash = Some(hash);
    manifest.updated_at_unix = now_unix();
    manifest.updated_by = Some(settings::local_device_id()?);
    write_manifest(folder, &manifest)?;
    info_snapshot_export(folder, revision);
    Ok(())
}

async fn try_adopt_matching_remote_revision(
    app: &tauri::AppHandle,
    pool: &SqlitePool,
    folder: &SyncFolder,
    manifest: &SyncManifest,
) -> Result<bool, String> {
    let Some(remote_hash) = manifest.snapshot_hash.as_deref() else {
        return Ok(false);
    };

    let local = settings::read_local_settings()?;
    let local_tip = local
        .last_exported_revision
        .max(local.last_imported_revision)
        .unwrap_or(0);
    if manifest.current_revision <= local_tip || local.sync_dirty {
        return Ok(false);
    }
    if !local_library_has_documents(pool).await? {
        return Ok(false);
    }

    let local_hash = comparable_local_snapshot_hash(app, pool).await?;
    let matches = if local_hash == remote_hash {
        true
    } else {
        match local_content_matches_remote_snapshot(app, pool, folder).await {
            Ok(matches) => matches,
            Err(err) => {
                log::warn!(
                    target: "gloss_lib::sync",
                    "failed to compare local database content with sync snapshot: {}",
                    err
                );
                false
            }
        }
    };
    if !matches {
        return Ok(false);
    }

    settings::update_local_settings(|local| {
        local.sync_dirty = false;
        local.last_imported_revision = Some(manifest.current_revision);
    })?;
    log::info!(
        target: "gloss_lib::sync",
        "adopted existing sync snapshot revision={} after local snapshot match",
        manifest.current_revision
    );
    Ok(true)
}

async fn local_content_matches_remote_snapshot(
    app: &tauri::AppHandle,
    pool: &SqlitePool,
    folder: &SyncFolder,
) -> Result<bool, String> {
    if !folder.file_exists(SNAPSHOT_RELATIVE_PATH)? {
        return Ok(false);
    }
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let snapshot_path = data_dir.join(format!(
        "gloss-sync-remote-compare-{}.tmp.db",
        settings::make_local_id("snapshot")
    ));
    if snapshot_path.exists() {
        std::fs::remove_file(&snapshot_path).map_err(|e| e.to_string())?;
    }
    folder.copy_sync_file_to_app(SNAPSHOT_RELATIVE_PATH, &snapshot_path)?;

    let snapshot_url = format!("sqlite://{}?mode=rwc", snapshot_path.display());
    let snapshot_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&snapshot_url)
        .await
        .map_err(|e| e.to_string())?;

    let local_hash = database_content_hash(pool).await;
    let snapshot_hash = database_content_hash(&snapshot_pool).await;
    snapshot_pool.close().await;
    let _ = std::fs::remove_file(&snapshot_path);

    Ok(local_hash? == snapshot_hash?)
}

async fn database_content_hash(pool: &SqlitePool) -> Result<String, String> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != '_sqlx_migrations' \
         ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    let mut hash = FNV_OFFSET;
    for table in tables {
        hash = fnv1a_update(hash, table.as_bytes());
        hash = fnv1a_update(hash, b"\x1e");

        let columns = table_columns(pool, &table).await?;
        for column in &columns {
            hash = fnv1a_update(hash, column.as_bytes());
            hash = fnv1a_update(hash, b"\x1f");
        }
        hash = fnv1a_update(hash, b"\x1e");

        let quoted_table = quote_sqlite_ident(&table);
        let row_expr = columns
            .iter()
            .map(|column| format!("quote({})", quote_sqlite_ident(column)))
            .collect::<Vec<_>>()
            .join(" || char(31) || ");
        let mut sql = format!("SELECT {row_expr} AS row_repr FROM {quoted_table}");
        if table == "app_settings" && columns.iter().any(|column| column == "setting_key") {
            let local_keys = settings::LOCAL_ONLY_DB_KEYS
                .iter()
                .map(|key| sqlite_string_literal(key))
                .collect::<Vec<_>>()
                .join(", ");
            sql.push_str(&format!(" WHERE setting_key NOT IN ({local_keys})"));
        }
        sql.push_str(" ORDER BY row_repr");

        let rows: Vec<String> = sqlx::query_scalar(&sql)
            .fetch_all(pool)
            .await
            .map_err(|e| e.to_string())?;
        for row in rows {
            hash = fnv1a_update(hash, row.as_bytes());
            hash = fnv1a_update(hash, b"\x1e");
        }
    }

    Ok(format!("{hash:016x}"))
}

async fn table_columns(pool: &SqlitePool, table: &str) -> Result<Vec<String>, String> {
    let sql = format!("PRAGMA table_info({})", quote_sqlite_ident(table));
    let rows = sqlx::query(&sql)
        .fetch_all(pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(|row| row.get("name")).collect())
}

async fn comparable_local_snapshot_hash(
    app: &tauri::AppHandle,
    pool: &SqlitePool,
) -> Result<String, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let tmp_db = data_dir.join(format!(
        "gloss-sync-compare-{}.tmp.db",
        settings::make_local_id("snapshot")
    ));
    if tmp_db.exists() {
        std::fs::remove_file(&tmp_db).map_err(|e| e.to_string())?;
    }

    let result = async {
        sqlx::query("VACUUM INTO ?")
            .bind(tmp_db.to_string_lossy().to_string())
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;

        let tmp_url = format!("sqlite://{}?mode=rwc", tmp_db.display());
        let tmp_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&tmp_url)
            .await
            .map_err(|e| e.to_string())?;
        for key in settings::LOCAL_ONLY_DB_KEYS {
            sqlx::query("DELETE FROM app_settings WHERE setting_key = ?")
                .bind(key)
                .execute(&tmp_pool)
                .await
                .map_err(|e| e.to_string())?;
        }
        tmp_pool.close().await;

        hash_path_file(&tmp_db)
    }
    .await;

    let _ = std::fs::remove_file(&tmp_db);
    result
}

async fn normalize_document_pdfs(app: &tauri::AppHandle, pool: &SqlitePool) -> Result<(), String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let pdfs_dir = data_dir.join("pdfs");
    std::fs::create_dir_all(&pdfs_dir).map_err(|e| e.to_string())?;
    let rows = sqlx::query(
        "SELECT id, file_path, document_mode, document_sync_id, original_file_name FROM source_documents ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    for row in rows {
        let id: i64 = row.get("id");
        let file_path: String = row.get("file_path");
        let document_mode: String = row.get("document_mode");
        if document_mode.eq_ignore_ascii_case("notebook") {
            continue;
        }
        let document_sync_id = row
            .get::<Option<String>, _>("document_sync_id")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| settings::make_local_id("doc"));
        let original_file_name = row
            .get::<Option<String>, _>("original_file_name")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                Path::new(&file_path)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
            });
        let desired_relative = format!("pdfs/{document_sync_id}.pdf");
        if file_path != desired_relative {
            let old_abs = data_dir.join(&file_path);
            let new_abs = data_dir.join(&desired_relative);
            if old_abs.exists() && !new_abs.exists() {
                std::fs::copy(&old_abs, &new_abs).map_err(|e| e.to_string())?;
            }
        }
        sqlx::query(
            "UPDATE source_documents SET document_sync_id = ?, original_file_name = ?, file_path = ? WHERE id = ?",
        )
        .bind(&document_sync_id)
        .bind(original_file_name.as_deref())
        .bind(&desired_relative)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn copy_local_pdfs_to_sync(
    app: &tauri::AppHandle,
    folder: &SyncFolder,
    pool: &SqlitePool,
) -> Result<(), String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let rows =
        sqlx::query("SELECT title, file_path, document_mode FROM source_documents ORDER BY id")
            .fetch_all(pool)
            .await
            .map_err(|e| e.to_string())?;
    for row in rows {
        let title: String = row.get("title");
        let file_path: String = row.get("file_path");
        let document_mode: String = row.get("document_mode");
        if document_mode.eq_ignore_ascii_case("notebook") {
            continue;
        }
        let source = data_dir.join(&file_path);
        if !source.exists() {
            return Err(format!("PDF for {title:?} is missing locally: {file_path}"));
        }
        folder.copy_app_file_to_sync(&source, &file_path)?;
    }
    Ok(())
}

fn copy_sync_pdfs_to_local(
    app: &tauri::AppHandle,
    folder: &SyncFolder,
    docs: &[SnapshotDocumentRow],
) -> Result<(), String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    for doc in docs {
        if doc.document_mode.eq_ignore_ascii_case("notebook") {
            continue;
        }
        let destination = data_dir.join(&doc.file_path);
        folder.copy_sync_file_to_app(&doc.file_path, &destination)?;
    }
    Ok(())
}

async fn load_snapshot_document_rows(
    pool: &SqlitePool,
) -> Result<Vec<SnapshotDocumentRow>, String> {
    let rows =
        sqlx::query("SELECT title, file_path, document_mode FROM source_documents ORDER BY id")
            .fetch_all(pool)
            .await
            .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|row| SnapshotDocumentRow {
            title: row.get("title"),
            file_path: row.get("file_path"),
            document_mode: row.get("document_mode"),
        })
        .collect())
}

fn missing_pdfs_for_snapshot_rows(
    folder: &SyncFolder,
    docs: &[SnapshotDocumentRow],
) -> Result<Vec<String>, String> {
    let mut missing = Vec::new();
    for doc in docs {
        if doc.document_mode.eq_ignore_ascii_case("notebook") {
            continue;
        }
        if !folder.file_exists(&doc.file_path)? {
            missing.push(format!("{} ({})", doc.title, doc.file_path));
        }
    }
    Ok(missing)
}

async fn missing_sync_pdfs_for_current_db(
    folder: &SyncFolder,
    pool: &SqlitePool,
) -> Result<Vec<String>, String> {
    let docs = load_snapshot_document_rows(pool).await?;
    missing_pdfs_for_snapshot_rows(folder, &docs)
}

async fn local_library_has_documents(pool: &SqlitePool) -> Result<bool, String> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM source_documents")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(count > 0)
}

async fn create_local_conflict_backup(
    app: &tauri::AppHandle,
    folder: &SyncFolder,
    pool: &SqlitePool,
) -> Result<(), String> {
    folder.ensure_dir("conflicts")?;
    let device_id = settings::local_device_id()?;
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let backup = data_dir.join(format!("{device_id}-{}.conflict.tmp.db", now_unix()));
    if backup.exists() {
        std::fs::remove_file(&backup).map_err(|e| e.to_string())?;
    }
    sqlx::query("VACUUM INTO ?")
        .bind(backup.to_string_lossy().to_string())
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    let relative = format!("conflicts/{device_id}-{}.db", now_unix());
    folder.copy_app_file_to_sync(&backup, &relative)?;
    let _ = std::fs::remove_file(&backup);
    Ok(())
}

fn configured_folder() -> Result<SyncFolder, String> {
    let local = settings::read_local_settings()?;
    SyncFolder::from_local(&local)
        .or_else(|| SyncFolder::from_last_local(&local))
        .ok_or_else(|| "Local sync is not configured yet.".to_string())
}

fn normalize_folder_path(path: &str) -> Result<PathBuf, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("sync folder path was empty".to_string());
    }
    Ok(PathBuf::from(path))
}

fn folder_from_enable_input(input: &str) -> Result<SyncFolder, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("sync folder path was empty".to_string());
    }

    #[cfg(target_os = "android")]
    {
        if trimmed.starts_with("content://") {
            let local = settings::read_local_settings()?;
            let label = local
                .sync_folder_label
                .or(local.last_sync_folder_label)
                .unwrap_or_else(|| "Gloss Sync".to_string());
            return Ok(SyncFolder::AndroidTree {
                tree_uri: trimmed.to_string(),
                label,
            });
        }
        let local = settings::read_local_settings()?;
        if let Some(folder) = SyncFolder::from_local(&local) {
            if folder.kind() == "android_tree"
                && (trimmed == folder.display_path() || trimmed == folder.label())
            {
                return Ok(folder);
            }
        }
    }

    normalize_folder_path(trimmed).map(SyncFolder::Path)
}

fn initialise_sync_folder(folder: &SyncFolder) -> Result<(), String> {
    match folder {
        SyncFolder::Path(path) => {
            std::fs::create_dir_all(path).map_err(|e| e.to_string())?;
        }
        #[cfg(target_os = "android")]
        SyncFolder::AndroidTree { .. } => {}
    }
    folder.ensure_dir("library")?;
    folder.ensure_dir("pdfs")?;
    folder.ensure_dir("devices")?;
    folder.ensure_dir("conflicts")?;
    write_stignore(folder)?;
    if read_manifest(folder)?.is_none() {
        write_manifest(
            folder,
            &SyncManifest {
                schema_version: SYNC_SCHEMA_VERSION,
                library_id: settings::make_local_id("library"),
                current_revision: 0,
                snapshot_hash: None,
                updated_at_unix: now_unix(),
                updated_by: None,
            },
        )?;
    }
    Ok(())
}

fn ensure_manifest(folder: &SyncFolder) -> Result<SyncManifest, String> {
    initialise_sync_folder(folder)?;
    read_manifest(folder)?.ok_or_else(|| "sync manifest is missing".to_string())
}

fn read_manifest(folder: &SyncFolder) -> Result<Option<SyncManifest>, String> {
    let Some(contents) = folder.read_text(MANIFEST_FILE)? else {
        return Ok(None);
    };
    serde_json::from_str(&contents).map(Some).map_err(|e| {
        format!(
            "failed to read Gloss sync manifest at {}: {e}",
            folder.label()
        )
    })
}

fn write_manifest(folder: &SyncFolder, manifest: &SyncManifest) -> Result<(), String> {
    let contents = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
    folder.write_text_atomic(MANIFEST_FILE, &contents)
}

fn write_stignore(folder: &SyncFolder) -> Result<(), String> {
    let contents = "\
// Managed by Gloss. Syncthing keeps .stignore local to each device.
(?d)library/*.tmp
(?d)library/*.tmp.db
(?d)library/*.db-wal
(?d)library/*.db-shm
(?d)library/*.db-journal
(?d)pdfs/*.tmp
(?d)*.tmp
(?d)*.part
(?d).gloss-sync.tmp.json
.DS_Store
Thumbs.db
desktop.ini
";
    folder.write_text_atomic(".stignore", contents)
}

fn read_devices(folder: &SyncFolder) -> Result<Vec<SyncDevicePresence>, String> {
    let mut devices = Vec::new();
    for entry in folder.list_recursive("devices")? {
        if entry.is_dir || !entry.path.ends_with(".json") {
            continue;
        }
        let Some(contents) = folder.read_text(&entry.path)? else {
            continue;
        };
        if let Ok(device) = serde_json::from_str::<SyncDevicePresence>(&contents) {
            devices.push(device);
        }
    }
    devices.sort_by(|a, b| b.last_seen_unix.cmp(&a.last_seen_unix));
    Ok(devices)
}

fn claim_editing_lease(folder: &SyncFolder) -> Result<(), String> {
    let device_id = settings::local_device_id()?;
    clear_other_editing_leases(folder, &device_id)?;
    write_device_presence(folder, true)
}

fn clear_other_editing_leases(folder: &SyncFolder, current_device_id: &str) -> Result<(), String> {
    let now = now_unix();
    for entry in folder.list_recursive("devices")? {
        if entry.is_dir || !entry.path.ends_with(".json") {
            continue;
        }

        let Some(contents) = folder.read_text(&entry.path)? else {
            continue;
        };
        let Ok(mut device) = serde_json::from_str::<SyncDevicePresence>(&contents) else {
            continue;
        };
        if device.device_id == current_device_id || !device.active_writer {
            continue;
        }

        device.active_writer = false;
        device.lease_expires_unix = now;
        let contents = serde_json::to_string_pretty(&device).map_err(|e| e.to_string())?;
        folder.write_text_atomic(&entry.path, &contents)?;
    }
    Ok(())
}

fn write_device_presence(folder: &SyncFolder, active_writer: bool) -> Result<(), String> {
    let device_id = settings::local_device_id()?;
    let now = now_unix();
    let presence = SyncDevicePresence {
        device_id: device_id.clone(),
        device_name: device_name(),
        active_writer,
        last_seen_unix: now,
        lease_expires_unix: if active_writer {
            now + LEASE_SECONDS
        } else {
            now
        },
    };
    let contents = serde_json::to_string_pretty(&presence).map_err(|e| e.to_string())?;
    folder.write_text_atomic(&format!("devices/{device_id}.json"), &contents)
}

fn detect_conflicts(folder: &SyncFolder) -> Result<Vec<SyncConflict>, String> {
    let mut conflicts = Vec::new();
    for entry in folder.list_recursive("")? {
        if entry.is_dir {
            continue;
        }
        let file_name = Path::new(&entry.path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let is_syncthing_conflict = file_name.contains("sync-conflict");
        let is_gloss_conflict = entry
            .path
            .split('/')
            .any(|component| component == "conflicts")
            && entry.path.ends_with(".db");
        if is_syncthing_conflict || is_gloss_conflict {
            conflicts.push(SyncConflict {
                id: stable_path_id(&entry.path),
                path: entry.path,
                kind: if is_syncthing_conflict {
                    "syncthing".to_string()
                } else {
                    "local_recovery".to_string()
                },
            });
        }
    }
    conflicts.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(conflicts)
}

fn clear_resolved_conflicts(folder: &SyncFolder) -> Result<(), String> {
    for conflict in detect_conflicts(folder)? {
        folder.delete_file(&conflict.path).map_err(|err| {
            format!(
                "failed to remove resolved sync conflict file {}: {err}",
                conflict.path
            )
        })?;
    }
    Ok(())
}

fn collect_file_entries(
    root: &Path,
    dir: &Path,
    out: &mut Vec<SyncFileEntry>,
) -> Result<(), String> {
    if !dir.exists() {
        return Ok(());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.to_string()),
    };
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let metadata = entry.metadata().map_err(|e| e.to_string())?;
        let relative = path.strip_prefix(root).unwrap_or(&path);
        let relative = relative.to_string_lossy().replace('\\', "/");
        out.push(SyncFileEntry {
            path: relative,
            is_dir: metadata.is_dir(),
        });
        if path.is_dir() {
            collect_file_entries(root, &path, out)?;
        }
    }
    Ok(())
}

fn copy_path_file_if_changed(source: &Path, destination: &Path) -> Result<(), String> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if destination.exists() {
        let source_len = source.metadata().map_err(|e| e.to_string())?.len();
        let destination_len = destination.metadata().map_err(|e| e.to_string())?.len();
        if source_len == destination_len {
            return Ok(());
        }
    }
    let tmp = sibling_tmp_path(destination);
    std::fs::copy(source, &tmp).map_err(|e| e.to_string())?;
    publish_path_file(&tmp, destination)
}

fn sibling_tmp_path(destination: &Path) -> PathBuf {
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("gloss-sync");
    destination.with_file_name(format!("{file_name}.tmp"))
}

fn publish_path_file(source: &Path, destination: &Path) -> Result<(), String> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if destination.exists() {
        std::fs::remove_file(destination).map_err(|e| e.to_string())?;
    }
    std::fs::rename(source, destination).map_err(|e| e.to_string())
}

fn hash_path_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    Ok(format!("{:016x}", fnv1a(&bytes)))
}

fn stable_path_id(path: &str) -> String {
    format!("{:016x}", fnv1a(path.as_bytes()))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    fnv1a_update(FNV_OFFSET, bytes)
}

fn fnv1a_update(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn quote_sqlite_ident(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn sqlite_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn sync_message(
    status: &str,
    dirty: bool,
    remote_newer: bool,
    active_writer: Option<&SyncDevicePresence>,
    conflict_count: usize,
    missing_pdf_count: usize,
) -> String {
    if let Some(writer) = active_writer {
        return format!(
            "{} is currently editing. This device should stay read-only until the lease expires or you take over.",
            writer.device_name
        );
    }
    if conflict_count > 0 {
        return format!("Found {conflict_count} sync conflict item(s) to review.");
    }
    if missing_pdf_count > 0 {
        return format!("Waiting for Syncthing to finish {missing_pdf_count} PDF file(s).");
    }
    if remote_newer && dirty {
        return "This device and the sync folder both have newer changes. Open Local sync to choose which copy to keep.".to_string();
    }
    if remote_newer {
        return "A newer synced snapshot is ready to import.".to_string();
    }
    if dirty {
        return "This device has local changes waiting for automatic sync.".to_string();
    }
    match status {
        "not_configured" => "Choose a folder to start local sync.".to_string(),
        "folder_missing" => "The configured sync folder is missing.".to_string(),
        "initialised" => "Sync folder is initialized and ready for the first snapshot.".to_string(),
        _ => "Local sync is ready.".to_string(),
    }
}

fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Gloss device".to_string())
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn info_snapshot_export(folder: &SyncFolder, revision: i64) {
    log::info!(
        target: "gloss_lib::sync",
        "exported sync snapshot revision={} folder={}",
        revision,
        folder.label()
    );
}
