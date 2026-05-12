#!/usr/bin/env node

const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const pkg = require('../package.json');

const baseUrl = process.env.CX_GITLAB_BASE_URL || 'https://git.huayi.tech';
const projectPath = process.env.CX_GITLAB_PROJECT_PATH || 'awesome/cx';
const token = process.env.CX_GITLAB_TOKEN || process.env.GITLAB_TOKEN;

function detectAssetName() {
  const platformMap = {
    darwin: 'darwin',
    linux: 'linux'
  };
  const archMap = {
    arm64: 'arm64',
    x64: 'x86_64'
  };

  const osName = platformMap[process.platform];
  const arch = archMap[process.arch];
  if (!osName || !arch) {
    throw new Error(`Unsupported platform: ${process.platform}/${process.arch}`);
  }
  return `cx-${osName}-${arch}`;
}

async function download(url, destination) {
  const headers = token ? { 'PRIVATE-TOKEN': token } : {};
  const response = await fetch(url, { headers, redirect: 'follow' });
  if (!response.ok) {
    throw new Error(`Download failed (${response.status}) for ${url}`);
  }
  const body = Buffer.from(await response.arrayBuffer());
  fs.mkdirSync(path.dirname(destination), { recursive: true });
  fs.writeFileSync(destination, body);
}

function verifyChecksum(checksumFile, binaryFile, assetName) {
  const checksumLine = fs
    .readFileSync(checksumFile, 'utf8')
    .split(/\r?\n/)
    .find((line) => line.endsWith(` ${assetName}`));
  if (!checksumLine) {
    throw new Error(`Missing checksum entry for ${assetName}`);
  }
  const expected = checksumLine.split(/\s+/)[0];
  const actual = crypto
    .createHash('sha256')
    .update(fs.readFileSync(binaryFile))
    .digest('hex');
  if (expected !== actual) {
    throw new Error(`Checksum mismatch for ${assetName}`);
  }
}

async function ensureBinary() {
  if (!token) {
    throw new Error('Missing CX_GITLAB_TOKEN or GITLAB_TOKEN for private GitLab release download');
  }

  const assetName = detectAssetName();
  const cacheRoot =
    process.env.XDG_CACHE_HOME || path.join(os.homedir(), '.cache');
  const installDir = path.join(cacheRoot, 'cx-internal', pkg.version);
  const binaryPath = path.join(installDir, 'cx');
  const checksumPath = path.join(installDir, 'SHA256SUMS');
  const releaseBase = `${baseUrl}/${projectPath}/-/releases/v${pkg.version}/downloads`;

  try {
    if (!fs.existsSync(binaryPath) || !fs.existsSync(checksumPath)) {
      throw new Error('missing cached binary or checksum');
    }
    verifyChecksum(checksumPath, binaryPath, assetName);
  } catch (_error) {
    await download(`${releaseBase}/binaries/${assetName}`, binaryPath);
    await download(`${releaseBase}/checksums/SHA256SUMS`, checksumPath);
    verifyChecksum(checksumPath, binaryPath, assetName);
    fs.chmodSync(binaryPath, 0o755);
  }

  return binaryPath;
}

(async () => {
  try {
    const binaryPath = await ensureBinary();
    const result = spawnSync(binaryPath, process.argv.slice(2), { stdio: 'inherit' });
    if (result.error) {
      throw result.error;
    }
    process.exit(result.status === null ? 1 : result.status);
  } catch (error) {
    console.error(`cx wrapper failed: ${error.message}`);
    process.exit(1);
  }
})();
