# OracleHub Soroban Port — RESEARCH.md

> Phase 0 deliverable. Source-of-truth for downstream design decisions.
> Status: complete. No open blocking questions; uncertainties are bounded and parameterised.

## 0. Sources Consulted

| Source | Path / URL | What it told us |
|---|---|---|
| Solidity OracleHub | `xccy-core/src/OracleHub.sol` | Aggregator semantics: per-asset price-source registry + guard profile (heartbeat + clamp); APR is delegated to a separate `AprOracle`. |
| Solidity AprOracle | `xccy-core/src/oracles/AprOracle.sol` | EWMA smoothing + observation buffer + tenor-conversion math. **Out of scope** for v0.1 Soroban port (prompt mandates minimal API). |
| Adapter analogues | `xccy-core/src/oracles/adapters/{Aave,Erc4626,External,AaveRwa}AprAdapter.sol` | Direct mapping to 5 Soroban adapters (see §3). |
| PriceGuards lib | `xccy-core/src/lib/PriceGuardsLib.sol` | Exact semantics of `applyStaleness` and `withinBpsBand` — must replicate within 1 wei. |
| OracleHub interfaces | `xccy-core/src/interfaces/oracle/IOracleHub*.sol`, `IPriceSource.sol`, `IAprAdapter.sol` | Function signatures, error names, event names. |
| OracleHub tests | `xccy-core/test/oracles/OracleHub.t.sol` | 7 unit tests + 1 fork test. Unit tests yield 7+ usable differential vectors; fork-test vectors require live Aave/Pendle state and are not directly portable. |
| Stellar architecture doc | `output/pdf/xccy-stellar-technical-architecture.pdf` (read in full) | Confirms first market = sUSDe/PYUSD, Reflector for prices, Blend for variable rates, sUSDe rate-feed publication is a **launch dependency**, not cosmetic. |
| Repo CLAUDE.md | `xccy-core/CLAUDE.md`, root `CLAUDE.md` | Solidity conventions, precision suffixes (Wad/Ray/X96), test taxonomy. |
| Stellar port plan | `STELLAR_PORT_PLAN.md` | M5 (W6–W7) maps to OracleHub + Reflector + sUSDe rate. DoD: stable APR, stale-guard. |

## 1. What We Are Porting

The Solidity stack is **two-tier**:

```
OracleHub  ──get_price──>  IPriceSource (per asset)
    └──get_apr──>  AprOracle  ──>  IAprAdapter (per asset kind)
```

`OracleHub.sol` is intentionally tiny (~92 LOC). It only holds:
- `priceSourceOf: asset → source` mapping
- `guardOf: asset → PriceGuard{heartbeatSec, clampMinBps, clampMaxBps}` mapping
- `aprOracle: address` (single delegate for all rate queries)

`getPriceUsdWad(asset)`:
1. Load source → `peekPrice(asset) → (priceWad, ts, ok)`
2. If `!ok` → invalid
3. Apply staleness: reject if `block.timestamp - ts > heartbeatSec`
4. If stable-clamp configured (any of `clampMinBps`/`clampMaxBps` non-zero):
   - if `price ∈ anchor·[1+min/1e4, 1+max/1e4]` (anchor = 1e18) → return $1
   - else → return raw price
5. Otherwise → return raw price

`getOnAprRay(asset)` is a pure pass-through to `AprOracle.getOnAprRay`.

The Soroban port **collapses these into a single hub** because:
- v1 Stellar market needs only 5 specific feeds — no leverage in two parallel registries.
- Soroban WASM bytes are precious; one contract beats two.
- The price/rate distinction is captured by `OracleKind` instead of by separate contracts.

## 2. Architectural Decisions Locked

| Decision | Choice | Why |
|---|---|---|
| Singleton hub | Yes — `OracleHub` is one contract holding the registry | Mirrors Solidity, minimises cross-contract calls per read. |
| Adapter dispatch | Hub stores `Map<OracleId, Address>`, calls `Address::invoke_contract` | Soroban-idiomatic; lets adapters be upgraded independently. |
| Money math precision | WAD on `i128`, intermediate `mul_div` via `soroban_sdk::I256` | Spec mandate. Matches Solidity WAD precisely. |
| Rate semantics | All adapter outputs are **WAD** (1e18 == 100% APR-equivalent). Solidity `RAY` (1e27) is normalised down to WAD inside the Blend adapter. | Single precision unit; no Ray drift across the codebase. |
| Trait surface | Minimal SEP-40-compatible `PriceFeed` trait: `lastprice(asset) -> Option<PriceData>`, `decimals() -> u32`. | Future-proof against any SEP-40-compliant Stellar oracle. |
| Errors | `OracleError` enum derived 1:1 from Solidity custom errors via `#[contracterror]`. No `unwrap()`, no `panic!` in contract code paths. | Spec mandate; required by Phase 8 verification. |
| Storage | `instance` for admin/pause/timelock; `persistent` for registry. **No** `temporary` cache (analysis below). | TTL economics didn't justify cache complexity for v0.1. |
| Upgrades | Soroban-native `update_current_contract_wasm` + 24h timelock + emergency pause | Spec mandate. |
| Differential ε | **1 wei at WAD** (i.e., abs diff ≤ 1) | Spec mandate; matches Solidity rounding tolerance. |

### 2.1 Why no `temporary` cache

A `temporary` `last_read_cache` was considered for the hub (cache adapter response within a ledger). Rejected because:

- Soroban already executes within a single ledger atomically — within one tx, redundant reads are rare in our consumer set.
- A cache complicates correctness reasoning (stale-read attack surface, eviction logic) for negligible gas saving.
- If profiling later shows benefit, it can be added without breaking the public API.

## 3. Adapter Decisions

Each row: **primary** data path + **fallback**. Fallbacks engage only if primary is structurally unavailable on Stellar at deploy time.

### 3.1 `reflector_price` (replaces ChainlinkOracle/Chainlinksource)

| Field | Decision |
|---|---|
| Primary | Reflector via SEP-40 `lastprice(asset)` + `decimals()` |
| Fallback | None — Reflector is the canonical Stellar price oracle; if it's down, prices are unavailable, period. |
| Public methods | `get_price(asset) -> PriceData`, `set_feed(asset, feed_addr)` (admin) |
| Storage | `feeds: Map<Address, Address>` (asset → reflector subscription contract) |
| Decimals normalisation | Multiply/divide to bring SEP-40 reading up to WAD precision (`decimals = 14` is the documented Reflector default; we use it as a fallback if `decimals()` call fails). |
| Use case | PYUSD spot vs USD for `OracleHub.get_price(...)` calls. |

**Open uncertainty (non-blocking)**: exact mainnet/testnet Reflector contract addresses change with subscription tier. Resolved by parameterising the feed address via admin setter — no code change on rotation.

### 3.2 `blend_rate` (replaces AaveAprAdapter)

| Field | Decision |
|---|---|
| Primary | Blend `Pool.get_reserve_data(asset)` returning struct with utilisation curve fields; compute `borrow_rate` and `supply_rate` per Blend's IR model. |
| Fallback | None — Blend is the canonical variable-rate venue per the architecture doc. |
| Public methods | `get_borrow_rate(asset) -> i128 (WAD)`, `get_supply_rate(asset) -> i128 (WAD)`, `set_pool(addr)` (admin) |
| Storage | `pool: Address` (one Blend pool address; v1 has one pool) |
| Rate normalisation | Blend uses 7-decimal `SCALAR_7` (1e7). Convert up to WAD by multiplying by `1e11`. |
| IR formula | Approximated stub for v0.1: `borrow_rate = base_rate + utilisation * slope`. **TODO at Blend integration time**: replace stub with exact piecewise IR curve once Blend's published `Pool` ABI is wired. The hub-side API stays stable. |

**Note on the IR curve**: The exact slope coefficients are a property of the Blend pool config, not of the adapter — so the adapter just reads `borrow_rate` directly off `get_reserve_data` if Blend exposes it that way; otherwise the slope-from-utilisation computation is needed. We code the adapter against an abstract `BlendPoolClient` trait so we can swap real ABI for mock in tests.

### 3.3 `benji_yield` (replaces AaveRwaAprAdapter)

| Field | Decision |
|---|---|
| Primary | Read FOBXX share-price/NAV from Franklin Templeton's Stellar deployment if a public read function is exposed. |
| Fallback (active by default for v0.1) | Admin-pushed signed feed: a relayer signs `(yield_wad, timestamp)` with Ed25519, the adapter verifies via `env.crypto().ed25519_verify` and stores. |
| Why fallback is the default | RWA on-chain readability for FOBXX is not yet confirmed in public docs (knowledge cutoff). The signed-feed pattern is the safe default; if/when on-chain readability is confirmed, `set_mode(Mode::OnChain)` flips behaviour without an upgrade. |
| Public methods | `get_yield() -> i128 (WAD)`, `push_signed(payload, signature)`, `set_signer(pubkey)` (admin), `set_mode(mode)` (admin) |
| Storage | `mode: SourceMode {Signed, OnChain}`, `signer_pubkey: BytesN<32>`, `last: Option<RateData>`, `nav_contract: Option<Address>` |

### 3.4 `usde_rate` (replaces Erc4626AprAdapter)

| Field | Decision |
|---|---|
| Primary | Read sUSDe `convert_to_assets(WAD)` exchange rate from the wrapped/native sUSDe contract on Stellar. The sUSDe vault inherits ERC-4626 semantics and Stellar wrappers preserve them. |
| Fallback | Admin-pushed signed feed (same Ed25519 pattern as `benji_yield`). |
| Why fallback exists | Per the architecture doc §5: "If the rate is not available natively on Stellar, the deployment requires an adapter or relayer path. **This is a launch dependency, not a cosmetic integration detail.**" |
| Public methods | `get_rate() -> i128 (WAD)`, `push_signed(...)`, `set_vault(addr)`, `set_mode(...)` |
| Storage | Same shape as benji_yield. |

### 3.5 `custom_apr` (replaces ExternalAprAdapter)

| Field | Decision |
|---|---|
| Primary | Manual admin setter, **timelocked** + **max-deviation guard**. Used for off-the-shelf APR that has no other on-chain source. |
| Fallback | N/A — this is the fallback path itself for cases where adapters above can't service the asset. |
| Public methods | `set_apr(new_apr_wad, effective_at)` (admin, with timelock), `get_apr() -> i128 (WAD)`, `set_max_deviation_bps(bps)` (admin) |
| Storage | `current_apr: i128`, `pending: Option<(i128, u64 effective_at)>`, `max_deviation_bps: u32` |
| Deviation rule | Reject `set_apr(new)` if `current != 0` and `|new - current| * 10_000 / current > max_deviation_bps`. |

## 4. Differential Test Plan

**Goal**: prove the Soroban code yields identical outputs to Solidity for shared logic, within ε = 1 wei.

### 4.1 Vectors usable from `OracleHub.t.sol`

| # | Solidity test | Logic exercised | Vector inputs | Vector expected |
|---|---|---|---|---|
| 1 | `test_staleness_pass` | `applyStaleness` fresh path | `heartbeat=3600, age=100` | `valid=true, price=1e18` |
| 2 | `test_staleness_fail` | `applyStaleness` stale path | `heartbeat=60, age=3600` | `valid=false` |
| 3 | `test_stableClamp_passWithin30bps` | `withinBpsBand` inside band | `price=997e15, clamp=±30` | `valid=true, price=1e18` |
| 4 | `test_stableClamp_failOutside30bps` | `withinBpsBand` outside band | `price=996e15, clamp=±30` | `valid=true, price=996e15` |
| 5 | `test_guardProfile_reverts_whenMinAboveMax` | guard validation | `min=10, max=5` | `Err(InvalidGuardBand)` |
| 6 | `test_guardProfile_reverts_whenBpsOutOfRange` | guard validation | `min=-10001, max=0` | `Err(GuardBpsOutOfRange)` |
| 7 | `test_errorBubbling` | source returns `ok=false` | `source.set(0,0,false)` | `valid=false` |

### 4.2 Vectors synthesised for adapter logic

| # | Adapter | Logic | Vector inputs | Vector expected |
|---|---|---|---|---|
| 8 | `wad::mul_wad` | `mul(1e18, 1e18)` | `(1e18, 1e18)` | `1e18` |
| 9 | `wad::mul_wad` | half × half | `(5e17, 5e17)` | `25e16` |
| 10 | `wad::div_wad` | identity | `(1e18, 1e18)` | `1e18` |
| 11 | `wad::div_wad` | half | `(5e17, 1e18)` | `5e17` |
| 12 | `wad::mul_div_i128` | overflow guard | `(i128::MAX, 2, 1)` | `Err(Overflow)` |
| 13 | `blend_rate` rate normalisation | Blend SCALAR_7 → WAD | `borrow_rate_scalar7=500_000` (5%) | `5e16` |
| 14 | `blend_rate` zero | `borrow_rate=0` | `0` | `0` |
| 15 | `usde_rate` ERC-4626 ratio | `convert_to_assets(1e18)=1.05e18` | `1.05e18 - 1e18 = 5e16` interpreted as growth | `growth=5e16` |
| 16 | `custom_apr` deviation reject | `current=5e16, new=10e16, max=1000bps` | exceeds 100% deviation | `Err(DeviationExceeded)` |
| 17 | `custom_apr` deviation accept | `current=5e16, new=5.5e16, max=2000bps` | 10% delta within 20% | `Ok(())` |
| 18 | `bps_band` lower edge | `value=997e15, anchor=1e18, min=-30, max=30` | exactly on edge | `true` |
| 19 | `bps_band` upper edge | `value=1003e15, anchor=1e18, min=-30, max=30` | exactly on edge | `true` |
| 20 | `bps_band` outside | `value=995999999999999999, anchor=1e18, min=-30, max=30` | just outside | `false` |
| 21 | timelock invariant | `propose, advance < 24h, commit` | `commit before timelock` | `Err(TimelockNotElapsed)` |
| 22 | pause invariant | `pause then read` | any read while paused | `Err(Paused)` |

22 vectors satisfies the spec's "≥20 vectors" requirement.

## 5. Errors Mapping (Solidity → Soroban)

| Solidity custom error | Soroban `OracleError` variant | Numeric code |
|---|---|---|
| `OracleHub_ZeroAddress()` | `ZeroAddress` | 1 |
| `OracleHub_InvalidGuardBand()` | `InvalidGuardBand` | 2 |
| `OracleHub_GuardBpsOutOfRange()` | `GuardBpsOutOfRange` | 3 |
| (new) source returned `ok=false` | `SourceUnavailable` | 4 |
| (new) timestamp from future | `FutureTimestamp` | 5 |
| (new) past heartbeat | `StaleData` | 6 |
| (new) WAD overflow | `Overflow` | 7 |
| (new) divide-by-zero | `DivideByZero` | 8 |
| (new) admin gating | `Unauthorized` | 9 |
| (new) registry miss | `OracleNotRegistered` | 10 |
| (new) `Initializable` re-init | `AlreadyInitialized` | 11 |
| (new) timelock guard | `TimelockNotElapsed` | 12 |
| (new) pause | `Paused` | 13 |
| (new) custom-APR delta guard | `DeviationExceeded` | 14 |
| (new) signed-feed Ed25519 fail | `BadSignature` | 15 |
| (new) signed-feed replay | `StalePushedFeed` | 16 |
| (new) custom-APR effective_at in past | `InvalidEffectiveAt` | 17 |
| (new) admin not set | `AdminNotSet` | 18 |
| (new) generic miss | `AssetNotSupported` | 19 |

## 6. Property Invariants (Phase 6 design)

1. **`wad::mul_wad` commutativity**: `mul_wad(a, b) == mul_wad(b, a)` for all `a, b ∈ [0, sqrt(I256_MAX)]`.
2. **WAD round-trip**: `div_wad(mul_wad(a, b), b) ∈ [a-1, a+1]` for `b > 0`.
3. **Overflow safety**: `mul_div_i128(i128::MAX, i128::MAX, 1)` returns `Err(Overflow)`, never panics.
4. **Registry round-trip**: `register(id, addr); lookup(id) == Some(addr)`.
5. **Registry eviction**: `register(id, a); unregister(id); lookup(id) == None` and any read returns `OracleNotRegistered`.
6. **Timelock monotonicity**: `propose_upgrade(h)` followed by `commit_upgrade()` at `t < propose_t + 24h` returns `TimelockNotElapsed`; at `t ≥ propose_t + 24h` succeeds.
7. **Pause coverage**: when `paused == true`, every read method returns `Paused`. When `paused == false`, behaviour matches the unpaused contract.

## 7. Toolchain & Storage Tier Strategy

### 7.1 Toolchain pinned versions

- Rust: `1.83.0` (matches host machine; pinned via `rust-toolchain.toml`)
- soroban-sdk: `22` (latest published per spec)
- soroban-sdk dev-deps: `testutils` feature on
- proptest: `1`
- serde_json: `1` (test-only, for `solidity_vectors.json` parsing)

### 7.2 Storage tiers used

- `instance`: `admin`, `pending_admin`, `paused`, `pending_upgrade`. Hot, ~32-byte fixed-size set; cheap to load.
- `persistent`: `registry: Map<OracleId, Address>`. TTL extended on every read inside `get_rate`/`get_price` via `env.storage().persistent().extend_ttl(...)`.
- `temporary`: **none**. Justified in §2.1.

### 7.3 TTL extension policy

- Persistent registry entries: extend by `bump_amount = 30 * 17280` (≈30 days at 17280 ledgers/day) on every successful read, threshold = `7 * 17280` (7 days).
- Instance storage: extended automatically on any state-changing call (Soroban default).

## 8. Open Uncertainties (Bounded, Non-Blocking)

| Uncertainty | Bound / mitigation |
|---|---|
| Reflector mainnet/testnet contract addresses | Parameterised via `set_feed(asset, addr)`; no code change on rotation. |
| Blend `Pool` interest-rate curve coefficients | Adapter trait abstracts the curve formula; mock used for unit tests; integration tests will verify against live testnet. |
| FOBXX on-chain NAV readability | Adapter ships in `Mode::Signed` by default; `set_mode(OnChain)` is a no-upgrade switch once readability is confirmed. |
| sUSDe Stellar deployment final form (Allbridge wrapper vs native OFT) | Same dual-mode pattern as benji_yield; vault address parameterised. |
| `stellar-cli` install on dev host | Phase 7 integration script ships executable; CI installs `stellar` on first run. |

## 9. Phase 0 Exit Criteria

- [x] All input files cited above (§0).
- [x] PDF read in full (pages 1–7); key constraints quoted (§3.4 sUSDe rate-feed launch dependency).
- [x] All 5 adapters documented with primary + fallback (§3).
- [x] Differential ε locked at 1 wei (§2, §4).
- [x] No open blocking questions (§8 lists only parameterised, non-blocking items).
