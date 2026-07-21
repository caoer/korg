#!/usr/bin/env bash
# Install grok-anthropic-serve from the GitHub latest release.
#
# Usage:
#   curl -fsSL https://github.com/caoer/korg/releases/latest/download/install.sh | bash
#   INSTALL_DIR=~/bin ./scripts/install-grok-anthropic-serve.sh
#   VERSION=v0.1.0 ./scripts/install-grok-anthropic-serve.sh   # pin a tag
#
# Env:
#   INSTALL_DIR  – install destination directory (default: ~/.local/bin)
#   REPO         – GitHub owner/repo (default: caoer/korg)
#   VERSION      – release tag (default: latest)
#   BIN_NAME     – binary name (default: grok-anthropic-serve)

set -euo pipefail

REPO="${REPO:-caoer/korg}"
VERSION="${VERSION:-latest}"
BIN_NAME="${BIN_NAME:-grok-anthropic-serve}"
INSTALL_DIR="${INSTALL_DIR:-${HOME}/.local/bin}"

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: required command not found: $1" >&2
    exit 1
  }
}

need curl
need tar
need uname

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"

case "${os}" in
  darwin) os_tag="apple-darwin" ;;
  linux) os_tag="unknown-linux-gnu" ;;
  *)
    echo "error: unsupported OS: ${os}" >&2
    exit 1
    ;;
esac

case "${arch}" in
  arm64 | aarch64) arch_tag="aarch64" ;;
  x86_64 | amd64) arch_tag="x86_64" ;;
  *)
    echo "error: unsupported arch: ${arch}" >&2
    exit 1
    ;;
esac

target="${arch_tag}-${os_tag}"
asset="${BIN_NAME}-${target}.tar.gz"

if [[ "${VERSION}" == "latest" ]]; then
  base="https://github.com/${REPO}/releases/latest/download"
else
  # Accept both v1.2.3 and 1.2.3
  tag="${VERSION}"
  [[ "${tag}" == v* ]] || tag="v${tag}"
  base="https://github.com/${REPO}/releases/download/${tag}"
fi

url="${base}/${asset}"
tmpdir="$(mktemp -d)"
trap 'rm -rf "${tmpdir}"' EXIT

echo "Downloading ${url}"
curl -fL --retry 3 --retry-delay 1 -o "${tmpdir}/${asset}" "${url}"

# Optional checksum verification when SHA256SUMS is available.
if curl -fsSL "${base}/SHA256SUMS" -o "${tmpdir}/SHA256SUMS" 2>/dev/null; then
  if command -v shasum >/dev/null 2>&1; then
    (cd "${tmpdir}" && shasum -a 256 -c SHA256SUMS --ignore-missing)
  elif command -v sha256sum >/dev/null 2>&1; then
    (cd "${tmpdir}" && sha256sum -c SHA256SUMS --ignore-missing)
  else
    echo "warning: no shasum/sha256sum; skipping checksum verify" >&2
  fi
else
  echo "warning: SHA256SUMS not found; skipping checksum verify" >&2
fi

tar -C "${tmpdir}" -xzf "${tmpdir}/${asset}"
test -f "${tmpdir}/${BIN_NAME}"

mkdir -p "${INSTALL_DIR}"
install -m 0755 "${tmpdir}/${BIN_NAME}" "${INSTALL_DIR}/${BIN_NAME}"

if [[ -f "${tmpdir}/VERSION.txt" ]]; then
  echo "Installed metadata:"
  sed 's/^/  /' "${tmpdir}/VERSION.txt" || true
fi

echo "Installed ${INSTALL_DIR}/${BIN_NAME}"
"${INSTALL_DIR}/${BIN_NAME}" --version 2>/dev/null || true

case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo "note: ${INSTALL_DIR} is not on PATH; add it or move the binary." >&2
    ;;
esac
