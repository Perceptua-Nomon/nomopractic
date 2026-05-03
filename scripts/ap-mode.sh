#!/usr/bin/env bash
# ap-mode.sh — manage the nomon Wi-Fi Soft AP via NetworkManager.
#
# Usage:
#   ap-mode.sh up      — activate the nomon-<last4> hotspot
#   ap-mode.sh down    — deactivate the hotspot
#   ap-mode.sh status  — print current AP state ("up" or "down")
#
# The SSID is derived from the last 4 hex digits of the wlan0 MAC address.
# The WPA2 passphrase is read from NOMON_PAIRING_SECRET_PATH (default:
# /var/lib/nomon/pairing_secret). The AP is served at 192.168.4.1 with
# dnsmasq providing DHCP on 192.168.4.0/24.

set -euo pipefail

PAIRING_SECRET_PATH="${NOMON_PAIRING_SECRET_PATH:-/var/lib/nomon/pairing_secret}"
IFACE="wlan0"
CON_NAME="nomon-softap"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

_get_mac_last4() {
    local mac
    mac=$(cat "/sys/class/net/${IFACE}/address" 2>/dev/null || true)
    if [[ -z "${mac}" ]]; then
        echo "0000"
        return
    fi
    # Strip colons, take last 4 hex chars
    echo "${mac//:/}" | tr '[:upper:]' '[:lower:]' | grep -o '.\{4\}$'
}

_get_ssid() {
    echo "nomon-$(_get_mac_last4)"
}

_get_passphrase() {
    if [[ ! -f "${PAIRING_SECRET_PATH}" ]]; then
        echo "ERROR: pairing secret file not found: ${PAIRING_SECRET_PATH}" >&2
        exit 1
    fi
    local secret
    secret=$(tr -d '[:space:]' < "${PAIRING_SECRET_PATH}")
    if [[ ${#secret} -lt 8 ]]; then
        echo "ERROR: pairing secret must be at least 8 characters (WPA2 minimum)" >&2
        exit 1
    fi
    echo "${secret}"
}

_connection_exists() {
    nmcli -t -f NAME connection show 2>/dev/null | grep -q "^${CON_NAME}$"
}

_connection_active() {
    nmcli -t -f NAME connection show --active 2>/dev/null | grep -q "^${CON_NAME}$"
}

# ---------------------------------------------------------------------------
# Subcommands
# ---------------------------------------------------------------------------

cmd_up() {
    if _connection_active; then
        echo "already up"
        exit 0
    fi

    local ssid passphrase
    ssid=$(_get_ssid)
    passphrase=$(_get_passphrase)

    if _connection_exists; then
        # Connection profile exists but is not active — just activate it.
        nmcli connection up "${CON_NAME}"
    else
        # Create a new hotspot connection profile.
        nmcli connection add \
            type wifi \
            ifname "${IFACE}" \
            con-name "${CON_NAME}" \
            autoconnect no \
            ssid "${ssid}" \
            -- \
            wifi.mode ap \
            wifi-sec.key-mgmt wpa-psk \
            wifi-sec.psk "${passphrase}" \
            ipv4.method shared \
            ipv4.addresses "192.168.4.1/24" \
            ipv6.method disabled

        nmcli connection up "${CON_NAME}"
    fi

    echo "up: SSID=${ssid}"
}

cmd_down() {
    if _connection_active; then
        nmcli connection down "${CON_NAME}"
        echo "down"
    else
        echo "already down"
    fi
}

cmd_status() {
    if _connection_active; then
        echo "up"
    else
        echo "down"
    fi
}

# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

case "${1:-}" in
    up)     cmd_up ;;
    down)   cmd_down ;;
    status) cmd_status ;;
    *)
        echo "Usage: $(basename "$0") up|down|status" >&2
        exit 1
        ;;
esac
