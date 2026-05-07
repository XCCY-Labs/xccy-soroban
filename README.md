# xccy-soroban

Soroban-native implementation of [XCCY](https://xccy.fi) — an Interest Rate
Swap AMM protocol — ported from the Ethereum/Polygon Solidity codebase to
Stellar/Soroban.

> **Status**: early. The **OracleHub** module is the only thing shipped so far.
> Hub + 5 adapters are deployed and live on Stellar **testnet**, three of them
> wired to real on-chain data (Reflector for prices, Blend Pool V2 for variable
> rates, custom-APR for admin-set fixed rates). Two adapters (sUSDe, BENJI) ship
> with admin-pushed signed-feed paths and AprOracle-style observation buffers
> until native on-chain readers land on Stellar.

## What's in this repo

```
xccy-soroban/
├── Cargo.toml                              # workspace
├── rust-toolchain.toml                     # 1.91.0
├── .cargo/config.toml                      # wasm32v1-none target hint
└── oraclehub/
    ├── .planning/                          # RESEARCH.md, VERIFICATION.md
    ├── crates/
    │   ├── types/                          # OracleId, OracleKind, RateData,
    │   │                                   # PriceData, OracleError, SepAsset,
    │   │                                   # PriceGuard, apply_staleness, …
    │   └── wad/                             # WAD math via I256 intermediates
    ├── contracts/
    │   ├── adapters/
    │   │   ├── reflector_price/            # SEP-40 wrapper
    │   │   ├── blend_rate/                 # blend-contract-sdk Pool client
    │   │   ├── benji_yield/                # AprOracle pull pattern
    │   │   ├── usde_rate/                  # ERC-4626 + signed-feed dual mode
    │   │   └── custom_apr/                 # admin-set + timelock + dev-guard
    │   └── hub/                            # singleton aggregator
    ├── fixtures/solidity_vectors.json      # 22 differential vectors
    └── tests/integration.sh                # local sandbox smoke test
```

99 unit/property/differential tests pass; `cargo clippy --workspace --all-targets
-- -D warnings` and `cargo fmt --check --all` are clean.

## Live testnet deployment

| Contract | Address |
|---|---|
| OracleHub v2 | [`CDYX3GIDMUIUH5FVDRZSCD6ZC4MVRZT27NPLRAKFPKN7YOKCS4BWRQHA`](https://stellar.expert/explorer/testnet/contract/CDYX3GIDMUIUH5FVDRZSCD6ZC4MVRZT27NPLRAKFPKN7YOKCS4BWRQHA) |
| ReflectorPrice v2 | [`CACERBYEMK44JOSILZSZIR3HRDGALO3I6QC2KI7QCGQFRTV2ZHJKZ5BJ`](https://stellar.expert/explorer/testnet/contract/CACERBYEMK44JOSILZSZIR3HRDGALO3I6QC2KI7QCGQFRTV2ZHJKZ5BJ) |
| BlendRate | [`CCHP47YX3HRF32D2YQCMZ4O2QIPASLHZPVB2GKK4F7NUQGMMCKL65M5R`](https://stellar.expert/explorer/testnet/contract/CCHP47YX3HRF32D2YQCMZ4O2QIPASLHZPVB2GKK4F7NUQGMMCKL65M5R) |
| CustomApr | [`CDSQRJA6O4F3HWHIQ6HCHSNAZY6M4YRCHJDUTRN6IYVHV3ZT2QG2FDDX`](https://stellar.expert/explorer/testnet/contract/CDSQRJA6O4F3HWHIQ6HCHSNAZY6M4YRCHJDUTRN6IYVHV3ZT2QG2FDDX) |

Live external dependencies the hub talks to:

- Reflector External CEX/DEX (USD base, 16 assets):
  `CCYOZJCOPG34LLQQ7N24YXBM7LL62R7ONMZ3G6WZAAYPB5OYKOMJRN63`
- Blend Capital TestnetV2 pool (USDC, XLM, wETH, wBTC):
  `CCEBVDYM32YNYCVNRXQKDFFPISJJCV557CDZEIRBEE4NCV4KHPQ44HGF`

### Sample reads

```bash
# Live BTC/USD price (via Reflector → reflector_price → hub)
stellar contract invoke --network testnet \
  --id CDYX3GIDMUIUH5FVDRZSCD6ZC4MVRZT27NPLRAKFPKN7YOKCS4BWRQHA \
  -- get_price --id '{"key":"BTC","kind":0}' --asset '{"Other":"BTC"}'
# → {"price":"81054114045187125560000","timestamp":1778115300}  (= $81,054.11)

# Live USDC supply rate (via Blend Pool V2 → blend_rate → hub)
stellar contract invoke --network testnet \
  --id CDYX3GIDMUIUH5FVDRZSCD6ZC4MVRZT27NPLRAKFPKN7YOKCS4BWRQHA \
  -- get_rate --id '{"key":"USDC_b","kind":1}' \
  --key CAQCFVLOBK5GIULPNZRGATJJMIZL5BSP7X5YJVMGCPTUEPFM4AVSRCJU
# → {"updated_at":1778186784,"value":"1055701738429000000"}  (= +5.57% accumulated)
```

## Architecture, briefly

```
                        ┌──────────────────────────────┐
                        │       OracleHub (singleton)   │
                        │  ─────────────────────────    │
                        │  • registry: OracleId→Address │
                        │  • get_price(id, sep_asset)   │
                        │  • get_rate(id, asset)        │
                        │  • admin (two-step + timelock)│
                        │  • emergency pause            │
                        └──────────────┬───────────────┘
                                       │  Address::invoke_contract
       ┌───────────────────────────────┼───────────────────────────────┐
       ▼                ▼              ▼              ▼                ▼
  reflector_price   blend_rate    benji_yield    usde_rate         custom_apr
   (SEP-40)        (blend SDK)   (signed-feed +   (ERC-4626 +     (admin-set,
                                  pull/AprOracle)  signed-feed)    timelock,
                                                                   max-deviation)
       │                │              │              │
       ▼                ▼              ▼              ▼
   Reflector       Blend Pool      [admin push]   [admin push or
   subscription    V2                + observation  on-chain sUSDe
   contract                          buffer]       wrapper]
```

All adapter outputs are normalised to **WAD** (1e18). Cumulative-index adapters
(blend_rate, benji_yield) return monotonically growing indexes; downstream
consumers compute realised APR as the index ratio over a time window
(`AprOracle.getRateFromTo` semantics from the Solidity reference).

## Build

```bash
# Workspace build (host target)
cargo build --workspace

# Production WASM (Soroban requires wasm32v1-none in SDK ≥25)
cargo build --release --target wasm32v1-none --workspace

# Optimise for deploy
for c in oraclehub_hub oraclehub_reflector_price oraclehub_blend_rate \
         oraclehub_benji_yield oraclehub_usde_rate oraclehub_custom_apr; do
  stellar contract optimize --wasm target/wasm32v1-none/release/${c}.wasm
done
```

## Test

```bash
cargo test --workspace --no-fail-fast
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check --all
```

Test breakdown (99 total):

| Suite | # | Notes |
|---|---|---|
| `oraclehub-types` | 10 | `apply_staleness`, `within_bps_band`, `PriceGuard::validate` |
| `oraclehub-wad` | 16 | `mul_wad`, `div_wad`, `mul_div_i128`, `lerp`, `to_wad`, overflow |
| `reflector_price` | 6 | SEP-40 round-trip, decimals normalisation, error paths |
| `blend_rate` | 6 | Real `pool::Reserve` ABI, indices in WAD, live utilisation |
| `benji_yield` | 22 | Push paths, observation buffer, **AprOracle-style derivation** |
| `usde_rate` | 7 | ERC-4626 mock + signed-feed mode toggling |
| `custom_apr` | 12 | Timelock state machine, deviation guard, auto-promote |
| `oraclehub-hub` (lib) | 12 | Cross-crate dispatch, admin SM, kind validation |
| `oraclehub-hub` (differential) | 1 (22 vectors) | Solidity parity within 1 wei |
| `oraclehub-hub` (properties) | 7 | proptest @ 4,600 iterations |

## Design decisions worth flagging

- **WAD on `i128` with `I256` intermediates**: precision-preserving multiplication
  for any cross-contract money math. Same as the Solidity reference.
- **Cumulative-index rate primitive**: matches the Solidity `AprOracle` —
  consumers derive APR from index deltas over a time window rather than relying
  on instantaneous APR.
- **One feed per `reflector_price` instance**: deploy multiple adapters for
  multiple Reflector tiers (CEX/DEX, FX, Stellar DEX). Hub registry keys them
  by `OracleId`.
- **Pull-pattern observations in `benji_yield`**: anyone may call `update_state`
  to snapshot the latest stored NAV into a bounded ring buffer (default 64).
  `get_apr_from_to(from, to)` derives realised APR from bracketing observations,
  with linear interpolation if endpoints fall between snapshots.
- **`blend-contract-sdk` for ABI**: the Pool client and `Reserve` types are
  auto-generated via `contractimport!(pool.wasm)` — impossible to drift from
  the on-chain ABI.
- **Soroban-native upgrades + 24h timelock + emergency pause**: every admin
  surface is locked behind `require_auth`; `propose_upgrade(wasm_hash)` must
  age 24h before `commit_upgrade()` is callable, and pause blocks the commit
  even after timelock elapses.

## Roadmap (what's next)

| | Description |
|---|---|
| ☐ | sUSDe relayer service: pull `convertToAssets` from Ethereum sUSDe, push signed feed to `usde_rate` |
| ☐ | BENJI NAV relayer service: pull NAV from Franklin Templeton publication, push signed feed |
| ☐ | Multi-sig admin (Stellar account-level multisig) for hub + adapters |
| ☐ | Migration of `events().publish` calls to `#[contractevent]` macro (sdk 25 idiom) |
| ☐ | Mainnet readiness: security audit, parameter calibration, observability |
| ☐ | Port of remaining XCCY modules: `VAMMManager`, `CollateralEngine`, `LockYieldBlend`, `BorrowHedgeBlend`, settlement & maturity |

## License

BSL-1.2 (matching the Solidity reference).
