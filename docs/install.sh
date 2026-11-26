#!/bin/sh
# popier installer: downloads the static Linux binary from the latest GitHub
# release, checks its SHA-256 and installs it.
#
#   curl -fsSL https://viralabs-dev.github.io/popier/install.sh | sh
#
# Options (environment variables):
#   POPIER_VERSION      a release tag such as v0.1.0 (default: latest)
#   POPIER_INSTALL_DIR  where to put the binary (default: ~/.local/bin)
set -eu

repo="viralabs-dev/popier"
version="${POPIER_VERSION:-latest}"
dir="${POPIER_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '\033[1;33m◆\033[0m %s\n' "$*"; }
die() { printf '\033[1;31m✗\033[0m %s\n' "$*" >&2; exit 1; }

os="$(uname -s)"
arch="$(uname -m)"
[ "$os" = "Linux" ] || die "popier has prebuilt binaries for Linux only (this is $os). Build it with: cargo install --git https://github.com/$repo"
case "$arch" in
  x86_64 | amd64) target="x86_64-unknown-linux-musl" ;;
  *) die "no prebuilt binary for $arch yet. Build it with: cargo install --git https://github.com/$repo" ;;
esac

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL --retry 3 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q -O "$2" "$1"; }
else
  die "curl or wget is needed to download popier"
fi
command -v tar >/dev/null 2>&1 || die "tar is needed to unpack popier"

if [ "$version" = "latest" ]; then
  base="https://github.com/$repo/releases/latest/download"
else
  base="https://github.com/$repo/releases/download/$version"
fi
asset="popier-$target.tar.gz"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $asset ($version)"
fetch "$base/$asset" "$tmp/$asset" || die "download failed: $base/$asset"

if fetch "$base/$asset.sha256" "$tmp/$asset.sha256" 2>/dev/null; then
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$tmp" && sha256sum -c --quiet "$asset.sha256") || die "checksum mismatch, not installing"
    say "checksum ok"
  else
    say "sha256sum not found, skipping the checksum"
  fi
fi

tar -xzf "$tmp/$asset" -C "$tmp"
[ -f "$tmp/popier" ] || die "the archive has no popier binary"
mkdir -p "$dir"
install -m 755 "$tmp/popier" "$dir/popier"
say "installed $("$dir/popier" --version) to $dir/popier"

case ":$PATH:" in
  *":$dir:"*) ;;
  *)
    say "$dir is not on your PATH. Add this to your shell profile:"
    printf '    export PATH="%s:$PATH"\n' "$dir"
    ;;
esac

printf '\nNext:\n  popier init --discover   # find your repos (uses your gh login)\n  popier                   # watch\n'
