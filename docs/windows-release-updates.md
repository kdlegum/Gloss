# Windows releases and updates

Gloss now has two Windows update layers:

- The app checks GitHub Releases and shows an "Update available" button on the library screen.
- The update banner can install signed Tauri updater packages from `latest.json`.
- Tagged Windows releases are built with Tauri updater artifacts.

## Release checklist

1. Commit every migration in `src-tauri/migrations/`.

   Never ship a build from a commit that is missing a migration already applied by local/dev builds. That is what causes SQLx `VersionMissing` startup failures on existing data.

2. Bump the version in all three places:

   - `package.json`
   - `src-tauri/Cargo.toml`
   - `src-tauri/tauri.conf.json`

3. Run the validation commands locally:

   ```powershell
   npm run check
   cargo check --manifest-path src-tauri\Cargo.toml
   ```

4. Create and push a semver tag that matches the app version:

   ```powershell
   git tag v0.1.1
   git push origin v0.1.1
   ```

5. GitHub Actions verifies that `package.json`, `src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json` all match the tag version.

6. GitHub Actions creates a draft GitHub Release with the Windows `.msi`, NSIS `.exe`, updater bundles, signatures, and `latest.json`.

7. Install the draft artifact on a test Windows machine, open an existing Gloss library, and confirm the database migrates cleanly.

8. Publish the draft release.

The app checks releases without GitHub authentication. If the repository remains private, the GitHub Releases API and `latest.json` download URL will still return 404 from installed apps. In that case, either make the release assets public or host `latest.json` and the updater assets at a public URL and update the endpoint in `src-tauri/tauri.conf.json`.

## Signing setup for updater artifacts

The tag release job expects these repository secrets:

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

The workflow creates the draft GitHub Release with a token that has `contents: write`.
By default it uses GitHub's built-in `GITHUB_TOKEN`, and the Windows release job
already requests that permission. If GitHub still reports `Resource not accessible
by integration` while creating the release, check **Settings > Actions > General >
Workflow permissions** and any organization policy that may restrict write tokens.
As a fallback, create a fine-grained personal access token scoped to this repository
with **Contents: Read and write**, then store it as a repository secret named
`GH_RELEASE_TOKEN`. The Windows and Android release upload steps will prefer that
secret when it exists and otherwise fall back to `GITHUB_TOKEN`.

Generate them with the Tauri signer command, then keep the private key only in GitHub Secrets or another secret manager:

```powershell
npm run tauri signer generate -w .tauri-signing/gloss-updater.key -p <strong-password>
```

The public key is not a secret and is committed in `src-tauri/tauri.conf.json`.

The private key generated for this repo is stored locally at `.tauri-signing/gloss-updater.key`, which is ignored by Git. Copy its contents into the `TAURI_SIGNING_PRIVATE_KEY` GitHub secret. The generated password is stored locally at `.tauri-signing/gloss-updater.password.txt` and belongs in `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
