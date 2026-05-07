#![allow(deprecated)] // soroban-sdk 25: events().publish migrating to #[contractevent], TODO
#![no_std]

//! BenjiYield adapter — surfaces FOBXX (Franklin Templeton tokenised
//! US-Treasury fund) yield on Stellar via the **AprOracle pull pattern**.
//!
//! ### Why this shape
//!
//! Franklin Templeton publishes BENJI NAV off-chain on a fixed cadence
//! (≈daily). There is no live on-chain NAV reader on Stellar today, so the
//! adapter sits in `Mode::Signed` by default: a relayer pushes
//! `(nav_wad, ts, nonce)` tuples signed with Ed25519. The contract verifies
//! the signature, replay-checks the nonce, and atomically stores the latest
//! reading.
//!
//! ### Pull model — observations + APR derivation
//!
//! In addition to the raw "current NAV" surface, the adapter maintains a
//! ring buffer of (ts, nav_wad) observations:
//!
//! - `update_state()` — anyone may call. Reads the latest stored NAV (from
//!   the signed feed in `Mode::Signed`, or from an on-chain reader once
//!   `Mode::OnChain` is wired) and appends an observation **iff** its
//!   timestamp is strictly newer than the most recent stored one.
//! - `get_apr_from_to(from, to)` — derives the realised APR over the window
//!   from the bracketing observations. Returned in WAD (1e18).
//!
//! The realised APR formula is the standard linear annualisation:
//!
//! ```text
//! realised_apr = (nav[to] / nav[from] - 1) · SECONDS_PER_YEAR / (to - from)
//! ```
//!
//! This is the same primitive shape as `AprOracle.getRateFromTo` in the
//! Solidity reference — downstream consumers can rely on identical
//! semantics.

use oraclehub_types::{OracleError, RateData, WAD};
use oraclehub_wad::mul_div_i128;
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, xdr::ToXdr, Address, Bytes, BytesN, Env,
    Symbol, Vec,
};
use stellar_access::ownable::{self as ownable, Ownable};
use stellar_macros::only_owner;

const SECONDS_PER_YEAR: i128 = 365 * 24 * 60 * 60;
const DEFAULT_MAX_OBSERVATIONS: u32 = 64;

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Mode,
    SignerPubkey,
    NavContract,
    Last,            // RateData — most recent raw NAV reading
    Nonce,           // u64 — replay-protection counter for signed feeds
    Observations,    // Vec<Observation> — bounded ring of NAV snapshots
    MaxObservations, // u32 — ring capacity (default 64)
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceMode {
    Signed,
    OnChain,
}

/// Off-chain payload pushed by the relayer. `nav_wad` is BENJI NAV per share
/// in WAD (1.0 = par, 1.05·WAD = +5% accumulated growth). `nonce` is a
/// monotonic counter to defeat replay attacks.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedFeedPayload {
    pub nav_wad: i128,
    pub updated_at: u64,
    pub nonce: u64,
}

/// One point in the cumulative NAV index history.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub ts: u64,
    pub nav_wad: i128,
}

const TOPIC_PUSH: Symbol = symbol_short!("pushed");
const TOPIC_MODE: Symbol = symbol_short!("mode_set");
const TOPIC_SNAP: Symbol = symbol_short!("snapshot");

#[contract]
pub struct BenjiYield;

#[contractimpl]
impl BenjiYield {
    pub fn __constructor(env: Env, admin: Address, signer_pubkey: BytesN<32>) {
        ownable::set_owner(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::SignerPubkey, &signer_pubkey);
        env.storage()
            .instance()
            .set(&DataKey::Mode, &SourceMode::Signed);
        env.storage().instance().set(&DataKey::Nonce, &0u64);
        env.storage()
            .instance()
            .set(&DataKey::MaxObservations, &DEFAULT_MAX_OBSERVATIONS);
    }

    // ------------- Owner-gated config -------------

    #[only_owner]
    pub fn set_mode(env: Env, mode: SourceMode) {
        env.storage().instance().set(&DataKey::Mode, &mode);
        env.events().publish((TOPIC_MODE,), mode);
    }

    #[only_owner]
    pub fn set_signer(env: Env, pubkey: BytesN<32>) {
        env.storage()
            .instance()
            .set(&DataKey::SignerPubkey, &pubkey);
    }

    #[only_owner]
    pub fn set_nav_contract(env: Env, addr: Address) {
        env.storage().instance().set(&DataKey::NavContract, &addr);
    }

    #[only_owner]
    pub fn set_max_observations(env: Env, n: u32) -> Result<(), OracleError> {
        if n == 0 {
            return Err(OracleError::InvalidArgument);
        }
        env.storage().instance().set(&DataKey::MaxObservations, &n);
        Ok(())
    }

    // ------------- Push paths (raw NAV in) -------------

    /// Bootstrap / multi-sig push path. Owner pushes a `(nav, ts)` tuple
    /// directly via the OZ-Ownable auth check, instead of an off-chain Ed25519
    /// signature. Useful during initial deployment or when the operator is a
    /// Stellar-native multi-sig rather than an off-chain signer.
    #[only_owner]
    pub fn admin_push(env: Env, nav_wad: i128, updated_at: u64) -> Result<(), OracleError> {
        if nav_wad <= 0 {
            return Err(OracleError::InvalidArgument);
        }
        if updated_at > env.ledger().timestamp() {
            return Err(OracleError::FutureTimestamp);
        }
        let last: Option<RateData> = env.storage().instance().get(&DataKey::Last);
        if let Some(prev) = &last {
            if updated_at <= prev.updated_at {
                return Err(OracleError::StalePushedFeed);
            }
        }
        env.storage().instance().set(
            &DataKey::Last,
            &RateData {
                value: nav_wad,
                updated_at,
            },
        );
        env.events().publish((TOPIC_PUSH,), (nav_wad, updated_at));
        Ok(())
    }

    /// Relayer push path. Verifies the Ed25519 signature against the
    /// configured signer pubkey and replay-checks the nonce.
    pub fn push_signed(
        env: Env,
        payload: SignedFeedPayload,
        signature: BytesN<64>,
    ) -> Result<(), OracleError> {
        let pubkey: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::SignerPubkey)
            .ok_or(OracleError::AdminNotSet)?;

        let stored_nonce: u64 = env.storage().instance().get(&DataKey::Nonce).unwrap_or(0);
        if payload.nonce <= stored_nonce {
            return Err(OracleError::StalePushedFeed);
        }
        if payload.updated_at > env.ledger().timestamp() {
            return Err(OracleError::FutureTimestamp);
        }

        let msg: Bytes = payload.clone().to_xdr(&env);
        env.crypto().ed25519_verify(&pubkey, &msg, &signature);

        env.storage()
            .instance()
            .set(&DataKey::Nonce, &payload.nonce);
        env.storage().instance().set(
            &DataKey::Last,
            &RateData {
                value: payload.nav_wad,
                updated_at: payload.updated_at,
            },
        );
        env.events().publish(
            (TOPIC_PUSH,),
            (payload.nav_wad, payload.updated_at, payload.nonce),
        );
        Ok(())
    }

    // ------------- Pull pattern -------------

    /// Snapshot the current NAV reading into the observation ring buffer.
    ///
    /// Anyone may call. The call is a no-op iff the latest stored
    /// observation already has the same (or newer) timestamp — keeps the
    /// buffer monotonic in `ts`.
    pub fn update_state(env: Env) -> Result<bool, OracleError> {
        let current = read_current(&env)?;
        let mut buf: Vec<Observation> = env
            .storage()
            .instance()
            .get(&DataKey::Observations)
            .unwrap_or(Vec::new(&env));

        if let Some(latest) = buf.last() {
            if current.updated_at <= latest.ts {
                return Ok(false); // not newer; skip
            }
        }
        let max: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxObservations)
            .unwrap_or(DEFAULT_MAX_OBSERVATIONS);
        // Evict oldest if at capacity
        while buf.len() >= max {
            buf.pop_front();
        }
        buf.push_back(Observation {
            ts: current.updated_at,
            nav_wad: current.value,
        });
        env.storage().instance().set(&DataKey::Observations, &buf);
        env.events()
            .publish((TOPIC_SNAP,), (current.value, current.updated_at));
        Ok(true)
    }

    /// Realised APR over `[from, to]` in WAD, derived from observations
    /// bracketing the window.
    ///
    /// Returns `Err(SourceUnavailable)` if the buffer doesn't contain
    /// observations bracketing the window. Returns 0 if `to <= from` or if
    /// NAV did not change over the window.
    pub fn get_apr_from_to(env: Env, from: u64, to: u64) -> Result<i128, OracleError> {
        if to <= from {
            return Ok(0);
        }
        let buf: Vec<Observation> = env
            .storage()
            .instance()
            .get(&DataKey::Observations)
            .unwrap_or(Vec::new(&env));
        if buf.len() < 2 {
            return Err(OracleError::SourceUnavailable);
        }
        let nav_from = interpolate(&env, &buf, from)?;
        let nav_to = interpolate(&env, &buf, to)?;
        if nav_to <= nav_from {
            return Ok(0);
        }
        // ratio_minus_one = nav_to / nav_from - 1, in WAD
        let ratio_minus_one = mul_div_i128(&env, nav_to - nav_from, WAD, nav_from)?;
        let dt = (to - from) as i128;
        // realised_apr = ratio_minus_one * SECONDS_PER_YEAR / dt
        mul_div_i128(&env, ratio_minus_one, SECONDS_PER_YEAR, dt)
    }

    /// Realised APR from `from` to the current ledger timestamp.
    pub fn get_apr_from(env: Env, from: u64) -> Result<i128, OracleError> {
        let now = env.ledger().timestamp();
        Self::get_apr_from_to(env, from, now)
    }

    // ------------- Reads / views -------------

    pub fn get_yield(env: Env) -> Result<RateData, OracleError> {
        read_current(&env)
    }

    pub fn peek_rate(env: Env, _key: Address) -> Result<RateData, OracleError> {
        read_current(&env)
    }

    pub fn latest_observation(env: Env) -> Option<Observation> {
        let buf: Vec<Observation> = env
            .storage()
            .instance()
            .get(&DataKey::Observations)
            .unwrap_or(Vec::new(&env));
        buf.last()
    }

    pub fn observation_count(env: Env) -> u32 {
        let buf: Vec<Observation> = env
            .storage()
            .instance()
            .get(&DataKey::Observations)
            .unwrap_or(Vec::new(&env));
        buf.len()
    }

    pub fn observation_at(env: Env, idx: u32) -> Option<Observation> {
        let buf: Vec<Observation> = env
            .storage()
            .instance()
            .get(&DataKey::Observations)
            .unwrap_or(Vec::new(&env));
        buf.get(idx)
    }

    pub fn last_nonce(env: Env) -> u64 {
        env.storage().instance().get(&DataKey::Nonce).unwrap_or(0)
    }

    pub fn mode(env: Env) -> SourceMode {
        env.storage()
            .instance()
            .get(&DataKey::Mode)
            .unwrap_or(SourceMode::Signed)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_current(env: &Env) -> Result<RateData, OracleError> {
    let mode: SourceMode = env
        .storage()
        .instance()
        .get(&DataKey::Mode)
        .unwrap_or(SourceMode::Signed);
    match mode {
        SourceMode::Signed => env
            .storage()
            .instance()
            .get(&DataKey::Last)
            .ok_or(OracleError::SourceUnavailable),
        SourceMode::OnChain => Err(OracleError::SourceUnavailable),
    }
}

/// Linear interpolation of NAV at timestamp `t` from a monotonic-`ts`
/// observation buffer. Errors if `t` falls outside the buffer's covered range.
fn interpolate(env: &Env, buf: &Vec<Observation>, t: u64) -> Result<i128, OracleError> {
    let n = buf.len();
    let oldest = buf.get(0).ok_or(OracleError::SourceUnavailable)?;
    let newest = buf.get(n - 1).ok_or(OracleError::SourceUnavailable)?;
    if t < oldest.ts || t > newest.ts {
        return Err(OracleError::SourceUnavailable);
    }
    if t == oldest.ts {
        return Ok(oldest.nav_wad);
    }
    if t == newest.ts {
        return Ok(newest.nav_wad);
    }
    // Linear scan for bracketing pair (buffer ≤ 64 entries; cheap)
    for i in 0..(n - 1) {
        let a = buf.get(i).unwrap();
        let b = buf.get(i + 1).unwrap();
        if a.ts <= t && t <= b.ts {
            if a.ts == b.ts {
                return Ok(a.nav_wad);
            }
            // nav = a.nav + (b.nav - a.nav) * (t - a.ts) / (b.ts - a.ts)
            let dt = (b.ts - a.ts) as i128;
            let elapsed = (t - a.ts) as i128;
            let span = b.nav_wad - a.nav_wad;
            let interp = oraclehub_wad::mul_div_i128(env, span, elapsed, dt)?;
            return a.nav_wad.checked_add(interp).ok_or(OracleError::Overflow);
        }
    }
    Err(OracleError::SourceUnavailable)
}

#[contractimpl(contracttrait)]
impl Ownable for BenjiYield {}

#[cfg(test)]
mod test;
