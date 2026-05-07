#![allow(deprecated)] // soroban-sdk 25: events().publish migrating to #[contractevent], TODO
#![no_std]

//! BlendRate adapter — reads variable rates from a Blend Protocol V2 `Pool`.
//!
//! Uses the canonical `blend-contract-sdk` crate, which `contractimport!`s the
//! published Pool WASM and auto-derives the `Reserve` / `ReserveData` /
//! `ReserveConfig` types and the cross-contract `Client`. We call
//! `pool.get_reserve(asset)` and surface the cumulative bToken / dToken indices
//! re-scaled from Blend's 12-decimal precision (`SCALAR_12 = 1e12`) up to WAD
//! (`1e18`).
//!
//! Returning the *cumulative index* (rather than an instantaneous APR) is the
//! same primitive used by the Solidity `AprOracle` reference: downstream
//! consumers compute realised APR as `(idx_now − idx_then) / Δt`.

use blend_contract_sdk::pool;
use oraclehub_types::{OracleError, RateData};
use oraclehub_wad::mul_div_i128;
use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Address, Env, Symbol};
use stellar_access::ownable::{self as ownable, Ownable};
use stellar_macros::only_owner;

/// Blend stores rates as i128 with 12 decimals. WAD has 18, so we scale by 1e6.
const BLEND_TO_WAD_SCALAR: i128 = 1_000_000;

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Pool,
}

const TOPIC_POOL: Symbol = symbol_short!("pool_set");

#[contract]
pub struct BlendRate;

#[contractimpl]
impl BlendRate {
    pub fn __constructor(env: Env, admin: Address, pool: Address) {
        ownable::set_owner(&env, &admin);
        env.storage().instance().set(&DataKey::Pool, &pool);
    }

    #[only_owner]
    pub fn set_pool(env: Env, pool: Address) {
        env.storage().instance().set(&DataKey::Pool, &pool);
        env.events().publish((TOPIC_POOL,), pool);
    }

    pub fn pool(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Pool)
    }

    /// Cumulative supply index (`b_rate`) re-scaled to WAD.
    ///
    /// Grows monotonically as the pool accrues interest. Two consecutive
    /// reads divided by Δt yield the realised supply APR over that window.
    pub fn get_supply_rate(env: Env, asset: Address) -> Result<RateData, OracleError> {
        let reserve = read_reserve(&env, &asset)?;
        let value = mul_div_i128(&env, reserve.data.b_rate, BLEND_TO_WAD_SCALAR, 1)?;
        Ok(RateData {
            value,
            updated_at: reserve.data.last_time,
        })
    }

    /// Cumulative debt index (`d_rate`) re-scaled to WAD.
    pub fn get_borrow_rate(env: Env, asset: Address) -> Result<RateData, OracleError> {
        let reserve = read_reserve(&env, &asset)?;
        let value = mul_div_i128(&env, reserve.data.d_rate, BLEND_TO_WAD_SCALAR, 1)?;
        Ok(RateData {
            value,
            updated_at: reserve.data.last_time,
        })
    }

    /// Pool utilisation in WAD (the `config.util` field is the *target* util,
    /// not the *current* one — see `compute_utilisation` for the live value).
    pub fn get_utilisation(env: Env, asset: Address) -> Result<i128, OracleError> {
        let reserve = read_reserve(&env, &asset)?;
        compute_utilisation(&env, &reserve)
    }

    /// Unified rate-adapter entry point used by the Hub. Defaults to the
    /// supply-side cumulative index (variable-yield leg of the receiver strategy).
    pub fn peek_rate(env: Env, asset: Address) -> Result<RateData, OracleError> {
        Self::get_supply_rate(env, asset)
    }
}

#[contractimpl(contracttrait)]
impl Ownable for BlendRate {}

fn read_reserve(env: &Env, asset: &Address) -> Result<pool::Reserve, OracleError> {
    let pool_addr: Address = env
        .storage()
        .instance()
        .get(&DataKey::Pool)
        .ok_or(OracleError::AdminNotSet)?;
    let client = pool::Client::new(env, &pool_addr);
    Ok(client.get_reserve(asset))
}

/// Live utilisation: `d_supply * d_rate / (b_supply * b_rate)`.
///
/// Both supply totals and rate indices are 12-decimal; the ratio is
/// dimensionless and we normalise to WAD by scaling the result.
fn compute_utilisation(env: &Env, reserve: &pool::Reserve) -> Result<i128, OracleError> {
    let total_supply = mul_div_i128(env, reserve.data.b_supply, reserve.data.b_rate, 1)?;
    if total_supply == 0 {
        return Ok(0);
    }
    let total_borrow = mul_div_i128(env, reserve.data.d_supply, reserve.data.d_rate, 1)?;
    mul_div_i128(env, total_borrow, oraclehub_types::WAD, total_supply)
}

#[cfg(test)]
mod test;
