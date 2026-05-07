#![allow(deprecated)] // soroban-sdk 25: events().publish migrating to #[contractevent], TODO
#![no_std]

//! CustomApr adapter — admin-set APR with timelock and max-deviation guard.
//!
//! Used as the fallback rate source for assets without a structured on-chain
//! oracle. Setters are timelocked: `set_apr(new, effective_at)` records a
//! pending value that cannot be observed via `get_apr` until block timestamp
//! ≥ `effective_at`. The deviation guard rejects updates whose magnitude
//! exceeds `max_deviation_bps` of the previous value.

use oraclehub_types::{OracleError, RateData};
use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Address, Env, Symbol};

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    CurrentApr, // i128 (WAD)
    Pending,    // Option<(i128, u64)>
    MaxDevBps,  // u32
    UpdatedAt,  // u64 — when CurrentApr was promoted from a pending entry
}

const TOPIC_INIT: Symbol = symbol_short!("init");
const TOPIC_PROP: Symbol = symbol_short!("apr_prop");
const TOPIC_PROM: Symbol = symbol_short!("apr_prom");

#[contract]
pub struct CustomApr;

#[contractimpl]
impl CustomApr {
    pub fn __constructor(env: Env, admin: Address, max_deviation_bps: u32) {
        if env.storage().instance().has(&DataKey::Admin) {
            soroban_sdk::panic_with_error!(env, OracleError::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::CurrentApr, &0i128);
        env.storage()
            .instance()
            .set(&DataKey::MaxDevBps, &max_deviation_bps);
        env.storage().instance().set(&DataKey::UpdatedAt, &0u64);
        env.events()
            .publish((TOPIC_INIT,), (admin, max_deviation_bps));
    }

    pub fn set_apr(env: Env, new_apr_wad: i128, effective_at: u64) -> Result<(), OracleError> {
        require_admin(&env)?;
        if new_apr_wad < 0 {
            return Err(OracleError::InvalidArgument);
        }
        if effective_at <= env.ledger().timestamp() {
            return Err(OracleError::InvalidEffectiveAt);
        }

        let current: i128 = env
            .storage()
            .instance()
            .get(&DataKey::CurrentApr)
            .unwrap_or(0);
        let max_dev_bps: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxDevBps)
            .unwrap_or(0);
        if current != 0 && max_dev_bps > 0 {
            let delta = (new_apr_wad - current).abs();
            // delta * 10_000 / current  > max_dev_bps   ?
            // Using i128 arithmetic; current > 0 here.
            let lhs = delta.saturating_mul(10_000);
            let rhs = current.saturating_mul(max_dev_bps as i128);
            if lhs > rhs {
                return Err(OracleError::DeviationExceeded);
            }
        }

        env.storage()
            .instance()
            .set(&DataKey::Pending, &(new_apr_wad, effective_at));
        env.events()
            .publish((TOPIC_PROP,), (new_apr_wad, effective_at));
        Ok(())
    }

    pub fn cancel_pending(env: Env) -> Result<(), OracleError> {
        require_admin(&env)?;
        env.storage().instance().remove(&DataKey::Pending);
        Ok(())
    }

    /// Promote a pending apr to current if its effective_at has elapsed.
    /// Anyone can call; idempotent.
    pub fn promote(env: Env) -> Result<bool, OracleError> {
        let pending: Option<(i128, u64)> = env.storage().instance().get(&DataKey::Pending);
        let Some((value, effective_at)) = pending else {
            return Ok(false);
        };
        if env.ledger().timestamp() < effective_at {
            return Ok(false);
        }
        env.storage().instance().set(&DataKey::CurrentApr, &value);
        env.storage()
            .instance()
            .set(&DataKey::UpdatedAt, &env.ledger().timestamp());
        env.storage().instance().remove(&DataKey::Pending);
        env.events().publish((TOPIC_PROM,), value);
        Ok(true)
    }

    pub fn get_apr(env: Env) -> Result<RateData, OracleError> {
        // Auto-promote opportunistically so reads stay fresh without a separate tx.
        if let Some((value, effective_at)) = env
            .storage()
            .instance()
            .get::<_, (i128, u64)>(&DataKey::Pending)
        {
            if env.ledger().timestamp() >= effective_at {
                env.storage().instance().set(&DataKey::CurrentApr, &value);
                env.storage()
                    .instance()
                    .set(&DataKey::UpdatedAt, &env.ledger().timestamp());
                env.storage().instance().remove(&DataKey::Pending);
            }
        }
        let value: i128 = env
            .storage()
            .instance()
            .get(&DataKey::CurrentApr)
            .unwrap_or(0);
        let updated_at: u64 = env
            .storage()
            .instance()
            .get(&DataKey::UpdatedAt)
            .unwrap_or(0);
        if updated_at == 0 {
            return Err(OracleError::SourceUnavailable);
        }
        Ok(RateData { value, updated_at })
    }

    pub fn peek_rate(env: Env, _key: Address) -> Result<RateData, OracleError> {
        Self::get_apr(env)
    }

    pub fn pending(env: Env) -> Option<(i128, u64)> {
        env.storage().instance().get(&DataKey::Pending)
    }

    pub fn max_deviation_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::MaxDevBps)
            .unwrap_or(0)
    }

    pub fn set_max_deviation_bps(env: Env, bps: u32) -> Result<(), OracleError> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::MaxDevBps, &bps);
        Ok(())
    }
}

fn require_admin(env: &Env) -> Result<(), OracleError> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(OracleError::AdminNotSet)?;
    admin.require_auth();
    Ok(())
}

#[cfg(test)]
mod test;
