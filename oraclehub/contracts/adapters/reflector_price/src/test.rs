extern crate std;

use crate::{ReflectorPrice, ReflectorPriceClient};
use oraclehub_types::{OracleError, PriceData, SepAsset, WAD};
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger as _},
    Address, Env,
};

// ---------------------------------------------------------------------------
// Mock SEP-40 feed used as the upstream Reflector substitute in tests.
// ---------------------------------------------------------------------------

#[contract]
pub struct MockSep40;

#[contractimpl]
impl MockSep40 {
    pub fn __constructor(env: Env, price: i128, updated_at: u64, decimals: u32) {
        env.storage().instance().set(&"price", &price);
        env.storage().instance().set(&"ts", &updated_at);
        env.storage().instance().set(&"dec", &decimals);
    }

    pub fn set(env: Env, price: i128, updated_at: u64) {
        env.storage().instance().set(&"price", &price);
        env.storage().instance().set(&"ts", &updated_at);
    }

    pub fn lastprice(env: Env, _asset: SepAsset) -> Option<PriceData> {
        let price: i128 = env.storage().instance().get(&"price")?;
        let ts: u64 = env.storage().instance().get(&"ts").unwrap_or(0);
        if price == 0 && ts == 0 {
            return None;
        }
        Some(PriceData {
            price,
            updated_at: ts,
        })
    }

    pub fn decimals(env: Env) -> u32 {
        env.storage().instance().get(&"dec").unwrap_or(14)
    }
}

// ---------------------------------------------------------------------------

fn deploy_adapter<'a>(env: &'a Env, admin: &Address) -> ReflectorPriceClient<'a> {
    let id = env.register(ReflectorPrice, (admin,));
    ReflectorPriceClient::new(env, &id)
}

fn deploy_feed(env: &Env, price: i128, ts: u64, decimals: u32) -> Address {
    env.register(MockSep40, (price, ts, decimals))
}

#[test]
fn round_trip_set_feed_and_query() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let admin = Address::generate(&env);
    let asset = Address::generate(&env);

    // Reflector default decimals = 14; price = 1.0 USD = 1e14
    let feed = deploy_feed(&env, 100_000_000_000_000, 950, 14);

    let adapter = deploy_adapter(&env, &admin);
    adapter.set_feed(&asset, &feed);

    let p = adapter.peek_price(&asset);
    assert_eq!(p.price, WAD); // 1e14 normalised up to 1e18
    assert_eq!(p.updated_at, 950);
}

#[test]
fn unsupported_asset_errors() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let asset = Address::generate(&env);
    let adapter = deploy_adapter(&env, &admin);

    let err = adapter.try_peek_price(&asset).err().unwrap().unwrap();
    assert_eq!(err, OracleError::AssetNotSupported);
}

#[test]
fn source_unavailable_when_feed_returns_none() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let admin = Address::generate(&env);
    let asset = Address::generate(&env);
    // 0 price + ts=0 sentinel → mock returns None
    let feed = deploy_feed(&env, 0, 0, 14);
    let adapter = deploy_adapter(&env, &admin);
    adapter.set_feed(&asset, &feed);

    let err = adapter.try_peek_price(&asset).err().unwrap().unwrap();
    assert_eq!(err, OracleError::SourceUnavailable);
}

#[test]
fn feed_of_returns_set_address() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let asset = Address::generate(&env);
    let feed = deploy_feed(&env, 1, 1, 14);
    let adapter = deploy_adapter(&env, &admin);
    adapter.set_feed(&asset, &feed);

    assert_eq!(adapter.feed_of(&asset), Some(feed));
}

#[test]
fn unset_feed_clears_mapping() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let asset = Address::generate(&env);
    let feed = deploy_feed(&env, 1, 1, 14);
    let adapter = deploy_adapter(&env, &admin);
    adapter.set_feed(&asset, &feed);
    adapter.unset_feed(&asset);

    assert_eq!(adapter.feed_of(&asset), None);
}

#[test]
fn decimals_normalisation_above_18_compresses() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let admin = Address::generate(&env);
    let asset = Address::generate(&env);
    // 27-decimal price for 1.0 USD = 1e27 → expected WAD = 1e18
    let feed = deploy_feed(&env, 1_000_000_000_000_000_000_000_000_000, 950, 27);
    let adapter = deploy_adapter(&env, &admin);
    adapter.set_feed(&asset, &feed);

    let p = adapter.peek_price(&asset);
    assert_eq!(p.price, WAD);
}
