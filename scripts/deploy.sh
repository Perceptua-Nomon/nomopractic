#!/usr/bin/env bash
# deploy.sh — Download, verify, atomically install, and restart nomopractic.
#
# Usage:
#   ./scripts/deploy.sh <version> [<pi-host>]
#
# Arguments:
#   version   Git tag to deploy, e.g. "v0.2.0"
#   pi-host   SSH host (user@host or plain hostname). Defaults to
#             NOMON_PI_HOST env var. If empty, runs locally on the Pi.
#
# Environment:
#   NOMON_PI_HOST       SSH target (overridden by second arg)
#   NOMON_SSH_KEY       Path to SSH private key (optional)
#   NOMON_GITHUB_REPO   GitHub "owner/repo" slug. Default: Perceptua/nomopractic
#
# Examples:
#   # Deploy from a dev machine to a Pi over SSH:
#   ./scripts/deploy.sh v0.2.0 pi@raspberrypi.local
#
#   # Deploy directly on the Pi (scp the script first):
#   ./scripts/deploy.sh v0.2.0
#
# The script:
#   1. Downloads the binary + SHA-256 checksum from GitHub Releases.
#   2. Verifies the checksum.
#   3. Atomically replaces /usr/local/bin/nomopractic (keeps previous as .bak).
#   4. Restarts nomopractic.service.
#   5. Verifies the service is running (waits up to 10 s).
#
# Rollback is available by running:
#   sudo cp /usr/local/bin/nomopractic.bak /usr/local/bin/nomopractic
#   sudo systemctl restart nomopractic
#
# Exit codes:
#   0  success
#   1  usage error
#   2  download / checksum failure
#   3  installation / service failure

set -euo pipefail

# ── Constants ────────────────────────────────────────────────────────────────

REPO="${NOMON_GITHUB_REPO:-Perceptua/nomopractic}"
INSTALL_PATH="/usr/local/bin/nomopractic"
SERVICE="nomopractic"
DOWNLOAD_DIR="$(mktemp -d)"

cleanup() {
    rm -rf "$DOWNLOAD_DIR"
}
trap cleanup EXIT

# ── Argument parsing ─────────────────────────────────────────────────────────

if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <version> [<pi-host>]" >&2
    exit 1
fi

VERSION="$1"
PI_HOST="${2:-${NOMON_PI_HOST:-}}"

# Validate version format.
if [[ ! "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+ ]]; then
    echo "Error: version must start with 'v' followed by semver (e.g. v0.2.0)" >&2
    exit 1
fi

# ── SSH helper ───────────────────────────────────────────────────────────────
# If PI_HOST is set we run everything remotely; otherwise we run locally.

if [[ -n "$PI_HOST" ]]; then
    SSH_OPTS=(-o StrictHostKeyChecking=accept-new -o ConnectTimeout=15)
    if [[ -n "${NOMON_SSH_KEY:-}" ]]; then
        SSH_OPTS+=(-i "$NOMON_SSH_KEY")
    fi
    run_remote() {
        # shellcheck disable=SC2029
        ssh "${SSH_OPTS[@]}" "$PI_HOST" "bash -s" <<'REMOTE_SCRIPT'
set -euo pipefail
REMOTE_SCRIPT
        # Pass the function body as a heredoc
        # Alternative: inline mode used below via _remote_cmd
        :
    }
    _rsh() { ssh "${SSH_OPTS[@]}" "$PI_HOST" "$@"; }
    _rscp() { scp "${SSH_OPTS[@]}" "$@"; }
    ON_REMOTE=true
else
    _rsh() { bash -c "$@"; }
    _rscp() { cp "$1" "$2"; }  # local "scp" = cp
    ON_REMOTE=false
fi

# ── Download ─────────────────────────────────────────────────────────────────

BINARY_NAME="nomopractic-${VERSION}-aarch64-linux"
BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"
BINARY_URL="${BASE_URL}/${BINARY_NAME}"
SHA256_URL="${BASE_URL}/${BINARY_NAME}.sha256"

BINARY_FILE="${DOWNLOAD_DIR}/${BINARY_NAME}"
SHA256_FILE="${DOWNLOAD_DIR}/${BINARY_NAME}.sha256"

echo "==> Downloading nomopractic ${VERSION} from GitHub Releases..."
curl --fail --location --progress-bar --output "$BINARY_FILE" "$BINARY_URL"
curl --fail --location --silent     --output "$SHA256_FILE" "$SHA256_URL"

# ── Checksum verification ─────────────────────────────────────────────────────

echo "==> Verifying SHA-256 checksum..."
(
    cd "$DOWNLOAD_DIR"
    # The .sha256 file may contain the full path; normalise to just the filename.
    EXPECTED="$(awk '{print $1}' "$SHA256_FILE")  ${BINARY_NAME}"
    echo "$EXPECTED" | sha256sum --check --status || {
        echo "Error: checksum mismatch for ${BINARY_FILE}" >&2
        exit 2
    }
)
echo "    OK: checksum verified"

chmod +x "$BINARY_FILE"

# ── Remote or local installation ─────────────────────────────────────────────

install_binary() {
    local src="$1"

    # Keep the current binary as a rollback target.
    if [[ -f "${INSTALL_PATH}" ]]; then
        sudo cp "${INSTALL_PATH}" "${INSTALL_PATH}.bak"
        echo "==> Previous binary saved to ${INSTALL_PATH}.bak"
    fi

    # Atomic swap: write to a temp file in the same directory, then rename.
    local tmp
    tmp="$(dirname "${INSTALL_PATH}")/.nomopractic.tmp.$$"
    sudo cp "$src" "$tmp"
    sudo chmod 755 "$tmp"
    sudo mv -f "$tmp" "${INSTALL_PATH}"
    echo "==> Installed to ${INSTALL_PATH}"
}

echo "==> Installing binary..."

if [[ "$ON_REMOTE" == true ]]; then
    # Copy the verified binary to the Pi and run the install+restart there.
    REMOTE_TMP="/tmp/nomopractic.$$"
    _rscp "$BINARY_FILE" "${PI_HOST}:${REMOTE_TMP}"
    _rsh bash <<REMOTE
set -euo pipefail
INSTALL_PATH="${INSTALL_PATH}"
SERVICE="${SERVICE}"
REMOTE_TMP="${REMOTE_TMP}"

if [[ -f "\${INSTALL_PATH}" ]]; then
    sudo cp "\${INSTALL_PATH}" "\${INSTALL_PATH}.bak"
    echo "==> Previous binary saved to \${INSTALL_PATH}.bak"
fi

tmp="\$(dirname "\${INSTALL_PATH}")/.nomopractic.tmp.\$\$"
sudo cp "\${REMOTE_TMP}" "\${tmp}"
sudo chmod 755 "\${tmp}"
sudo mv -f "\${tmp}" "\${INSTALL_PATH}"
rm -f "\${REMOTE_TMP}"
echo "==> Installed to \${INSTALL_PATH}"

echo "==> Restarting \${SERVICE}.service..."
sudo systemctl daemon-reload
sudo systemctl restart "\${SERVICE}"
sudo systemctl enable  "\${SERVICE}"

echo "==> Waiting for service to become active..."
for i in \$(seq 1 10); do
    if systemctl is-active --quiet "\${SERVICE}"; then
        echo "==> \${SERVICE} is running (version ${VERSION})"
        exit 0
    fi
    sleep 1
done

echo "Error: \${SERVICE} did not start within 10 seconds" >&2
sudo journalctl -u "\${SERVICE}" -n 30 --no-pager >&2
exit 3
REMOTE

else
    # Local install (running on the Pi directly).
    install_binary "$BINARY_FILE"

    echo "==> Restarting ${SERVICE}.service..."
    sudo systemctl daemon-reload
    sudo systemctl restart "${SERVICE}"
    sudo systemctl enable  "${SERVICE}"

    echo "==> Waiting for service to become active..."
    for i in $(seq 1 10); do
        if systemctl is-active --quiet "${SERVICE}"; then
            echo "==> ${SERVICE} is running (version ${VERSION})"
            exit 0
        fi
        sleep 1
    done

    echo "Error: ${SERVICE} did not start within 10 seconds" >&2
    sudo journalctl -u "${SERVICE}" -n 30 --no-pager >&2
    exit 3
fi
