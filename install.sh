#!/bin/sh
# Install the amail binary from a GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/OWNER/agent-mail/main/install.sh | sh
#
# Environment:
#   AMAIL_VERSION      version to install, e.g. 0.1.0 (default: the latest release)
#   AMAIL_INSTALL_DIR  where to put the binary (default: ~/.local/bin)
#   AMAIL_REPO         GitHub repository (default below)
#   AMAIL_DOWNLOAD_BASE  base URL of the release files, instead of GitHub
#                      (for mirrors and tests); files are fetched from
#                      $AMAIL_DOWNLOAD_BASE/<file>
#
# Prebuilt binaries exist for Linux x86_64 and aarch64. The download is
# checked against the release's SHA256SUMS before anything is installed.
set -eu

AMAIL_REPO="${AMAIL_REPO:-OWNER/agent-mail}"

say() { printf '%s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required"; }

fetch() { # url dest
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --proto '=https,http' -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$2" "$1"
    else
        die "curl or wget is required"
    fi
}

os=$(uname -s)
arch=$(uname -m)
case "$os" in
    Linux) ;;
    Darwin)
        say "Prebuilt amail binaries are for Linux only. On macOS the server and the"
        say "mail commands can be built from source (agents and teams need Linux):"
        say "  git clone https://github.com/$AMAIL_REPO && cd agent-mail"
        say "  (cd web && npm ci && npm run build)"
        say "  cargo install --path crates/amail-cli"
        exit 1
        ;;
    *) die "unsupported system: $os (prebuilt binaries are for Linux)" ;;
esac
case "$arch" in
    x86_64 | amd64) target=x86_64-unknown-linux-gnu ;;
    aarch64 | arm64) target=aarch64-unknown-linux-gnu ;;
    *) die "unsupported architecture: $arch (prebuilt binaries: x86_64, aarch64)" ;;
esac

need tar
if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | cut -d ' ' -f 1; }
elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
else
    die "sha256sum or shasum is required to verify the download"
fi

version="${AMAIL_VERSION:-}"
version="${version#v}"
if [ -z "$version" ]; then
    if [ -n "${AMAIL_DOWNLOAD_BASE:-}" ]; then
        die "set AMAIL_VERSION when AMAIL_DOWNLOAD_BASE is used"
    fi
    need curl
    # The latest release's page redirects to .../releases/tag/v<version>.
    url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$AMAIL_REPO/releases/latest") \
        || die "cannot find the latest release of $AMAIL_REPO"
    version="${url##*/v}"
    case "$version" in
        "" | */*) die "cannot find the latest release of $AMAIL_REPO (got $url)" ;;
    esac
fi

base="${AMAIL_DOWNLOAD_BASE:-https://github.com/$AMAIL_REPO/releases/download/v$version}"
name="amail-$version-$target"
dir="${AMAIL_INSTALL_DIR:-$HOME/.local/bin}"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' INT TERM

say "downloading $name.tar.gz"
fetch "$base/$name.tar.gz" "$tmp/$name.tar.gz" || die "download failed: $base/$name.tar.gz"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "download failed: $base/SHA256SUMS"

want=$(awk -v f="$name.tar.gz" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")
[ -n "$want" ] || die "$name.tar.gz is not listed in SHA256SUMS"
got=$(sha256 "$tmp/$name.tar.gz")
[ "$want" = "$got" ] || die "checksum mismatch for $name.tar.gz (expected $want, got $got)"
say "checksum ok"

tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
[ -f "$tmp/$name/amail" ] || die "the archive has no $name/amail"
mkdir -p "$dir"
# Replace the file instead of writing into it: a running amail keeps its copy.
cp "$tmp/$name/amail" "$dir/.amail.new"
chmod 755 "$dir/.amail.new"
mv -f "$dir/.amail.new" "$dir/amail"
say "installed amail $version to $dir/amail"

case ":$PATH:" in
    *":$dir:"*) ;;
    *) say "note: $dir is not on your PATH; add it, e.g. export PATH=\"$dir:\$PATH\"" ;;
esac
