#!/usr/bin/env node
// postinstall: download the prebuilt tcap binary for this platform into bin/.
//
// Falls back to dl.agora.build if GitHub is unreachable, and never hard-fails
// `npm install` — a broken download leaves the placeholder in bin/tcap, which
// prints how to recover rather than a confusing ENOENT.
"use strict";

const https = require("https");
const fs = require("fs");
const path = require("path");
const os = require("os");
const { execSync } = require("child_process");

const pkg = require("./package.json");
const REPO = process.env.TCAP_REPO || "Agora-Build/TermCap";
const VERSION = pkg.version;
const TAG = `v${VERSION}`;
const BIN_DIR = path.join(__dirname, "bin");
const BIN_PATH = path.join(BIN_DIR, "tcap");

const ARCH = { x64: "x86_64", arm64: "aarch64" }[process.arch];
const OS = { darwin: "darwin", linux: "linux" }[process.platform];

function bail(msg) {
  // Exit 0: a failed optional download must not fail the consumer's whole
  // dependency tree. bin/tcap stays as the placeholder that explains itself.
  console.error(`tcap: ${msg}`);
  console.error(`tcap: install manually from https://github.com/${REPO}/releases/tag/${TAG}`);
  console.error(`tcap: or run: curl -fsSL https://dl.agora.build/tcap/install.sh | bash`);
  process.exit(0);
}

if (!OS || !ARCH) {
  bail(`unsupported platform ${process.platform}/${process.arch}`);
}

const target = `${OS}-${ARCH}`;
const asset = `tcap-${VERSION}-${target}.tar.gz`;
const sources = [
  `https://github.com/${REPO}/releases/download/${TAG}/${asset}`,
  `https://dl.agora.build/tcap/releases/${TAG}/${asset}`,
];

function download(url, dest, redirects = 0) {
  return new Promise((resolve, reject) => {
    if (redirects > 5) return reject(new Error("too many redirects"));
    https
      .get(url, { headers: { "User-Agent": "tcap-npm-installer" } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          res.resume();
          return resolve(download(res.headers.location, dest, redirects + 1));
        }
        if (res.statusCode !== 200) {
          res.resume();
          return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
        }
        const f = fs.createWriteStream(dest);
        res.pipe(f);
        f.on("finish", () => f.close(() => resolve()));
        f.on("error", reject);
      })
      .on("error", reject);
  });
}

async function install() {
  fs.mkdirSync(BIN_DIR, { recursive: true });
  const tmp = path.join(os.tmpdir(), `tcap-${Date.now()}.tar.gz`);

  let lastErr;
  for (const url of sources) {
    try {
      console.log(`tcap: downloading ${url}`);
      await download(url, tmp);
      lastErr = null;
      break;
    } catch (e) {
      lastErr = e;
      console.error(`tcap: ${e.message}`);
    }
  }
  if (lastErr) throw lastErr;

  try {
    // Flat tarball: the executable sits at the archive root.
    execSync(`tar -xzf "${tmp}" -C "${BIN_DIR}"`, { stdio: "pipe" });
  } finally {
    try {
      fs.unlinkSync(tmp);
    } catch {}
  }

  fs.chmodSync(BIN_PATH, 0o755);
  console.log(`tcap: installed ${VERSION} (${target}) to ${BIN_PATH}`);
  console.log('tcap: enable shell integration:  eval "$(tcap init zsh)"  in ~/.zshrc');
}

install().catch((err) => bail(`download failed: ${err.message}`));
