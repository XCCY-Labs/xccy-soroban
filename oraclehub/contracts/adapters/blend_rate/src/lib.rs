#![no_std]

//! BlendRate adapter — reads variable rates from a Blend `Pool` contract.
//!
//! In production, the adapter calls `Pool::get_reserve_data(asset)` and
//! computes borrow/supply rates from the IR utilisation curve. For v0.1 the
//! adapter is wired to a `BlendPoolClient` trait that the production Blend
//! Pool contract satisfies; tests use a mock pool returning canned reserve
//! data.
//!
//! Blend uses 7-decimal `SCALAR_7` precision; rates are normalised up to WAD.

use oraclehub_types::{OracleError, RateData};
use oraclehub_wad::to_wad;
use soroban_sdk::{
    contract, contractclient, contractimpl, contracttype, symbol_short, Address, Env, Symbol,
};

const BLEND_DECIMALS: u32 = 7;

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Pool,
}

/// Subset of Blend's `Pool` interface that the adapter consumes.
///
/// Real Blend pool returns a struct with `borrow_rate_scalar7` and
/// `supply_rate_scalar7` among other fields; we expose the minimum needed.
#[contractclient(name = "BlendPoolClient")]
pub trait BlendPool {
    fn borrow_rate(env: Env, asset: Address) -> i128;
    fn supply_rate(env: Env, asset: Address) -> i128;
    fn last_update(env: Env, asset: Address) -> u64;
}

const TOPIC_INIT: Symbol = symbol_short!("init");
const TOPIC_POOL: Symbol = symbol_short!("pool_set");

#[contract]
pub struct BlendRate;

#[contractimpl]
impl BlendRate {
    pub fn __constructor(env: Env, admin: Address, pool: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            soroban_sdk::panic_with_error!(env, OracleError::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Pool, &pool);
        env.events().publish((TOPIC_INIT,), (admin, pool));
    }

    pub fn set_pool(env: Env, pool: Address) -> Result<(), OracleError> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Pool, &pool);
        env.events().publish((TOPIC_POOL,), pool);
        Ok(())
    }

    pub fn pool(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Pool)
    }

    pub fn get_borrow_rate(env: Env, asset: Address) -> Result<RateData, OracleError> {
        let pool = pool_addr(&env)?;
        let client = BlendPoolClient::new(&env, &pool);
        let raw = client.borrow_rate(&asset);
        let ts = client.last_update(&asset);
        let value = to_wad(&env, raw, BLEND_DECIMALS)?;
        Ok(RateData {
            value,
            updated_at: ts,
        })
    }

    pub fn get_supply_rate(env: Env, asset: Address) -> Result<RateData, OracleError> {
        let pool = pool_addr(&env)?;
        let client = BlendPoolClient::new(&env, &pool);
        let raw = client.supply_rate(&asset);
        let ts = client.last_update(&asset);
        let value = to_wad(&env, raw, BLEND_DECIMALS)?;
        Ok(RateData {
            value,
            updated_at: ts,
        })
    }

    /// Unified rate-adapter entry point used by the Hub. Defaults to supply rate
    /// (variable yield leg of the receiver strategy).
    pub fn peek_rate(env: Env, asset: Address) -> Result<RateData, OracleError> {
        Self::get_supply_rate(env, asset)
    }
}

fn pool_addr(env: &Env) -> Result<Address, OracleError> {
    env.storage()
        .instance()
        .get(&DataKey::Pool)
        .ok_or(OracleError::AdminNotSet)
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
