#!/usr/bin/env bash
# Install grok-anthropic-serve from the GitHub latest release.
#
# Usage:
#   curl -fsSL https://github.com/caoer/korg/releases/latest/download/install.sh | bash
#   INSTALL_DIR=~/bin ./scripts/install-grok-anthropic-serve.sh
#   VERSION=v0.1.0 ./scripts/install-grok-anthropic-serve.sh   # pin a tag
#   FORCE=1 ./scripts/install-grok-anthropic-serve.sh          # reinstall even if version matches
#
# Env:
#   INSTALL_DIR  – install destination directory (default: ~/.local/bin)
#   REPO         – GitHub owner/repo (default: caoer/korg)
#   VERSION      – release tag (default: latest)
#   BIN_NAME     – binary name (default: grok-anthropic-serve)
#   FORCE        – set to 1 to re-download even when installed version matches

set -euo pipefail

REPO="${REPO:-caoer/korg}"
VERSION="${VERSION:-latest}"
BIN_NAME="${BIN_NAME:-grok-anthropic-serve}"
INSTALL_DIR="${INSTALL_DIR:-${HOME}/.local/bin}"
FORCE="${FORCE:-0}"

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

# Resolve the desired version string before downloading so we can skip
# when the already-installed binary matches (saves bandwidth on reinstall).
desired_version=""
if [[ "${VERSION}" == "latest" ]]; then
  if manifest="$(curl -fsSL "${base}/latest.json" 2>/dev/null)"; then
    # Prefer python (always available on macOS; common on Linux); fall back to
    # sed so we don't require jq.
    if command -v python3 >/dev/null 2>&1; then
      desired_version="$(printf '%s' "${manifest}" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("version",""))' 2>/dev/null || true)"
    else
      desired_version="$(printf '%s' "${manifest}" | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
    fi
  fi
else
  desired_version="${VERSION#v}"
fi

installed_bin="${INSTALL_DIR}/${BIN_NAME}"
if [[ "${FORCE}" != "1" && -x "${installed_bin}" && -n "${desired_version}" ]]; then
  # clap --version: "grok-anthropic-serve <VERSION>"
  current_version="$("${installed_bin}" --version 2>/dev/null | head -1 | awk '{print $NF}' || true)"
  if [[ -n "${current_version}" && "${current_version}" == "${desired_version}" ]]; then
    echo "Already installed: ${installed_bin} (${current_version}) — matches desired ${desired_version}, skipping download"
    echo "  re-install: FORCE=1 $0   or   FORCE=1 curl ... | bash"
    exit 0
  fi
  if [[ -n "${current_version}" ]]; then
    echo "Upgrading ${BIN_NAME}: ${current_version} → ${desired_version:-unknown}"
  fi
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
install -m 0755 "${tmpdir}/${BIN_NAME}" "${installed_bin}"

# Persist VERSION.txt next to the binary for other tools / version-skip checks.
if [[ -f "${tmpdir}/VERSION.txt" ]]; then
  cp "${tmpdir}/VERSION.txt" "${INSTALL_DIR}/${BIN_NAME}.VERSION.txt"
  echo "Installed metadata:"
  sed 's/^/  /' "${tmpdir}/VERSION.txt" || true
elif [[ -n "${desired_version}" ]]; then
  printf 'version=%s\n' "${desired_version}" > "${INSTALL_DIR}/${BIN_NAME}.VERSION.txt"
fi

echo "Installed ${installed_bin}"
"${installed_bin}" --version 2>/dev/null || true

case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo "note: ${INSTALL_DIR} is not on PATH; add it or move the binary." >&2
    ;;
esac
