extern crate std;

use crate::{ReflectorPrice, ReflectorPriceClient};
use oraclehub_types::{OracleError, PriceData, SepAsset, WAD};
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger as _},
    Address, Env, Symbol,
};

// ---------------------------------------------------------------------------
// Mock SEP-40 feed used as the upstream Reflector substitute in tests.
// Field names (`timestamp`) and types match SEP-40 exactly.
// ---------------------------------------------------------------------------

#[contract]
pub struct MockSep40;

#[contractimpl]
impl MockSep40 {
    pub fn __constructor(env: Env, price: i128, timestamp: u64, decimals: u32) {
        env.storage().instance().set(&"price", &price);
        env.storage().instance().set(&"ts", &timestamp);
        env.storage().instance().set(&"dec", &decimals);
    }

    pub fn lastprice(env: Env, _asset: SepAsset) -> Option<PriceData> {
        let price: i128 = env.storage().instance().get(&"price")?;
        let ts: u64 = env.storage().instance().get(&"ts").unwrap_or(0);
        if price == 0 && ts == 0 {
            return None;
        }
        Some(PriceData {
            price,
            timestamp: ts,
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
fn round_trip_set_feed_and_query_stellar_asset() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let admin = Address::generate(&env);
    let asset = Address::generate(&env);

    // Reflector default decimals = 14; price = 1.0 USD = 1e14
    let feed = deploy_feed(&env, 100_000_000_000_000, 950, 14);

    let adapter = deploy_adapter(&env, &admin);
    adapter.set_feed(&feed);

    let p = adapter.peek_price(&SepAsset::Stellar(asset));
    assert_eq!(p.price, WAD);
    assert_eq!(p.timestamp, 950);
}

#[test]
fn query_other_symbol_asset() {
    // External CEX/DEX-style query: asset is a Symbol (e.g. "BTC")
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    // BTC at $81,032.50 in 14-dec precision
    let feed = deploy_feed(&env, 8_103_250_000_000_000_000, 1778100000, 14);

    let adapter = deploy_adapter(&env, &admin);
    adapter.set_feed(&feed);

    let p = adapter.peek_price(&SepAsset::Other(Symbol::new(&env, "BTC")));
    // 8.10325e18 / 1e14 * 1e18 = 8.10325e22 (BTC price in WAD)
    assert_eq!(p.price, 81_032_500_000_000_000_000_000);
}

#[test]
fn no_feed_returns_source_unavailable() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let adapter = deploy_adapter(&env, &admin);

    let asset = SepAsset::Other(Symbol::new(&env, "BTC"));
    let err = adapter.try_peek_price(&asset).err().unwrap().unwrap();
    assert_eq!(err, OracleError::SourceUnavailable);
}

#[test]
fn source_returns_none_propagates_unavailable() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let feed = deploy_feed(&env, 0, 0, 14);
    let adapter = deploy_adapter(&env, &admin);
    adapter.set_feed(&feed);

    let asset = SepAsset::Other(Symbol::new(&env, "BTC"));
    let err = adapter.try_peek_price(&asset).err().unwrap().unwrap();
    assert_eq!(err, OracleError::SourceUnavailable);
}

#[test]
fn feed_getter_returns_set_address() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let feed = deploy_feed(&env, 1, 1, 14);
    let adapter = deploy_adapter(&env, &admin);
    adapter.set_feed(&feed);

    assert_eq!(adapter.feed(), Some(feed));
}

#[test]
fn decimals_normalisation_above_18_compresses() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let admin = Address::generate(&env);
    // 27-decimal price for 1.0 USD = 1e27 → expected WAD = 1e18
    let feed = deploy_feed(&env, 1_000_000_000_000_000_000_000_000_000, 950, 27);
    let adapter = deploy_adapter(&env, &admin);
    adapter.set_feed(&feed);

    let asset = SepAsset::Other(Symbol::new(&env, "FOO"));
    let p = adapter.peek_price(&asset);
    assert_eq!(p.price, WAD);
}
