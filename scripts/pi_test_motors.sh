#!/usr/bin/env bash
# pi_test_motors.sh — Motor IPC integration tests against a running Pi daemon.
#
# Requires the nomopractic service to be running on the target host.
# Tests communicate via the Unix socket using socat (must be installed on Pi).
#
# Usage:
#   ./scripts/pi_test_motors.sh [pi-host]
#
# Arguments:
#   pi-host   SSH host (user@host). Defaults to NOMON_PI_HOST env var.
#
# Environment:
#   NOMON_PI_HOST   SSH target if not provided as argument.
#   NOMON_SSH_KEY   Path to SSH private key (optional).
#
# Exit codes:
#   0  all tests passed
#   1  one or more tests failed

set -euo pipefail

SOCK="/run/nomopractic/nomopractic.sock"
PI_HOST="${1:-${NOMON_PI_HOST:-}}"

if [[ -z "$PI_HOST" ]]; then
    echo "Error: pi-host is required (argument or NOMON_PI_HOST env var)" >&2
    exit 1
fi

SSH_CTL="$(mktemp -u /tmp/nomon-pi-test-XXXXXX)"
SSH_OPTS=(
    -o StrictHostKeyChecking=accept-new
    -o ConnectTimeout=15
    -o ControlMaster=auto
    -o ControlPath="$SSH_CTL"
    -o ControlPersist=60
)
[[ -n "${NOMON_SSH_KEY:-}" ]] && SSH_OPTS+=(-i "$NOMON_SSH_KEY")

# Open the control master connection once (prompts for password if needed).
echo "    Connecting to $PI_HOST ..."
ssh "${SSH_OPTS[@]}" "$PI_HOST" true || { echo "Error: cannot reach $PI_HOST" >&2; exit 3; }
cleanup_ssh() { ssh -O exit -o ControlPath="$SSH_CTL" "$PI_HOST" 2>/dev/null || true; }
trap cleanup_ssh EXIT

PASS=0
FAIL=0

# ── Helpers ──────────────────────────────────────────────────────────────────

# Send one NDJSON request and capture the single-line response.
ipc_one() {
    ssh "${SSH_OPTS[@]}" "$PI_HOST" \
        "printf '%s\n' $(printf '%q' "$1") | socat - UNIX-CONNECT:$SOCK"
}

# Send N NDJSON requests (one per argument) and capture N response lines.
# Returns all lines as newline-separated output.
ipc_seq() {
    local payload=""
    for msg in "$@"; do
        payload+="${msg}"$'\n'
    done
    ssh "${SSH_OPTS[@]}" "$PI_HOST" \
        "printf '%s' $(printf '%q' "$payload") | socat - UNIX-CONNECT:$SOCK"
}

# Extract a field value from a JSON-ish line (no jq required).
# Usage: json_get '{"ok":true,"result":{"stopped":2}}' result.stopped
json_get() {
    python3 -c "
import json, sys
obj = json.loads(sys.argv[1])
keys = sys.argv[2].split('.')
for k in keys:
    obj = obj[k]
print(obj)
" "$1" "$2" 2>/dev/null || echo "<parse_error>"
}

run_test() {
    local name="$1" request="$2" check_fn="${3:-ok_true}"
    local resp
    resp=$(ipc_one "$request" 2>/dev/null) || { resp='{"ok":false,"error":{"code":"CONNECTION_ERROR","message":"socat failed"}}'; }
    if $check_fn "$resp"; then
        echo "  PASS  $name"
        ((PASS++)) || true
    else
        echo "  FAIL  $name"
        echo "        response: $resp"
        ((FAIL++)) || true
    fi
}

ok_true()  { echo "$1" | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if d.get('ok') else 1)" 2>/dev/null; }
ok_false() { echo "$1" | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if not d.get('ok') else 1)" 2>/dev/null; }

# ── Pre-flight ────────────────────────────────────────────────────────────────

echo "==> Motor IPC integration tests — target: $PI_HOST"
echo "    Socket: $SOCK"
echo ""

# ── T1: Health ────────────────────────────────────────────────────────────────

run_test "T1 Health" \
    '{"id":"t1","method":"health","params":{}}'

# ── T2: Get motor status (baseline — should be empty) ─────────────────────────

RESP_T2=$(ipc_one '{"id":"t2","method":"get_motor_status","params":{}}' 2>/dev/null) \
    || RESP_T2='{"ok":false}'
LEASE_COUNT=$(python3 -c "import json,sys; d=json.loads(sys.argv[1]); print(len(d.get('result',{}).get('active_leases',[])))" "$RESP_T2" 2>/dev/null || echo "-1")
if echo "$RESP_T2" | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if d.get('ok') else 1)" 2>/dev/null; then
    echo "  PASS  T2 Motor status baseline (${LEASE_COUNT} active leases)"
    ((PASS++)) || true
else
    echo "  FAIL  T2 Motor status baseline"
    echo "        response: $RESP_T2"
    ((FAIL++)) || true
fi

# ── T3: Set motor 0 forward at 40% and verify response ───────────────────────
# Note: single-shot socat disconnects immediately, triggering on_client_disconnect
# which safely idles the motor. The IPC response is still validated below.

run_test "T3 Motor 0 forward 40%" \
    '{"id":"t3","method":"set_motor_speed","params":{"channel":0,"speed_pct":40.0,"ttl_ms":5000}}'

# ── T4: Motor 0 reverse, Motor 1 forward — both in one connection ─────────────
# We pipe two requests in a single socat session so both channels hold their
# lease long enough for the status query below to see them.

COMBINED=$(ipc_seq \
    '{"id":"t4a","method":"set_motor_speed","params":{"channel":0,"speed_pct":-30.0,"ttl_ms":5000}}' \
    '{"id":"t4b","method":"set_motor_speed","params":{"channel":1,"speed_pct":30.0,"ttl_ms":5000}}' \
    '{"id":"t4c","method":"get_motor_status","params":{}}' \
    2>/dev/null) || COMBINED=""

RESP_T4A=$(echo "$COMBINED" | sed -n '1p')
RESP_T4B=$(echo "$COMBINED" | sed -n '2p')
RESP_T4C=$(echo "$COMBINED" | sed -n '3p')

if echo "$RESP_T4A" | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if d.get('ok') else 1)" 2>/dev/null; then
    echo "  PASS  T4a Motor 0 reverse 30%"
    ((PASS++)) || true
else
    echo "  FAIL  T4a Motor 0 reverse 30%"
    echo "        response: $RESP_T4A"
    ((FAIL++)) || true
fi

if echo "$RESP_T4B" | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if d.get('ok') else 1)" 2>/dev/null; then
    echo "  PASS  T4b Motor 1 forward 30%"
    ((PASS++)) || true
else
    echo "  FAIL  T4b Motor 1 forward 30%"
    echo "        response: $RESP_T4B"
    ((FAIL++)) || true
fi

ACTIVE=$(python3 -c "import json,sys; d=json.loads(sys.argv[1]); print(len(d.get('result',{}).get('active_leases',[])))" "$RESP_T4C" 2>/dev/null || echo "-1")
if [[ "$ACTIVE" == "2" ]]; then
    echo "  PASS  T4c Motor status shows 2 active leases"
    ((PASS++)) || true
else
    echo "  FAIL  T4c Motor status shows 2 active leases (got: $ACTIVE)"
    echo "        response: $RESP_T4C"
    ((FAIL++)) || true
fi

# ── T5: Stop all motors ──────────────────────────────────────────────────────

RESP_T5=$(ipc_one '{"id":"t5","method":"stop_all_motors","params":{}}' 2>/dev/null) \
    || RESP_T5='{"ok":false}'
STOPPED=$(json_get "$RESP_T5" "result.stopped" 2>/dev/null || echo "-1")
if echo "$RESP_T5" | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if d.get('ok') else 1)" 2>/dev/null; then
    echo "  PASS  T5 Stop all motors (stopped: $STOPPED)"
    ((PASS++)) || true
else
    echo "  FAIL  T5 Stop all motors"
    echo "        response: $RESP_T5"
    ((FAIL++)) || true
fi

# ── T6: Motor status after stop (leases should be cleared) ───────────────────

RESP_T6=$(ipc_one '{"id":"t6","method":"get_motor_status","params":{}}' 2>/dev/null) \
    || RESP_T6='{"ok":false}'
LEASE_COUNT_T6=$(python3 -c "import json,sys; d=json.loads(sys.argv[1]); print(len(d.get('result',{}).get('active_leases',[])))" "$RESP_T6" 2>/dev/null || echo "-1")
if [[ "$LEASE_COUNT_T6" == "0" ]]; then
    echo "  PASS  T6 Motor status after stop (0 active leases)"
    ((PASS++)) || true
else
    echo "  FAIL  T6 Motor status after stop (expected 0, got: $LEASE_COUNT_T6)"
    echo "        response: $RESP_T6"
    ((FAIL++)) || true
fi

# ── T7: Invalid channel ───────────────────────────────────────────────────────

run_test "T7 Invalid motor channel (expect INVALID_PARAMS)" \
    '{"id":"t7","method":"set_motor_speed","params":{"channel":9,"speed_pct":50.0}}' \
    ok_false

# ── T8: Speed out of range ────────────────────────────────────────────────────

run_test "T8 Speed out of range (expect INVALID_PARAMS)" \
    '{"id":"t8","method":"set_motor_speed","params":{"channel":0,"speed_pct":200.0}}' \
    ok_false

# ── T9: TTL watchdog — set speed with minimum TTL, wait, verify lease expires ─
# Set motor with 100 ms TTL (minimum). The watchdog polls every watchdog_poll_ms
# (default 100 ms), so the motor should be idled within ~200 ms.

WATCHDOG=$(ipc_seq \
    '{"id":"t9a","method":"set_motor_speed","params":{"channel":0,"speed_pct":50.0,"ttl_ms":100}}' \
    2>/dev/null) || WATCHDOG=""

RESP_T9A=$(echo "$WATCHDOG" | sed -n '1p')
if echo "$RESP_T9A" | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if d.get('ok') else 1)" 2>/dev/null; then
    # Wait for watchdog to expire the lease.
    sleep 0.5
    RESP_T9B=$(ipc_one '{"id":"t9b","method":"get_motor_status","params":{}}' 2>/dev/null) \
        || RESP_T9B='{"ok":false}'
    EXPIRED_COUNT=$(python3 -c "import json,sys; d=json.loads(sys.argv[1]); print(len(d.get('result',{}).get('active_leases',[])))" "$RESP_T9B" 2>/dev/null || echo "-1")
    if [[ "$EXPIRED_COUNT" == "0" ]]; then
        echo "  PASS  T9 TTL watchdog idles motor after lease expiry"
        ((PASS++)) || true
    else
        echo "  FAIL  T9 TTL watchdog: expected 0 leases after expiry, got $EXPIRED_COUNT"
        echo "        t9b response: $RESP_T9B"
        ((FAIL++)) || true
    fi
else
    echo "  FAIL  T9 TTL watchdog: set_motor_speed failed"
    echo "        t9a response: $RESP_T9A"
    ((FAIL++)) || true
fi

# ── Summary ──────────────────────────────────────────────────────────────────

echo ""
echo "Results: $PASS passed, $FAIL failed"
if [[ $FAIL -eq 0 ]]; then
    echo "All motor tests PASSED"
    exit 0
else
    echo "SOME TESTS FAILED — review output above"
    exit 1
fi
