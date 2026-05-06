extern crate std;

use crate::{CustomApr, CustomAprClient};
use oraclehub_types::{OracleError, WAD};
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    Address, Env,
};

const FIVE_PCT: i128 = 50_000_000_000_000_000; // 5e16 = 5% APR in WAD
const SIX_PCT: i128 = 60_000_000_000_000_000;
const TEN_PCT: i128 = 100_000_000_000_000_000;

fn setup<'a>(env: &'a Env, max_dev_bps: u32) -> (CustomAprClient<'a>, Address) {
    let admin = Address::generate(env);
    let id = env.register(CustomApr, (&admin, max_dev_bps));
    (CustomAprClient::new(env, &id), admin)
}

#[test]
fn initial_get_apr_unavailable() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    let (adapter, _admin) = setup(&env, 1000);
    let err = adapter.try_get_apr().err().unwrap().unwrap();
    assert_eq!(err, OracleError::SourceUnavailable);
}

#[test]
fn set_apr_with_future_effective_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    let (adapter, _admin) = setup(&env, 1000);
    adapter.set_apr(&FIVE_PCT, &200);
    assert_eq!(adapter.pending(), Some((FIVE_PCT, 200)));
}

#[test]
fn set_apr_with_past_effective_errors() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    let (adapter, _admin) = setup(&env, 1000);
    let err = adapter.try_set_apr(&FIVE_PCT, &50).err().unwrap().unwrap();
    assert_eq!(err, OracleError::InvalidEffectiveAt);
}

#[test]
fn set_apr_negative_errors() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    let (adapter, _admin) = setup(&env, 1000);
    let err = adapter.try_set_apr(&-1, &200).err().unwrap().unwrap();
    assert_eq!(err, OracleError::InvalidArgument);
}

#[test]
fn promote_after_effective_advances_state() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    let (adapter, _admin) = setup(&env, 1000);
    adapter.set_apr(&FIVE_PCT, &200);
    env.ledger().with_mut(|l| l.timestamp = 200);
    let promoted = adapter.promote();
    assert!(promoted);
    let r = adapter.get_apr();
    assert_eq!(r.value, FIVE_PCT);
    assert_eq!(r.updated_at, 200);
}

#[test]
fn promote_before_effective_is_noop() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    let (adapter, _admin) = setup(&env, 1000);
    adapter.set_apr(&FIVE_PCT, &200);
    let promoted = adapter.promote();
    assert!(!promoted);
}

#[test]
fn deviation_guard_rejects_excessive_change() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    // 1000 bps = 10% max change
    let (adapter, _admin) = setup(&env, 1000);
    adapter.set_apr(&FIVE_PCT, &200);
    env.ledger().with_mut(|l| l.timestamp = 200);
    adapter.promote();

    // From 5% to 10% = 100% deviation, far above 10% allowed.
    env.ledger().with_mut(|l| l.timestamp = 300);
    let err = adapter.try_set_apr(&TEN_PCT, &400).err().unwrap().unwrap();
    assert_eq!(err, OracleError::DeviationExceeded);
}

#[test]
fn deviation_guard_accepts_small_change() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    // 2000 bps = 20% max change
    let (adapter, _admin) = setup(&env, 2000);
    adapter.set_apr(&FIVE_PCT, &200);
    env.ledger().with_mut(|l| l.timestamp = 200);
    adapter.promote();

    // From 5% to 6% = 20% deviation, exactly at the bound.
    env.ledger().with_mut(|l| l.timestamp = 300);
    adapter.set_apr(&SIX_PCT, &400);
    assert_eq!(adapter.pending(), Some((SIX_PCT, 400)));
}

#[test]
fn deviation_zero_max_disables_check() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    let (adapter, _admin) = setup(&env, 0);
    adapter.set_apr(&FIVE_PCT, &200);
    env.ledger().with_mut(|l| l.timestamp = 200);
    adapter.promote();
    env.ledger().with_mut(|l| l.timestamp = 300);
    // First promotion sets current to FIVE_PCT, then huge jump should still pass when max_dev=0
    adapter.set_apr(&TEN_PCT, &400);
    assert_eq!(adapter.pending(), Some((TEN_PCT, 400)));
}

#[test]
fn cancel_pending_clears() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    let (adapter, _admin) = setup(&env, 1000);
    adapter.set_apr(&FIVE_PCT, &200);
    adapter.cancel_pending();
    assert_eq!(adapter.pending(), None);
}

#[test]
fn get_apr_auto_promotes() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    let (adapter, _admin) = setup(&env, 1000);
    adapter.set_apr(&FIVE_PCT, &200);
    env.ledger().with_mut(|l| l.timestamp = 250);
    // No explicit promote; get_apr should auto-promote.
    let r = adapter.get_apr();
    assert_eq!(r.value, FIVE_PCT);
}

#[test]
fn deviation_first_set_is_unrestricted() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    let (adapter, _admin) = setup(&env, 100);
    // current == 0 → deviation check is skipped on the very first set
    adapter.set_apr(&WAD, &200);
    assert_eq!(adapter.pending(), Some((WAD, 200)));
}
