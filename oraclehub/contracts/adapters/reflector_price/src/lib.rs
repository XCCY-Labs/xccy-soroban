#![allow(deprecated)] // soroban-sdk 25: events().publish migrating to #[contractevent], TODO
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
use stellar_access::ownable::{self as ownable, Ownable};
use stellar_macros::only_owner;

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Feed,
}

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
        ownable::set_owner(&env, &admin);
    }

    #[only_owner]
    pub fn set_feed(env: Env, feed: Address) {
        env.storage().instance().set(&DataKey::Feed, &feed);
        env.events().publish((TOPIC_FEED,), feed);
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

#[contractimpl(contracttrait)]
impl Ownable for ReflectorPrice {}

#[cfg(test)]
mod test;
