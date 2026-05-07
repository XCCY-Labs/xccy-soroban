#![no_std]

//! ReflectorPrice adapter — thin SEP-40 wrapper.
//!
//! Each instance of this adapter binds one Reflector subscription contract
//! (one feed). To consume multiple Reflector tiers (CEX/DEX, FX, Stellar DEX),
//! deploy multiple `ReflectorPrice` instances and register them under
//! different `OracleId` keys in the hub.
//!
//! `peek_price(asset)` accepts the standard SEP-40 `Asset` type
//! (`SepAsset::Stellar(Address) | SepAsset::Other(Symbol)`) so it can address
//! both Stellar-native assets (e.g. PYUSD SAC) and external symbols (e.g.
//! `BTC`, `ETH`, `XLM`). Decimals are normalised from the feed's native
//! precision (Reflector default = 14) up to WAD (1e18).

use oraclehub_types::{OracleError, PriceData, SepAsset};
use oraclehub_wad::to_wad;
use soroban_sdk::{
    contract, contractclient, contractimpl, contracttype, symbol_short, Address, Env, Symbol,
};

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Feed,
}

const TOPIC_INIT: Symbol = symbol_short!("init");
const TOPIC_FEED: Symbol = symbol_short!("feed_set");

/// SEP-40-shaped client we generate against the registered feed contract.
///
/// Field names and ordering must match SEP-40 exactly — Reflector returns
/// `PriceData { price, timestamp }`, so our shared `oraclehub_types::PriceData`
/// uses the same shape and we can pass it through unchanged.
#[contractclient(name = "Sep40Client")]
pub trait Sep40Feed {
    fn lastprice(env: Env, asset: SepAsset) -> Option<PriceData>;
    fn decimals(env: Env) -> u32;
}

#[contract]
pub struct ReflectorPrice;

#[contractimpl]
impl ReflectorPrice {
    pub fn __constructor(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            soroban_sdk::panic_with_error!(env, OracleError::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.events().publish((TOPIC_INIT,), admin);
    }

    pub fn set_feed(env: Env, feed: Address) -> Result<(), OracleError> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Feed, &feed);
        env.events().publish((TOPIC_FEED,), feed);
        Ok(())
    }

    pub fn feed(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Feed)
    }

    /// Read the latest price for `asset`, normalised to WAD precision.
    pub fn peek_price(env: Env, asset: SepAsset) -> Result<PriceData, OracleError> {
        let feed_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Feed)
            .ok_or(OracleError::SourceUnavailable)?;

        let client = Sep40Client::new(&env, &feed_addr);
        let raw = client
            .lastprice(&asset)
            .ok_or(OracleError::SourceUnavailable)?;
        let decimals = client.decimals();
        let price_wad = to_wad(&env, raw.price, decimals)?;
        Ok(PriceData {
            price: price_wad,
            timestamp: raw.timestamp,
        })
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
