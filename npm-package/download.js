// Resolves the voom binary, fetching it from the GitHub release on first use.
//
// This is deliberately *not* a postinstall script. npm 11.19 blocks install scripts by default
// (`allow-scripts`), and a blocked postinstall does not merely skip the download — npm declines
// to create the bin link at all, so `npm i -g @goldziher/voom` produced `voom: command not
// found` and `npx -y @goldziher/voom` silently did nothing. Fetching lazily from `bin/voom`
// instead means the package works with scripts disabled, which is now the default and will only
// get stricter. It is also what the PyPI wrapper has always done (pip-package/voom/downloader.py),
// so the two channels now behave the same way.

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const https = require("node:https");
const http = require("node:http");
const tar = require("tar");
const AdmZip = require("adm-zip");

const { version } = require("./package.json");

const REPO = "Goldziher/voom";
const BINARY = "voom";

function getPlatformTriple() {
  const type = os.type();
  const arch = os.arch();

  if (type === "Windows_NT") {
    if (arch === "x64") return "x86_64-pc-windows-gnu";
    throw new Error(`Unsupported Windows architecture: ${arch}`);
  }

  if (type === "Linux") {
    if (arch === "x64") return "x86_64-unknown-linux-gnu";
    if (arch === "arm64") return "aarch64-unknown-linux-gnu";
    throw new Error(`Unsupported Linux architecture: ${arch}`);
  }

  if (type === "Darwin") {
    if (arch === "x64") return "x86_64-apple-darwin";
    if (arch === "arm64") return "aarch64-apple-darwin";
    throw new Error(`Unsupported macOS architecture: ${arch}`);
  }

  throw new Error(`Unsupported platform: ${type} ${arch}`);
}

// npm keeps the `-rc.N` form that git tags use, so no normalization is needed here.
// The PyPI wrapper does need it — see pip-package/voom/downloader.py.
function getBinaryUrl() {
  const platform = getPlatformTriple();
  const ext = platform.includes("windows") ? "zip" : "tar.gz";
  return `https://github.com/${REPO}/releases/download/v${version}/${BINARY}-${platform}.${ext}`;
}

// Per-user rather than inside the package directory: a global install often lands in a
// root-owned prefix that the running user cannot write to, which is exactly when the first run
// would need to write. Keyed by version so an upgrade fetches rather than reusing the old one.
function cacheDir() {
  if (os.type() === "Windows_NT" && process.env.LOCALAPPDATA) {
    return path.join(process.env.LOCALAPPDATA, BINARY, version);
  }
  return path.join(os.homedir(), ".cache", BINARY, version);
}

function binaryName() {
  return os.type() === "Windows_NT" ? `${BINARY}.exe` : BINARY;
}

function downloadWithRedirects(url, dest, maxRedirects = 5) {
  return new Promise((resolve, reject) => {
    if (maxRedirects <= 0) {
      return reject(new Error("Too many redirects"));
    }

    const urlObj = new URL(url);
    const client = urlObj.protocol === "https:" ? https : http;

    const req = client.get(url, { headers: { "User-Agent": "voom-npm-wrapper" } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        return downloadWithRedirects(res.headers.location, dest, maxRedirects - 1)
          .then(resolve)
          .catch(reject);
      }

      if (res.statusCode !== 200) {
        return reject(new Error(`HTTP ${res.statusCode}: ${res.statusMessage}`));
      }

      const file = fs.createWriteStream(dest);
      res.pipe(file);

      file.on("finish", () => {
        file.close();
        resolve();
      });

      file.on("error", (err) => {
        fs.unlink(dest, () => {});
        reject(err);
      });
    });

    req.on("error", reject);
    req.setTimeout(30000, () => {
      req.destroy();
      reject(new Error("Download timeout"));
    });
  });
}

/// Returns a path to the voom binary, downloading it on first use.
async function ensureBinary() {
  const override = process.env.VOOM_BINARY;
  if (override) {
    return override;
  }

  const dir = cacheDir();
  const name = binaryName();
  const binaryPath = path.join(dir, name);
  if (fs.existsSync(binaryPath)) {
    return binaryPath;
  }

  const url = getBinaryUrl();
  const isZip = url.endsWith(".zip");
  fs.mkdirSync(dir, { recursive: true });

  // Two concurrent invocations would otherwise race on the same target. Each unpacks into its
  // own staging directory and renames, which is atomic within a filesystem — the loser's rename
  // simply replaces an identical file.
  const staging = fs.mkdtempSync(path.join(dir, ".staging-"));
  const archivePath = path.join(staging, isZip ? `${BINARY}.zip` : `${BINARY}.tar.gz`);

  process.stderr.write(`Downloading voom binary v${version}...\n`);
  try {
    await downloadWithRedirects(url, archivePath);

    if (isZip) {
      const zip = new AdmZip(archivePath);
      const entry = zip.getEntries().find((e) => e.entryName.endsWith(name));
      if (!entry) {
        throw new Error("Binary not found in downloaded archive");
      }
      zip.extractEntryTo(entry, staging, false, true);
    } else {
      await tar.extract({
        file: archivePath,
        cwd: staging,
        filter: (entryPath) => entryPath.endsWith(name),
      });
    }

    const staged = path.join(staging, name);
    if (!fs.existsSync(staged)) {
      throw new Error("Binary not found in downloaded archive");
    }
    if (os.type() !== "Windows_NT") {
      fs.chmodSync(staged, 0o755);
    }
    fs.renameSync(staged, binaryPath);
  } finally {
    fs.rmSync(staging, { recursive: true, force: true });
  }

  process.stderr.write("Binary downloaded successfully.\n");
  return binaryPath;
}

module.exports = { ensureBinary, getBinaryUrl, getPlatformTriple };
