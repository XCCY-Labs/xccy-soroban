extern crate std;

use crate::{BlendRate, BlendRateClient};
use oraclehub_types::WAD;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger as _},
    Address, Env,
};

// ---------------------------------------------------------------------------
// Mock Blend pool — implements BlendPool with canned per-asset rates.
// ---------------------------------------------------------------------------

#[contract]
pub struct MockBlendPool;

#[contractimpl]
impl MockBlendPool {
    pub fn __constructor(env: Env, borrow_scalar7: i128, supply_scalar7: i128, ts: u64) {
        env.storage().instance().set(&"b", &borrow_scalar7);
        env.storage().instance().set(&"s", &supply_scalar7);
        env.storage().instance().set(&"t", &ts);
    }

    pub fn borrow_rate(env: Env, _asset: Address) -> i128 {
        env.storage().instance().get(&"b").unwrap_or(0)
    }

    pub fn supply_rate(env: Env, _asset: Address) -> i128 {
        env.storage().instance().get(&"s").unwrap_or(0)
    }

    pub fn last_update(env: Env, _asset: Address) -> u64 {
        env.storage().instance().get(&"t").unwrap_or(0)
    }
}

fn setup<'a>(env: &'a Env, borrow: i128, supply: i128, ts: u64) -> (BlendRateClient<'a>, Address) {
    let admin = Address::generate(env);
    let asset = Address::generate(env);
    let pool = env.register(MockBlendPool, (borrow, supply, ts));
    let id = env.register(BlendRate, (&admin, &pool));
    (BlendRateClient::new(env, &id), asset)
}

#[test]
fn borrow_rate_normalised_to_wad() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);

    // 5% APR in SCALAR_7 == 500_000; expected WAD == 5e16
    let (adapter, asset) = setup(&env, 500_000, 0, 90);
    let r = adapter.get_borrow_rate(&asset);
    assert_eq!(r.value, 50_000_000_000_000_000);
    assert_eq!(r.updated_at, 90);
}

#[test]
fn supply_rate_normalised_to_wad() {
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, asset) = setup(&env, 0, 200_000, 80); // 2%
    let r = adapter.get_supply_rate(&asset);
    assert_eq!(r.value, 20_000_000_000_000_000);
}

#[test]
fn zero_borrow_rate_returns_zero() {
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, asset) = setup(&env, 0, 0, 0);
    let r = adapter.get_borrow_rate(&asset);
    assert_eq!(r.value, 0);
}

#[test]
fn peek_rate_returns_supply() {
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, asset) = setup(&env, 999_999, 333_333, 50);
    let r = adapter.peek_rate(&asset);
    assert_eq!(r.value, 33_333_300_000_000_000); // 3.33333% in WAD
}

#[test]
fn unit_rate_max_normalised_correctly() {
    // 100% APR (= 1e7 in SCALAR_7) → 1e18 in WAD
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, asset) = setup(&env, 10_000_000, 0, 1);
    let r = adapter.get_borrow_rate(&asset);
    assert_eq!(r.value, WAD);
}
