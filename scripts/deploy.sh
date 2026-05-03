#!/usr/bin/env bash
# deploy.sh — Build locally or download from GitHub, verify, atomically install, and restart nomopractic.
#
# Usage:
#   ./scripts/deploy.sh [--local] [<version>] [<pi-host>]
#
# Arguments:
#   --local   Build and deploy the current local code (aarch64 cross-compile).
#             Bypasses GitHub release downloads.
#   version   Git tag to deploy, e.g. "v0.2.0". If omitted, the script fetches
#             and deploys the latest release from GitHub. Ignored if --local.
#   pi-host   SSH host (user@host or plain hostname). Defaults to
#             NOMON_PI_HOST env var. If empty, runs locally on the Pi.
#
# Environment:
#   NOMON_PI_HOST       SSH target (overridden by pi-host arg)
#   NOMON_SSH_KEY       Path to SSH private key (optional)
#   NOMON_GITHUB_REPO   GitHub "owner/repo" slug. Default: Perceptua-Nomon/nomopractic
#
# Examples:
#   # Deploy local code from a dev machine to a Pi over SSH:
#   ./scripts/deploy.sh --local pi@raspberrypi.local
#
#   # Deploy latest release from a dev machine to a Pi over SSH:
#   ./scripts/deploy.sh pi@raspberrypi.local
#
#   # Deploy a specific version from a dev machine to a Pi over SSH:
#   ./scripts/deploy.sh v0.2.0 pi@raspberrypi.local
#
#   # Deploy local code directly on the Pi (scp the script first):
#   ./scripts/deploy.sh --local
#
# The script (release mode):
#   1. Downloads the binary + SHA-256 checksum from GitHub Releases.
#   2. Verifies the checksum.
#   3. Atomically replaces /usr/local/bin/nomopractic (keeps previous as .bak).
#   4. Restarts nomopractic.service.
#   5. Verifies the service is running (waits up to 10 s).
#
# The script (--local mode):
#   1. Cross-compiles the current local code for aarch64 (if not already built).
#   2. Copies the binary to the Pi.
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
#   2  download / checksum / build failure
#   3  installation / service failure

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(dirname "${SCRIPT_DIR}")"

# ── Constants ────────────────────────────────────────────────────────────────

REPO="${NOMON_GITHUB_REPO:-Perceptua-Nomon/nomopractic}"
INSTALL_PATH="/usr/local/bin/nomopractic"
SERVICE="nomopractic"
DOWNLOAD_DIR="$(mktemp -d)"

cleanup() {
    rm -rf "$DOWNLOAD_DIR"
}
trap cleanup EXIT

# Deploy-only variables that must NOT be written to the on-device env file.
_DEPLOY_EXCLUDE='^\s*(NOMON_PI_HOST|NOMON_SSH_KEY|NOMON_GITHUB_REPO)\s*='

# ── Load .env ──────────────────────────────────────────────────────────────────

ENV_FILE="${REPO_DIR}/.env"
if [[ -f "${ENV_FILE}" ]]; then
    while IFS= read -r line || [[ -n "${line}" ]]; do
        line="${line#"${line%%[![:space:]]*}"}"
        [[ "${line}" =~ ^# || -z "${line}" ]] && continue
        key="${line%%=*}"
        val="${line#*=}"
        val="${val%%#*}"
        val="${val#"${val%%[![:space:]]*}"}"
        val="${val%"${val##*[![:space:]]}"}"
        val="${val#\"}" ; val="${val%\"}"
        val="${val#\'}"; val="${val%\'}"
        case "${key}" in
            NOMON_PI_HOST|NOMON_SSH_KEY|NOMON_GITHUB_REPO|NOMON_SERVICE_USER|NOMON_SERVICE_GROUP)
                export "${key}=${val}" ;;
        esac
    done < "${ENV_FILE}"
fi

# Sanitize env-derived host/key values to remove any stray CR/LF or surrounding
# whitespace that may be present when editing .env on Windows or other editors.
NOMON_PI_HOST="${NOMON_PI_HOST:-}"
NOMON_SSH_KEY="${NOMON_SSH_KEY:-}"
# Remove CR and LF, trim leading/trailing whitespace
NOMON_PI_HOST="$(printf '%s' "${NOMON_PI_HOST}" | tr -d '\r\n' | sed -E 's/^[[:space:]]+|[[:space:]]+$//g')"
NOMON_SSH_KEY="$(printf '%s' "${NOMON_SSH_KEY}" | tr -d '\r\n' | sed -E 's/^[[:space:]]+|[[:space:]]+$//g')"

# Export cleaned values so later logic picks them up via ${NOMON_PI_HOST} etc.
export NOMON_PI_HOST NOMON_SSH_KEY


# ── Argument parsing ─────────────────────────────────────────────────────────

BUILD_LOCAL=false
VERSION="${1:-}"
PI_HOST="${2:-${NOMON_PI_HOST:-}}"

# Check if first argument is --local flag
if [[ "${VERSION}" == "--local" ]]; then
    BUILD_LOCAL=true
    VERSION=""
    PI_HOST="${2:-${NOMON_PI_HOST:-}}"
fi

# Sanitize PI_HOST (remove any stray CR/LF and surrounding whitespace)
PI_HOST="$(printf '%s' "${PI_HOST}" | tr -d '\r\n' | sed -E 's/^[[:space:]]+|[[:space:]]+$//g')"

if [[ -n "${VERSION}" && ! "${VERSION}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Error: version must start with 'v' followed by semver (e.g. v0.2.0)" >&2
    exit 1
fi

# ── Resolve version / Build ──────────────────────────────────────────────────

if [[ "${BUILD_LOCAL}" == true ]]; then
    # Get version from Cargo.toml
    VERSION="$(grep -m1 'version' Cargo.toml | sed -E 's/.*version\s*=\s*"([^"]+)".*/\1/')"
    if [[ -z "${VERSION}" ]]; then
        echo "Error: could not determine version from Cargo.toml" >&2
        exit 2
    fi
    echo "==> Building nomopractic ${VERSION} locally for aarch64..."
    
    # Cross-compile for aarch64
    if ! command -v cross &> /dev/null; then
        echo "Error: 'cross' command not found. Install it with: cargo install cross" >&2
        exit 2
    fi

    # If CROSS_CONTAINER is set, export it so `cross` will use that container image.
    if [[ -n "${CROSS_CONTAINER:-}" ]]; then
        export CROSS_CONTAINER="${CROSS_CONTAINER}"
        echo "==> Using cross container: ${CROSS_CONTAINER}"
    fi

    # Allow passing additional cargo flags (e.g. --features ble)
    if ! cross build --target aarch64-unknown-linux-gnu --release ${CARGO_BUILD_FLAGS:-}; then
        echo "Error: cross-compilation failed" >&2
        exit 2
    fi
    
    BINARY_FILE="target/aarch64-unknown-linux-gnu/release/nomopractic"
    if [[ ! -f "${BINARY_FILE}" ]]; then
        echo "Error: compiled binary not found at ${BINARY_FILE}" >&2
        exit 2
    fi
    
    DISPLAY_VERSION="${VERSION}"
else
    # ── Resolve version ───────────────────────────────────────────────────────

    if [[ -z "${VERSION}" ]]; then
        echo "==> Resolving latest release from GitHub..."
        VERSION="$(curl -sf "https://api.github.com/repos/${REPO}/releases/latest" \
            | python3 -c 'import sys,json; print(json.load(sys.stdin)["tag_name"])')"
        if [[ -z "${VERSION}" ]]; then
            echo "Error: could not determine latest release from GitHub." >&2
            exit 1
        fi
        echo "  Latest release: ${VERSION}"
    fi

    # ── Download ─────────────────────────────────────────────────────────────

    BINARY_NAME="nomopractic-${VERSION}-aarch64-linux"
    BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"
    BINARY_URL="${BASE_URL}/${BINARY_NAME}"
    SHA256_URL="${BASE_URL}/${BINARY_NAME}.sha256"

    BINARY_FILE="${DOWNLOAD_DIR}/${BINARY_NAME}"
    SHA256_FILE="${DOWNLOAD_DIR}/${BINARY_NAME}.sha256"

    echo "==> Downloading nomopractic ${VERSION} from GitHub Releases..."
    curl --fail --location --progress-bar --output "$BINARY_FILE" "$BINARY_URL"
    curl --fail --location --silent     --output "$SHA256_FILE" "$SHA256_URL"

    # ── Checksum verification ─────────────────────────────────────────────────

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
    DISPLAY_VERSION="${VERSION}"
fi

# ── SSH helper ───────────────────────────────────────────────────────────────
# If PI_HOST is set we run everything remotely; otherwise we run locally.

if [[ -n "$PI_HOST" ]]; then
    SSH_OPTS=(-o StrictHostKeyChecking=accept-new -o ConnectTimeout=15)
    if [[ -n "${NOMON_SSH_KEY:-}" ]]; then
        SSH_OPTS+=(-i "$NOMON_SSH_KEY")
    fi
    _rsh() { ssh "${SSH_OPTS[@]}" "$PI_HOST" "$@"; }
    _rscp() { scp "${SSH_OPTS[@]}" "$@"; }
    ON_REMOTE=true
else
    _rsh() { bash -c "$@"; }
    _rscp() { cp "$1" "$2"; }  # local "scp" = cp
    ON_REMOTE=false
fi
copy_nomopractic_env() {
    if [[ ! -f "${ENV_FILE}" ]]; then
        echo "==> Warning: .env not found; skipping /etc/nomopractic/nomopractic.env creation." >&2
        return
    fi

    local filtered
    filtered="$(grep -vE "${_DEPLOY_EXCLUDE}" "${ENV_FILE}" \
        | grep -vE '^\s*#' \
        | grep -vE '^\s*$' || true)"

    if [[ -z "${filtered}" ]]; then
        echo "==> No deploy-safe env vars found; skipping /etc/nomopractic/nomopractic.env creation." >&2
        return
    fi

    if [[ -n "${PI_HOST}" ]]; then
        echo "==> Writing /etc/nomopractic/nomopractic.env on remote host..."
        # Write to a local temp file, copy it to the remote host, then move into place with sudo.
        local tmp_env_file
        tmp_env_file="$(mktemp)"
        printf '%s\n' "${filtered}" > "${tmp_env_file}"
        _rscp "${tmp_env_file}" "${PI_HOST}:/tmp/nomopractic_env.$$"
        _rsh "sudo mkdir -p /etc/nomopractic && sudo mv -f /tmp/nomopractic_env.$$ /etc/nomopractic/nomopractic.env && sudo chmod 644 /etc/nomopractic/nomopractic.env"
        rm -f "${tmp_env_file}"
    else
        echo "==> Writing /etc/nomopractic/nomopractic.env locally..."
        sudo mkdir -p /etc/nomopractic
        printf '%s\n' "${filtered}" | sudo tee /etc/nomopractic/nomopractic.env > /dev/null
    fi
}
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

copy_nomopractic_env

REMOTE_SVC_TMP="/tmp/nomopractic_service.$$"
REMOTE_TMPFILES="/tmp/nomon_tmpfiles.conf.$$"
if [[ "$ON_REMOTE" == true ]]; then
    _rscp "${REPO_DIR}/systemd/nomopractic.service" "${PI_HOST}:${REMOTE_SVC_TMP}"
    # copy tmpfiles entry so remote deploy installs it
    _rscp "${REPO_DIR}/systemd/tmpfiles.d/nomon.conf" "${PI_HOST}:${REMOTE_TMPFILES}"
    _rscp "${REPO_DIR}/systemd/nomon-softap.service"           "${PI_HOST}:/tmp/nomon-softap.service.$$"
    _rscp "${REPO_DIR}/systemd/nomon-softap-watchdog.service"  "${PI_HOST}:/tmp/nomon-softap-watchdog.service.$$"
    _rscp "${REPO_DIR}/systemd/nomon-softap-watchdog.timer"    "${PI_HOST}:/tmp/nomon-softap-watchdog.timer.$$"
fi

echo "==> Installing binary..."

if [[ "$ON_REMOTE" == true ]]; then
    # Copy the verified binary to the Pi and run the install+restart there.
    REMOTE_TMP="/tmp/nomopractic.$$"
    _rscp "$BINARY_FILE" "${PI_HOST}:${REMOTE_TMP}"
    # If a local config.toml exists, upload it to the Pi for installation
    REMOTE_CONFIG_TMP=""
    if [[ -f "config.toml" ]]; then
        REMOTE_CONFIG_TMP="/tmp/nomopractic_config.$$"
        _rscp "config.toml" "${PI_HOST}:${REMOTE_CONFIG_TMP}"
    fi
    _rsh bash <<REMOTE
set -euo pipefail
INSTALL_PATH="${INSTALL_PATH}"
SERVICE="${SERVICE}"
REMOTE_TMP="${REMOTE_TMP}"
REMOTE_CONFIG_TMP="${REMOTE_CONFIG_TMP}"
REMOTE_SVC_TMP="${REMOTE_SVC_TMP}"
REMOTE_TMPFILES="${REMOTE_TMPFILES}"
_DEF_SVC_USER="${NOMON_SERVICE_USER:-root}"
_DEF_SVC_GROUP="${NOMON_SERVICE_GROUP:-nomon}"

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

# ── Install systemd service file ─────────────────────────────────────────────
echo "==> Installing nomopractic.service..."
if ! command -v envsubst >/dev/null 2>&1; then
    echo "Error: envsubst not found. Install gettext-base: sudo apt-get install -y gettext-base" >&2
    exit 3
fi

# Source on-device env for variable substitution; fall back to script defaults.
if [[ -f /etc/nomopractic/nomopractic.env ]]; then
    set -o allexport
    # shellcheck disable=SC1091
    source /etc/nomopractic/nomopractic.env
    set +o allexport
fi
export NOMON_SERVICE_USER="\${NOMON_SERVICE_USER:-\${_DEF_SVC_USER}}"
export NOMON_SERVICE_GROUP="\${NOMON_SERVICE_GROUP:-\${_DEF_SVC_GROUP}}"

_expanded="\$(envsubst '\$NOMON_SERVICE_USER \$NOMON_SERVICE_GROUP' < "\${REMOTE_SVC_TMP}")"
printf '%s\n' "\${_expanded}" | sudo tee /etc/systemd/system/nomopractic.service > /dev/null
sudo chmod 644 /etc/systemd/system/nomopractic.service
rm -f "${REMOTE_SVC_TMP}"
echo "==> nomopractic.service installed ✓"

# ── Install Soft AP systemd units ────────────────────────────────────────────
echo "==> Installing Soft AP systemd units..."
for _unit in nomon-softap.service nomon-softap-watchdog.service nomon-softap-watchdog.timer; do
    sudo cp "/tmp/${_unit}.$$" "/etc/systemd/system/${_unit}"
    sudo chmod 644 "/etc/systemd/system/${_unit}"
    echo "==> ${_unit} installed ✓"
done
sudo systemctl enable nomon-softap-watchdog.timer

# Install tmpfiles.d entry for /var/lib/nomon (if provided)
if [[ -f "${REMOTE_TMPFILES}" ]]; then
    echo "==> Installing tmpfiles.d/nomon.conf..."
    sudo mkdir -p /etc/tmpfiles.d
    sudo mv -f "${REMOTE_TMPFILES}" /etc/tmpfiles.d/nomon.conf
    sudo chmod 644 /etc/tmpfiles.d/nomon.conf
    sudo systemd-tmpfiles --create /etc/tmpfiles.d/nomon.conf || true
fi

# Ensure /var/lib/nomon is owned by the service user/group so nomopractic
# can read the pairing secret file. Use the configured service user/group
# (defaults on the remote host will be substituted at runtime).
sudo chown -R "\${NOMON_SERVICE_USER:-root}:\${NOMON_SERVICE_GROUP:-nomon}" /var/lib/nomon || true

# If a config was uploaded, atomically install it to /etc/nomopractic
if [[ -n "\${REMOTE_CONFIG_TMP}" ]]; then
    echo "==> Installing config to /etc/nomopractic/config.toml..."
    sudo mkdir -p /etc/nomopractic
    if [[ -f /etc/nomopractic/config.toml ]]; then
        sudo cp /etc/nomopractic/config.toml /etc/nomopractic/config.toml.bak
        echo "==> Previous config saved to /etc/nomopractic/config.toml.bak"
    fi
    sudo mv -f "\${REMOTE_CONFIG_TMP}" /etc/nomopractic/config.toml
    sudo chmod 644 /etc/nomopractic/config.toml
    echo "==> Installed config to /etc/nomopractic/config.toml"
fi

echo "==> Restarting \${SERVICE}.service..."
sudo systemctl daemon-reload
sudo systemctl restart "\${SERVICE}"
sudo systemctl enable  "\${SERVICE}"

echo "==> Waiting for service to become active..."
for _ in \$(seq 1 10); do
    if systemctl is-active --quiet "\${SERVICE}"; then
        echo "==> \${SERVICE} is running (version ${DISPLAY_VERSION})"
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

    # If a local config.toml exists, install it to /etc/nomopractic
    if [[ -f "config.toml" ]]; then
        echo "==> Installing local config to /etc/nomopractic/config.toml..."
        sudo mkdir -p /etc/nomopractic
        if [[ -f /etc/nomopractic/config.toml ]]; then
            sudo cp /etc/nomopractic/config.toml /etc/nomopractic/config.toml.bak
            echo "==> Previous config saved to /etc/nomopractic/config.toml.bak"
        fi
        sudo cp config.toml /etc/nomopractic/config.toml
        sudo chmod 644 /etc/nomopractic/config.toml
        echo "==> Installed config to /etc/nomopractic/config.toml"
    fi

    # ── Install systemd service file ─────────────────────────────────────────────
    echo "==> Installing nomopractic.service..."
    if ! command -v envsubst >/dev/null 2>&1; then
        echo "Error: envsubst not found. Install gettext-base: sudo apt-get install -y gettext-base" >&2
        exit 3
    fi

    # Source on-device env; fall back to script defaults.
    if [[ -f /etc/nomopractic/nomopractic.env ]]; then
        set -o allexport
        # shellcheck disable=SC1091
        source /etc/nomopractic/nomopractic.env
        set +o allexport
    fi
    export NOMON_SERVICE_USER="${NOMON_SERVICE_USER:-root}"
    export NOMON_SERVICE_GROUP="${NOMON_SERVICE_GROUP:-nomon}"

    _expanded="$(envsubst '$NOMON_SERVICE_USER $NOMON_SERVICE_GROUP' < "${REPO_DIR}/systemd/nomopractic.service")"
    printf '%s\n' "${_expanded}" | sudo tee /etc/systemd/system/nomopractic.service > /dev/null
    sudo chmod 644 /etc/systemd/system/nomopractic.service
    echo "==> nomopractic.service installed ✓"

    # ── Install Soft AP systemd units ────────────────────────────────────────────
    echo "==> Installing Soft AP systemd units..."
    for _unit in nomon-softap.service nomon-softap-watchdog.service nomon-softap-watchdog.timer; do
        sudo cp "${REPO_DIR}/systemd/${_unit}" "/etc/systemd/system/${_unit}"
        sudo chmod 644 "/etc/systemd/system/${_unit}"
        echo "==> ${_unit} installed ✓"
    done
    sudo systemctl enable nomon-softap-watchdog.timer

    # Install tmpfiles.d entry for /var/lib/nomon so the pairing secret
    # directory exists with correct owner/permissions on boot.
    echo "==> Installing tmpfiles.d/nomon.conf..."
    sudo mkdir -p /etc/tmpfiles.d
    sudo cp "${REPO_DIR}/systemd/tmpfiles.d/nomon.conf" /etc/tmpfiles.d/nomon.conf
    sudo chmod 644 /etc/tmpfiles.d/nomon.conf
    sudo systemd-tmpfiles --create /etc/tmpfiles.d/nomon.conf || true

    # Ensure /var/lib/nomon is owned by the service user/group so the
    # nomopractic process can read the pairing secret file.
    sudo chown -R "${NOMON_SERVICE_USER:-root}:${NOMON_SERVICE_GROUP:-nomon}" /var/lib/nomon || true

    echo "==> Restarting ${SERVICE}.service..."
    sudo systemctl daemon-reload
    sudo systemctl restart "${SERVICE}"
    sudo systemctl enable  "${SERVICE}"

    echo "==> Waiting for service to become active..."
    for _ in $(seq 1 10); do
        if systemctl is-active --quiet "${SERVICE}"; then
            echo "==> ${SERVICE} is running (version ${DISPLAY_VERSION})"
            exit 0
        fi
        sleep 1
    done

    echo "Error: ${SERVICE} did not start within 10 seconds" >&2
    sudo journalctl -u "${SERVICE}" -n 30 --no-pager >&2
    exit 3
fi
