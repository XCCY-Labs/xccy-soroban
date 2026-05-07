extern crate std;

use crate::{BlendRate, BlendRateClient, BLEND_TO_WAD_SCALAR};
use blend_contract_sdk::pool;
use oraclehub_types::WAD;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger as _},
    Address, Env,
};

// ---------------------------------------------------------------------------
// Mock Blend pool that satisfies just the `get_reserve(asset)` method we need.
// We construct the auto-generated `pool::Reserve` directly so the wire-level
// XDR encoding matches the real Blend Pool ABI exactly.
// ---------------------------------------------------------------------------

#[contract]
pub struct MockBlendPool;

#[contractimpl]
impl MockBlendPool {
    pub fn __constructor(
        env: Env,
        b_rate: i128,
        d_rate: i128,
        b_supply: i128,
        d_supply: i128,
        last_time: u64,
    ) {
        env.storage().instance().set(&"br", &b_rate);
        env.storage().instance().set(&"dr", &d_rate);
        env.storage().instance().set(&"bs", &b_supply);
        env.storage().instance().set(&"ds", &d_supply);
        env.storage().instance().set(&"lt", &last_time);
    }

    pub fn get_reserve(env: Env, asset: Address) -> pool::Reserve {
        // Blend's `SCALAR_7 = 10_000_000` represents 1.0 with 7 decimals. The
        // following values are in that scale: 9_500_000 = 0.95, 7_000_000 = 0.7,
        // and so on.
        pool::Reserve {
            asset,
            config: pool::ReserveConfig {
                c_factor: 9_500_000,
                decimals: 7,
                enabled: true,
                index: 0,
                l_factor: 9_500_000,
                max_util: 9_500_000,
                r_base: 50_000,
                r_one: 300_000,
                r_three: 10_000_000,
                r_two: 1_000_000,
                reactivity: 20,
                supply_cap: i128::MAX,
                util: 7_000_000,
            },
            data: pool::ReserveData {
                b_rate: env.storage().instance().get(&"br").unwrap_or(0),
                b_supply: env.storage().instance().get(&"bs").unwrap_or(0),
                backstop_credit: 0,
                d_rate: env.storage().instance().get(&"dr").unwrap_or(0),
                d_supply: env.storage().instance().get(&"ds").unwrap_or(0),
                ir_mod: 1_000_000,
                last_time: env.storage().instance().get(&"lt").unwrap_or(0),
            },
            scalar: 10_000_000,
        }
    }
}

fn setup<'a>(
    env: &'a Env,
    b_rate: i128,
    d_rate: i128,
    b_supply: i128,
    d_supply: i128,
    last_time: u64,
) -> (BlendRateClient<'a>, Address) {
    let admin = Address::generate(env);
    let asset = Address::generate(env);
    let pool = env.register(
        MockBlendPool,
        (b_rate, d_rate, b_supply, d_supply, last_time),
    );
    let id = env.register(BlendRate, (&admin, &pool));
    (BlendRateClient::new(env, &id), asset)
}

#[test]
fn supply_rate_b_rate_scaled_to_wad() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);

    // b_rate = 1.055701727823 in 12-dec → in WAD == 1.055701727823e18
    let (adapter, asset) = setup(&env, 1_055_701_727_823, 0, 0, 0, 90);
    let r = adapter.get_supply_rate(&asset);
    assert_eq!(r.value, 1_055_701_727_823 * BLEND_TO_WAD_SCALAR);
    assert_eq!(r.updated_at, 90);
}

#[test]
fn borrow_rate_d_rate_scaled_to_wad() {
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, asset) = setup(&env, 0, 1_068_746_303_820, 0, 0, 100);
    let r = adapter.get_borrow_rate(&asset);
    assert_eq!(r.value, 1_068_746_303_820 * BLEND_TO_WAD_SCALAR);
}

#[test]
fn par_index_normalises_to_wad() {
    // b_rate = 1.0 (in 12-dec) — fresh pool with no accrual yet → 1·WAD
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, asset) = setup(&env, 1_000_000_000_000, 0, 0, 0, 1);
    let r = adapter.get_supply_rate(&asset);
    assert_eq!(r.value, WAD);
}

#[test]
fn peek_rate_returns_supply() {
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, asset) = setup(&env, 2_000_000_000_000, 9_999_999_999_999, 0, 0, 50);
    let r = adapter.peek_rate(&asset);
    // peek_rate == supply
    assert_eq!(r.value, 2_000_000_000_000 * BLEND_TO_WAD_SCALAR);
}

#[test]
fn utilisation_70pct() {
    // total_borrow / total_supply = 0.7
    // total_borrow = d_supply * d_rate; total_supply = b_supply * b_rate
    // Pick: b_supply=100, b_rate=1e12 → total_supply=1e14
    //       d_supply=70,  d_rate=1e12 → total_borrow=7e13
    //       util = 7e13 / 1e14 * WAD = 7e17 = 0.7·WAD
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, asset) = setup(&env, 1_000_000_000_000, 1_000_000_000_000, 100, 70, 1);
    let util = adapter.get_utilisation(&asset);
    assert_eq!(util, 7 * WAD / 10);
}

#[test]
fn utilisation_zero_supply_returns_zero() {
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, asset) = setup(&env, 0, 0, 0, 0, 1);
    let util = adapter.get_utilisation(&asset);
    assert_eq!(util, 0);
}
