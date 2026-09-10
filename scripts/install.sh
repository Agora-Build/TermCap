#!/usr/bin/env bash
# tcap installer.
#
#   curl -fsSL https://dl.agora.build/tcap/install.sh | bash
#
# Downloads the prebuilt `tcap` binary for this OS/arch into /usr/local/bin and
# prints how to enable shell integration. Nothing is added to your dotfiles —
# that step is left to you.
#
# Env overrides:
#   TCAP_REPO       GitHub owner/repo         (default Agora-Build/TermCap)
#   TCAP_VERSION    release tag or "latest"   (default latest)
#   TCAP_BINDIR     install dir               (default /usr/local/bin)
set -euo pipefail

REPO="${TCAP_REPO:-Agora-Build/TermCap}"
VERSION="${TCAP_VERSION:-latest}"
BINDIR="${TCAP_BINDIR:-/usr/local/bin}"
DL_BASE="https://dl.agora.build/tcap"

say() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

SUDO=""
if [ ! -w "$BINDIR" ] 2>/dev/null || [ ! -d "$BINDIR" ]; then
  if [ "$(id -u)" -ne 0 ]; then
    command -v sudo >/dev/null 2>&1 && SUDO="sudo" \
      || die "need root (or sudo) to install into $BINDIR — set TCAP_BINDIR to a writable dir"
  fi
fi

# --- detect platform -> release target ---
os="$(uname -s)"; arch="$(uname -m)"
case "$os" in
  Darwin) os_tag="darwin" ;;
  Linux)  os_tag="linux" ;;
  *) die "unsupported OS: $os" ;;
esac
case "$arch" in
  x86_64|amd64)  arch_tag="x86_64" ;;
  arm64|aarch64) arch_tag="aarch64" ;;
  *) die "unsupported arch: $arch" ;;
esac
target="${os_tag}-${arch_tag}"
say "platform: $target"

# --- resolve version ---
if [ "$VERSION" = "latest" ]; then
  # Prefer the R2 marker: it is a plain text file, so no JSON parsing and no
  # GitHub API rate limit. Fall back to the API if the mirror is unreachable.
  VERSION="$(curl -fsSL "${DL_BASE}/releases/latest" 2>/dev/null || true)"
  if [ -n "$VERSION" ]; then
    VERSION="v${VERSION#v}"
  else
    VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
      | grep -m1 '"tag_name"' | cut -d'"' -f4)"
  fi
  [ -n "$VERSION" ] || die "could not resolve the latest version"
fi
say "version: $VERSION"

ver_no_v="${VERSION#v}"
asset="tcap-${ver_no_v}-${target}.tar.gz"

tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT

# Try the CDN first, then GitHub.
downloaded=""
for url in \
  "${DL_BASE}/releases/${VERSION}/${asset}" \
  "https://github.com/${REPO}/releases/download/${VERSION}/${asset}"
do
  say "downloading $url"
  if curl -fSL -o "$tmp/$asset" "$url" 2>/dev/null; then
    downloaded="$url"
    break
  fi
done
[ -n "$downloaded" ] || die "could not download $asset from either the CDN or GitHub"

# Verify the checksum when the sidecar is published alongside the tarball.
if curl -fsSL -o "$tmp/$asset.sha256" "${downloaded}.sha256" 2>/dev/null; then
  say "verifying checksum"
  expected="$(awk '{print $1}' "$tmp/$asset.sha256")"
  if command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
  else
    actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
  fi
  [ "$expected" = "$actual" ] || die "checksum mismatch (expected $expected, got $actual)"
else
  say "no checksum published for this release — skipping verification"
fi

tar -xzf "$tmp/$asset" -C "$tmp"
[ -f "$tmp/tcap" ] || die "unexpected tarball layout: no tcap at the archive root"

say "installing to $BINDIR"
$SUDO mkdir -p "$BINDIR"
$SUDO install -m 0755 "$tmp/tcap" "$BINDIR/tcap"

say "installed: $("$BINDIR/tcap" --version 2>/dev/null || echo tcap)"
cat <<'EOS'

Next: enable shell integration so tcap can record exit codes and command
boundaries. Add the line for your shell, then open a new terminal.

  zsh   echo 'eval "$(tcap init zsh)"'   >> ~/.zshrc
  bash  echo 'eval "$(tcap init bash)"'  >> ~/.bashrc
  fish  echo 'tcap init fish | source'   >> ~/.config/fish/config.fish

Then check everything is wired up:

  tcap doctor

EOS
