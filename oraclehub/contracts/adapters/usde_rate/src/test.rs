extern crate std;

use crate::{SourceMode, UsdeRate, UsdeRateClient};
use oraclehub_types::{OracleError, WAD};
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger as _},
    Address, BytesN, Env,
};

// ---------------------------------------------------------------------------
// Mock ERC-4626 vault — returns canned `convert_to_assets`.
// ---------------------------------------------------------------------------

#[contract]
pub struct MockErc4626;

#[contractimpl]
impl MockErc4626 {
    pub fn __constructor(env: Env, assets_per_share: i128, ts: u64) {
        env.storage().instance().set(&"a", &assets_per_share);
        env.storage().instance().set(&"t", &ts);
    }

    pub fn convert_to_assets(env: Env, shares_wad: i128) -> i128 {
        let aps: i128 = env.storage().instance().get(&"a").unwrap_or(WAD);
        // Linear: returns shares_wad scaled by aps/WAD
        aps.saturating_mul(shares_wad) / WAD
    }

    pub fn last_update(env: Env) -> u64 {
        env.storage().instance().get(&"t").unwrap_or(0)
    }
}

fn setup<'a>(env: &'a Env) -> (UsdeRateClient<'a>, Address) {
    let admin = Address::generate(env);
    let signer = BytesN::<32>::from_array(env, &[0u8; 32]);
    let id = env.register(UsdeRate, (&admin, &signer));
    (UsdeRateClient::new(env, &id), admin)
}

#[test]
fn initial_signed_mode_no_data() {
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, _admin) = setup(&env);
    let err = adapter.try_get_rate().err().unwrap().unwrap();
    assert_eq!(err, OracleError::SourceUnavailable);
}

#[test]
fn onchain_mode_with_vault_returns_growth() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);
    let (adapter, _admin) = setup(&env);
    // 1.05 USDe per sUSDe share = 5% growth
    let vault = env.register(MockErc4626, (1_050_000_000_000_000_000_i128, 950_u64));
    adapter.set_vault(&vault);
    adapter.set_mode(&SourceMode::OnChain);

    let r = adapter.get_rate();
    assert_eq!(r.value, 50_000_000_000_000_000); // 5e16 = 5% growth
    assert_eq!(r.updated_at, 950);
}

#[test]
fn onchain_negative_growth_clamped_to_zero() {
    // sUSDe NAV slightly below 1.0 (impossible in normal Ethena ops, but
    // defensive): we surface 0 growth, not negative.
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, _admin) = setup(&env);
    let vault = env.register(MockErc4626, (999_000_000_000_000_000_i128, 100_u64));
    adapter.set_vault(&vault);
    adapter.set_mode(&SourceMode::OnChain);

    let r = adapter.get_rate();
    assert_eq!(r.value, 0);
}

#[test]
fn onchain_zero_aps_errors() {
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, _admin) = setup(&env);
    let vault = env.register(MockErc4626, (0_i128, 100_u64));
    adapter.set_vault(&vault);
    adapter.set_mode(&SourceMode::OnChain);

    let err = adapter.try_get_rate().err().unwrap().unwrap();
    assert_eq!(err, OracleError::SourceUnavailable);
}

#[test]
fn mode_default_is_signed() {
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, _admin) = setup(&env);
    assert_eq!(adapter.mode(), SourceMode::Signed);
}

#[test]
fn admin_can_switch_modes() {
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, _admin) = setup(&env);
    adapter.set_mode(&SourceMode::OnChain);
    assert_eq!(adapter.mode(), SourceMode::OnChain);
    adapter.set_mode(&SourceMode::Signed);
    assert_eq!(adapter.mode(), SourceMode::Signed);
}

#[test]
fn onchain_no_vault_errors() {
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, _admin) = setup(&env);
    adapter.set_mode(&SourceMode::OnChain);
    let err = adapter.try_get_rate().err().unwrap().unwrap();
    assert_eq!(err, OracleError::SourceUnavailable);
}
