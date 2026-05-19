#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const rootDir = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const versionFiles = [
  'package.json',
  'package-lock.json',
  'src-tauri/tauri.conf.json',
  'src-tauri/Cargo.toml'
];

const semverPattern =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

function usage() {
  console.log(`Usage: npm run release:tag -- <version|tag> [--dry-run] [--no-push]

Examples:
  npm run release:tag -- 0.1.2
  npm run release:tag -- v0.1.2 --no-push

Updates the app version files, creates a version-bump commit, runs git tag,
and pushes the tag with git push origin <tag>.`);
}

function fail(message) {
  console.error(`release-version: ${message}`);
  process.exit(1);
}

function parseArgs(args) {
  const knownFlags = new Set(['--dry-run', '--no-push', '--help', '-h']);
  const flags = new Set();
  const values = [];

  for (const arg of args) {
    if (arg.startsWith('-')) {
      if (!knownFlags.has(arg)) {
        fail(`unknown option ${arg}`);
      }
      flags.add(arg);
    } else {
      values.push(arg);
    }
  }

  if (flags.has('--help') || flags.has('-h')) {
    usage();
    process.exit(0);
  }

  if (values.length !== 1) {
    usage();
    process.exit(values.length === 0 ? 0 : 1);
  }

  const version = values[0].trim().replace(/^[vV]/, '');
  if (!semverPattern.test(version)) {
    fail(`expected a semver version or v-prefixed semver tag, got ${values[0]}`);
  }

  return {
    dryRun: flags.has('--dry-run'),
    noPush: flags.has('--no-push'),
    tag: `v${version}`,
    version
  };
}

function git(args, { allowFailure = false, capture = false } = {}) {
  const result = spawnSync('git', args, {
    cwd: rootDir,
    encoding: 'utf8',
    stdio: capture ? ['ignore', 'pipe', 'pipe'] : 'inherit'
  });

  if (result.error) {
    throw result.error;
  }

  if (result.status !== 0 && !allowFailure) {
    const detail = capture ? result.stderr.trim() : '';
    throw new Error(detail || `git ${args.join(' ')} failed`);
  }

  return result;
}

function ensureVersionFilesClean() {
  const result = git(['status', '--porcelain', '--', ...versionFiles], { capture: true });
  const status = result.stdout.trim();
  if (status) {
    throw new Error(
      `version files already have local changes:\n${status}\nCommit or stash those changes before releasing.`
    );
  }
}

function ensureLocalTagMissing(tag) {
  const result = git(['rev-parse', '--verify', '--quiet', `refs/tags/${tag}`], {
    allowFailure: true,
    capture: true
  });

  if (result.status === 0) {
    throw new Error(`local tag ${tag} already exists`);
  }
}

function readProjectJson(path) {
  const text = readFileSync(resolve(rootDir, path), 'utf8');
  return JSON.parse(text);
}

function formatJson(value) {
  return `${JSON.stringify(value, null, 2)}\n`;
}

function setJsonVersion(path, version) {
  const absolutePath = resolve(rootDir, path);
  const data = JSON.parse(readFileSync(absolutePath, 'utf8'));
  data.version = version;
  return formatJson(data);
}

function setPackageLockVersion(version) {
  const absolutePath = resolve(rootDir, 'package-lock.json');
  const data = JSON.parse(readFileSync(absolutePath, 'utf8'));

  if (!data.packages?.['']) {
    throw new Error('package-lock.json does not contain packages[""]');
  }

  data.version = version;
  data.packages[''].version = version;
  return formatJson(data);
}

function setCargoPackageVersion(version) {
  const absolutePath = resolve(rootDir, 'src-tauri/Cargo.toml');
  const text = readFileSync(absolutePath, 'utf8');
  const parts = text.split(/(\r?\n)/);
  let inPackageSection = false;

  for (let index = 0; index < parts.length; index += 2) {
    const line = parts[index];

    if (/^\s*\[package\]\s*$/.test(line)) {
      inPackageSection = true;
      continue;
    }

    if (inPackageSection && /^\s*\[[^\]]+\]\s*$/.test(line)) {
      break;
    }

    if (inPackageSection && /^\s*version\s*=/.test(line)) {
      parts[index] = line.replace(
        /^(\s*version\s*=\s*")[^"]+(".*)$/,
        (_match, before, after) => `${before}${version}${after}`
      );
      return parts.join('');
    }
  }

  throw new Error('could not find package version in src-tauri/Cargo.toml');
}

function nextContentFor(path, version) {
  if (path === 'package-lock.json') {
    return setPackageLockVersion(version);
  }

  if (path === 'src-tauri/Cargo.toml') {
    return setCargoPackageVersion(version);
  }

  return setJsonVersion(path, version);
}

function currentVersions() {
  const packageJson = readProjectJson('package.json');
  const packageLock = readProjectJson('package-lock.json');
  const tauriConfig = readProjectJson('src-tauri/tauri.conf.json');
  const cargoToml = readFileSync(resolve(rootDir, 'src-tauri/Cargo.toml'), 'utf8');
  const cargoVersion = cargoToml.match(/^\s*version\s*=\s*"([^"]+)"/m)?.[1];

  return {
    'package.json': packageJson.version,
    'package-lock.json': packageLock.version,
    'package-lock.json packages[""]': packageLock.packages?.['']?.version,
    'src-tauri/tauri.conf.json': tauriConfig.version,
    'src-tauri/Cargo.toml': cargoVersion
  };
}

function ensureVersionsMatch(version) {
  const versions = currentVersions();
  const mismatches = Object.entries(versions).filter(([, actual]) => actual !== version);

  if (mismatches.length > 0) {
    throw new Error(
      `version mismatch after update:\n${mismatches
        .map(([path, actual]) => `  ${path}: ${actual ?? '(missing)'}`)
        .join('\n')}`
    );
  }
}

function main() {
  const { dryRun, noPush, tag, version } = parseArgs(process.argv.slice(2));

  if (!dryRun) {
    ensureVersionFilesClean();
    ensureLocalTagMissing(tag);
  }

  const updates = versionFiles.map((path) => {
    const absolutePath = resolve(rootDir, path);
    const original = readFileSync(absolutePath, 'utf8');
    const next = nextContentFor(path, version);
    return { absolutePath, changed: original !== next, next, path };
  });

  const changed = updates.filter((update) => update.changed);

  if (dryRun) {
    const summary = changed.length === 0 ? 'No file changes needed.' : `Would update ${changed.length} file(s):`;
    console.log(`${summary}${changed.length > 0 ? `\n${changed.map((update) => `  ${update.path}`).join('\n')}` : ''}`);
    console.log(`Would tag ${tag}${noPush ? '' : ' and push it to origin'}.`);
    return;
  }

  for (const update of changed) {
    writeFileSync(update.absolutePath, update.next);
  }

  ensureVersionsMatch(version);

  if (changed.length > 0) {
    console.log(`Updated ${changed.length} version file(s).`);
    git(['commit', '--only', '-m', `Bump version to ${tag}`, '--', ...versionFiles]);
  } else {
    console.log(`Version files already match ${version}; tagging current HEAD.`);
  }

  git(['tag', tag]);

  if (noPush) {
    console.log(`Created local tag ${tag}.`);
  } else {
    git(['push', 'origin', tag]);
  }
}

try {
  main();
} catch (error) {
  fail(error.message);
}
