# Syncthing Local Sync Setup

This guide explains how to sync a Gloss library between devices using Syncthing. Gloss does not sync the live app database directly. Instead, it creates a normal folder called something like `Gloss Sync` that contains safe database snapshots and uploaded PDFs.

Use this when you want local, private sync between your own desktop and tablet devices without putting Gloss data in a cloud account.

## What Gloss Syncs

Gloss syncs:

- Uploaded PDFs.
- Document metadata.
- Chunks and rechunking data.
- Notes, ink, marks, glossary entries, surfaces, and graph objects.
- Non-secret preferences that are safe to share between devices.

Gloss does not sync:

- AI API keys.
- Device-local setup state.
- The live app-private `gloss.db` database.

## Before You Start

Install Syncthing on each device that should share the Gloss library.

- Desktop: install Syncthing from the official site for Windows, macOS, or Linux.
- Android: use a currently maintained Syncthing Android option, such as Syncthing-Fork or another community-maintained wrapper. The original `syncthing-android` wrapper was archived in December 2024.

Pick one device to be the first source of truth. Usually this is the device where your current Gloss library already exists.

Do not put the Gloss sync folder inside another cloud-sync folder such as OneDrive, iCloud Drive, Dropbox, or Google Drive. Let Syncthing be the only tool syncing this folder.

## Folder Layout

When you connect a folder in Gloss, it creates this structure:

```text
Gloss Sync/
  .gloss-sync.json
  .stignore
  library/
    gloss.snapshot.db
  pdfs/
    <document_uuid>.pdf
  devices/
    <device_id>.json
  conflicts/
```

The important part is that Syncthing should sync the `Gloss Sync` folder itself. Do not point Syncthing at Gloss app data, and do not manually sync `gloss.db`.

Gloss writes `.stignore` on each linked device. Syncthing keeps `.stignore` local to each device, so Gloss recreates it whenever the folder is connected.

## Device 1: Create The Gloss Sync Folder

1. Open Gloss on the device that already has the library you want to share.
2. Go to the library screen.
3. Click `Local sync`.
4. Click `Choose`.
5. Create or select a user-visible folder, for example:

```text
C:\Users\<you>\Documents\Gloss Sync
```

or on macOS/Linux:

```text
~/Documents/Gloss Sync
```

6. Leave Gloss open for a few seconds.

Gloss will automatically publish a clean snapshot and copy the uploaded PDFs into the sync folder.

## Device 1: Add The Folder To Syncthing

1. Open the Syncthing web UI or Syncthing desktop app.
2. Choose `Add Folder`.
3. Set the folder label to `Gloss Sync`.
4. Set the folder path to the same folder you chose in Gloss.
5. Set the folder type to `Send & Receive`.
6. Enable file watching if Syncthing offers it.
7. Open file versioning settings for this folder.
8. Choose `Simple File Versioning`.
9. Set `Keep Versions` to at least `5`.
10. Share the folder with your other devices.
11. Wait until Syncthing shows the folder as `Up to Date`.

File versioning is configured per device and per folder, so repeat the versioning step on each device that joins the Gloss folder.

## Device 2: Join The Syncthing Folder

1. Install and open Syncthing on the second device.
2. Pair it with Device 1 by exchanging device IDs.
3. Accept the shared `Gloss Sync` folder from Device 1, or add a folder manually with the same folder ID.
4. Choose a local path that both Syncthing and Gloss can access.
5. Set folder type to `Send & Receive`.
6. Enable `Simple File Versioning` with at least `5` versions.
7. Wait until Syncthing says the folder is `Up to Date`.

On Android, choose a normal shared-storage folder that the Android sync app can write to and Gloss can select through Android's folder picker. A folder such as `Documents/Gloss Sync` is a good choice. Do not use the storage root, `Download` itself, or anything under `Android/data`, because modern Android versions restrict those locations. Also allow the Android sync app to run in the background, and consider disabling battery optimization for it so it can finish syncing large PDFs.

## Device 2: Connect Gloss

1. Open Gloss on Device 2.
2. Go to the library screen.
3. Click `Local sync`.
4. Click `Choose`.
5. Select the same local `Gloss Sync` folder that Syncthing is managing on this device. On Android this uses the system folder picker and Gloss keeps the read/write permission after restart.
6. Leave Gloss on the library screen for a few seconds.

Gloss will automatically stage the synced snapshot, copy missing PDFs into its local cache, preserve device-local secrets, and then replace this device's local Gloss library with the synced copy.

If Gloss says it is waiting for PDFs, leave Syncthing running until the folder is fully up to date. Gloss will try again automatically.

## Normal Editing Workflow

Use one active editing device at a time.

Before editing on a device:

1. Let Syncthing finish until the folder is `Up to Date`.
2. Open Gloss.
3. Stay on the library screen for a moment if another device published newer work.
4. Edit normally.

After editing:

1. Leave Gloss open for a few seconds so it can publish the latest snapshot.
2. Leave Syncthing running until it shows `Up to Date`.
3. Only then switch to editing on another device.

Gloss keeps the manual `Sync now`, `Import latest`, and `Take editing lease` buttons for recovery and debugging, but the normal path is automatic. If another device currently holds the editing lease, Gloss may open in read-only mode for writes. Use `Take editing lease` only when you are sure the other device is finished or unavailable.

## Conflicts And Recovery

Gloss is designed to avoid silent overwrites, but conflicts can still happen if two devices edit offline and then reconnect.

If Gloss reports conflicts:

1. Stop editing on all devices.
2. Let Syncthing finish syncing everything it can.
3. Open `Local sync` in Gloss.
4. Review the conflict message.
5. Choose `Use synced copy` to replace this device with the synced snapshot, or `Keep this device` to publish this device's local copy.

When local unsynced work conflicts with a newer remote snapshot, Gloss saves a recovery database under:

```text
Gloss Sync/conflicts/
```

Syncthing's Simple File Versioning may also keep older replaced files under `.stversions`. That is a last-resort recovery aid, not the normal workflow.

## Troubleshooting

### Gloss says it is waiting for PDFs

Syncthing has not finished downloading every referenced PDF yet. Keep Syncthing open until the folder is `Up to Date`, then import again.

### Syncthing shows conflict files

Do not delete them immediately. Open Gloss, go to `Local sync`, and resolve the conflict there first. If needed, keep a copy of the `conflicts/` folder before cleaning up old files.

### A device cannot edit

Another device may have the active editing lease. Wait up to 10 minutes for the lease to expire, or use `Take editing lease` if you are sure the other device is not editing.

### API keys are missing on a new device

That is expected. API keys stay local-only. Add the key again on each device that should use AI features.

### A PDF was deleted by mistake

Check Syncthing file versioning on another device, especially the `.stversions` folder for the `Gloss Sync` folder. Restore the matching `pdfs/<document_uuid>.pdf` file, wait for Syncthing to finish, then open Gloss again.

## References

- Syncthing ignore files: https://docs.syncthing.net/users/ignoring.html
- Syncthing file versioning: https://docs.syncthing.net/users/versioning
- Archived original Android wrapper: https://github.com/syncthing/syncthing-android
