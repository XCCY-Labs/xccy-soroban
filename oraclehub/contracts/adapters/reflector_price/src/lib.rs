#![no_std]

//! ReflectorPrice adapter — wraps a SEP-40-compatible price feed.
//!
//! The adapter holds a per-asset map of feed contract addresses (Reflector
//! "subscription" contracts) and exposes `peek_price(asset)` returning a
//! WAD-precision USD price. Decimals are normalised from the feed's native
//! precision up to 1e18.

use oraclehub_types::{OracleError, PriceData, SepAsset};
use oraclehub_wad::to_wad;
use soroban_sdk::{
    contract, contractclient, contractimpl, contracttype, symbol_short, Address, Env, Map, Symbol,
};

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Feeds, // Map<Address, Address>: asset -> feed contract
    Decimals(Address),
}

const TOPIC_INIT: Symbol = symbol_short!("init");
const TOPIC_FEED: Symbol = symbol_short!("feed_set");

/// SEP-40-shaped client we generate against any registered feed contract.
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
            panic_with_error(&env, OracleError::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        let feeds: Map<Address, Address> = Map::new(&env);
        env.storage().instance().set(&DataKey::Feeds, &feeds);
        env.events().publish((TOPIC_INIT,), admin);
    }

    pub fn set_feed(env: Env, asset: Address, feed: Address) -> Result<(), OracleError> {
        require_admin(&env)?;
        let mut feeds: Map<Address, Address> = env
            .storage()
            .instance()
            .get(&DataKey::Feeds)
            .ok_or(OracleError::AdminNotSet)?;
        feeds.set(asset.clone(), feed.clone());
        env.storage().instance().set(&DataKey::Feeds, &feeds);
        env.events().publish((TOPIC_FEED,), (asset, feed));
        Ok(())
    }

    pub fn unset_feed(env: Env, asset: Address) -> Result<(), OracleError> {
        require_admin(&env)?;
        let mut feeds: Map<Address, Address> = env
            .storage()
            .instance()
            .get(&DataKey::Feeds)
            .ok_or(OracleError::AdminNotSet)?;
        feeds.remove(asset.clone());
        env.storage().instance().set(&DataKey::Feeds, &feeds);
        Ok(())
    }

    pub fn feed_of(env: Env, asset: Address) -> Option<Address> {
        let feeds: Map<Address, Address> = env
            .storage()
            .instance()
            .get(&DataKey::Feeds)
            .unwrap_or(Map::new(&env));
        feeds.get(asset)
    }

    /// Read the latest price for `asset`, normalised to WAD precision.
    pub fn peek_price(env: Env, asset: Address) -> Result<PriceData, OracleError> {
        let feeds: Map<Address, Address> = env
            .storage()
            .instance()
            .get(&DataKey::Feeds)
            .unwrap_or(Map::new(&env));
        let feed_addr = feeds
            .get(asset.clone())
            .ok_or(OracleError::AssetNotSupported)?;

        let client = Sep40Client::new(&env, &feed_addr);
        let raw = client
            .lastprice(&SepAsset::Stellar(asset))
            .ok_or(OracleError::SourceUnavailable)?;
        let decimals = client.decimals();
        let price_wad = to_wad(&env, raw.price, decimals)?;
        Ok(PriceData {
            price: price_wad,
            updated_at: raw.updated_at,
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

fn panic_with_error(env: &Env, e: OracleError) -> ! {
    soroban_sdk::panic_with_error!(env, e)
}

#[cfg(test)]
mod test;
