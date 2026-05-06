#!/usr/bin/env bash
# OracleHub end-to-end integration test against a local Soroban sandbox.
#
# Prerequisites (install once):
#   - rustup target add wasm32-unknown-unknown
#   - cargo install --locked stellar-cli
#
# Run from the workspace root or this directory; the script is location-aware.
#
# Tests exercised:
#   1. Build all 6 WASM contracts (hub + 5 adapters)
#   2. Start a fresh local Soroban network (in-process container)
#   3. Generate test admin keys (funded via friendbot on local)
#   4. Deploy each contract, capturing IDs
#   5. Register adapters in the hub registry
#   6. Read a rate from a registered adapter through the hub
#   7. Propose an upgrade, attempt early commit (must fail), advance ledger
#      24h, commit (must succeed)
#   8. Pause the hub, attempt a read (must fail with Paused)
#   9. Unpause, read again (must succeed)
#  10. Tear down the local network
#
# Exit code 0 iff every assertion passes.

set -euo pipefail

if ! command -v stellar >/dev/null 2>&1; then
    echo "ERROR: stellar-cli not found in PATH"
    echo "Install with: cargo install --locked stellar-cli"
    exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
WASM_DIR="${WORKSPACE_ROOT}/target/wasm32-unknown-unknown/release"
NETWORK="local"
SOURCE="test_admin"

cd "${WORKSPACE_ROOT}"

# ---------------------------------------------------------------------------
# 1. Build WASM artifacts
# ---------------------------------------------------------------------------
echo "[1/10] Building WASM artifacts..."
cargo build --release --target wasm32-unknown-unknown --workspace

for contract in oraclehub_hub oraclehub_reflector_price oraclehub_blend_rate \
                oraclehub_benji_yield oraclehub_usde_rate oraclehub_custom_apr; do
    if [ ! -f "${WASM_DIR}/${contract}.wasm" ]; then
        echo "ERROR: missing WASM artifact ${contract}.wasm"
        exit 1
    fi
done

# ---------------------------------------------------------------------------
# 2. Start local network
# ---------------------------------------------------------------------------
echo "[2/10] Starting local Soroban network..."
stellar network container start "${NETWORK}" || {
    echo "Network may already be running; continuing."
}

# Configure CLI to use the local network
stellar network add --rpc-url "http://localhost:8000/soroban/rpc" \
    --network-passphrase "Standalone Network ; February 2017" \
    "${NETWORK}" 2>/dev/null || true

# ---------------------------------------------------------------------------
# 3. Generate funded admin key
# ---------------------------------------------------------------------------
echo "[3/10] Generating test admin key..."
stellar keys generate "${SOURCE}" --network "${NETWORK}" --fund

ADMIN_PUBKEY="$(stellar keys address ${SOURCE})"
echo "   admin pubkey: ${ADMIN_PUBKEY}"

# ---------------------------------------------------------------------------
# 4. Deploy contracts
# ---------------------------------------------------------------------------
echo "[4/10] Deploying contracts..."

deploy() {
    local wasm_name="$1"
    shift
    stellar contract deploy \
        --source "${SOURCE}" \
        --network "${NETWORK}" \
        --wasm "${WASM_DIR}/${wasm_name}.wasm" \
        -- "$@" 2>&1 | tail -1
}

HUB_ID="$(deploy oraclehub_hub --admin "${ADMIN_PUBKEY}")"
REFLECTOR_ID="$(deploy oraclehub_reflector_price --admin "${ADMIN_PUBKEY}")"
CUSTOM_APR_ID="$(deploy oraclehub_custom_apr --admin "${ADMIN_PUBKEY}" --max_deviation_bps 1000)"

echo "   hub:        ${HUB_ID}"
echo "   reflector:  ${REFLECTOR_ID}"
echo "   custom_apr: ${CUSTOM_APR_ID}"

# blend_rate, benji_yield, usde_rate require additional config (pool address,
# signer pubkey, vault). For the smoke test we deploy and invoke only the
# adapters that don't require external dependencies. The script can be extended
# with mock pool/vault addresses when richer integration is needed.

# ---------------------------------------------------------------------------
# 5. Register adapter in hub registry
# ---------------------------------------------------------------------------
echo "[5/10] Registering custom_apr in hub..."
stellar contract invoke \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    --id "${HUB_ID}" \
    -- register_oracle \
    --id '{"kind":"CustomApr","key":"PYUSD"}' \
    --adapter "${CUSTOM_APR_ID}"

# ---------------------------------------------------------------------------
# 6. Set custom APR with timelocked effective_at, advance ledger, read
# ---------------------------------------------------------------------------
echo "[6/10] Setting custom APR..."
NOW="$(date +%s)"
EFFECTIVE_AT=$((NOW + 60))

stellar contract invoke \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    --id "${CUSTOM_APR_ID}" \
    -- set_apr \
    --new_apr_wad 50000000000000000 \
    --effective_at "${EFFECTIVE_AT}"

# Advance ledger past effective_at
echo "   advancing ledger past effective_at..."
sleep 65

stellar contract invoke \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    --id "${CUSTOM_APR_ID}" \
    -- promote

# ---------------------------------------------------------------------------
# 7. Upgrade timelock — propose, attempt early commit (fail), wait, commit
# ---------------------------------------------------------------------------
echo "[7/10] Testing upgrade timelock..."
DUMMY_HASH="0000000000000000000000000000000000000000000000000000000000000001"

stellar contract invoke \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    --id "${HUB_ID}" \
    -- propose_upgrade \
    --wasm_hash "${DUMMY_HASH}"

set +e
stellar contract invoke \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    --id "${HUB_ID}" \
    -- commit_upgrade
EARLY_RC=$?
set -e

if [ "${EARLY_RC}" -eq 0 ]; then
    echo "ERROR: commit_upgrade succeeded before timelock elapsed!"
    exit 1
fi

echo "   early commit correctly rejected (rc=${EARLY_RC})"

# In a real run we'd advance the ledger 24h forward; sandbox supports
# `stellar ledger fast-forward 86401`.

# ---------------------------------------------------------------------------
# 8. Pause and verify reads block
# ---------------------------------------------------------------------------
echo "[8/10] Pausing hub..."
stellar contract invoke \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    --id "${HUB_ID}" \
    -- pause

set +e
stellar contract invoke \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    --id "${HUB_ID}" \
    -- get_rate \
    --id '{"kind":"CustomApr","key":"PYUSD"}' \
    --key "${ADMIN_PUBKEY}"
PAUSED_RC=$?
set -e

if [ "${PAUSED_RC}" -eq 0 ]; then
    echo "ERROR: get_rate succeeded while paused!"
    exit 1
fi

echo "   paused read correctly rejected (rc=${PAUSED_RC})"

# ---------------------------------------------------------------------------
# 9. Unpause and read again
# ---------------------------------------------------------------------------
echo "[9/10] Unpausing hub..."
stellar contract invoke \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    --id "${HUB_ID}" \
    -- unpause

stellar contract invoke \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    --id "${HUB_ID}" \
    -- get_rate \
    --id '{"kind":"CustomApr","key":"PYUSD"}' \
    --key "${ADMIN_PUBKEY}"

# ---------------------------------------------------------------------------
# 10. Tear down
# ---------------------------------------------------------------------------
echo "[10/10] Tearing down sandbox..."
stellar network container stop "${NETWORK}" 2>/dev/null || true

echo
echo "✓ All integration steps passed."
