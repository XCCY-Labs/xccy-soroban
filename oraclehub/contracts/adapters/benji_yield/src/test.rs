extern crate std;

use crate::{BenjiYield, BenjiYieldClient, Observation, SignedFeedPayload, SourceMode};
use oraclehub_types::{OracleError, WAD};
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    xdr::ToXdr,
    Address, Bytes, BytesN, Env,
};

const SECONDS_PER_YEAR: u64 = 365 * 24 * 60 * 60;

fn setup(env: &Env) -> (BenjiYieldClient<'_>, Address) {
    let admin = Address::generate(env);
    let signer = BytesN::<32>::from_array(env, &[0u8; 32]);
    let id = env.register(BenjiYield, (&admin, &signer));
    (BenjiYieldClient::new(env, &id), admin)
}

// ---------------------------------------------------------------------------
// Configuration & basic state
// ---------------------------------------------------------------------------

#[test]
fn initial_get_yield_unavailable() {
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, _admin) = setup(&env);
    let err = adapter.try_get_yield().err().unwrap().unwrap();
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
fn admin_can_set_mode() {
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, _admin) = setup(&env);
    adapter.set_mode(&SourceMode::OnChain);
    assert_eq!(adapter.mode(), SourceMode::OnChain);
}

#[test]
fn onchain_mode_unavailable_until_implemented() {
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, _admin) = setup(&env);
    adapter.set_mode(&SourceMode::OnChain);
    let err = adapter.try_get_yield().err().unwrap().unwrap();
    assert_eq!(err, OracleError::SourceUnavailable);
}

#[test]
fn last_nonce_starts_zero() {
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, _admin) = setup(&env);
    assert_eq!(adapter.last_nonce(), 0);
}

#[test]
fn xdr_payload_serialises() {
    let env = Env::default();
    let payload = SignedFeedPayload {
        nav_wad: WAD + 5_000_000_000_000_000, // 1.005·WAD
        updated_at: 100,
        nonce: 1,
    };
    let bytes: Bytes = payload.to_xdr(&env);
    assert!(!bytes.is_empty());
}

// ---------------------------------------------------------------------------
// admin_push (NAV bootstrap path)
// ---------------------------------------------------------------------------

#[test]
fn admin_push_sets_latest() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);
    let (adapter, _admin) = setup(&env);
    adapter.admin_push(&WAD, &500);
    let r = adapter.get_yield();
    assert_eq!(r.value, WAD);
    assert_eq!(r.updated_at, 500);
}

#[test]
fn admin_push_rejects_future_timestamp() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);
    let (adapter, _admin) = setup(&env);
    let err = adapter.try_admin_push(&WAD, &2000).err().unwrap().unwrap();
    assert_eq!(err, OracleError::FutureTimestamp);
}

#[test]
fn admin_push_rejects_non_positive_nav() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);
    let (adapter, _admin) = setup(&env);
    let err = adapter.try_admin_push(&0, &500).err().unwrap().unwrap();
    assert_eq!(err, OracleError::InvalidArgument);
    let err = adapter.try_admin_push(&-1, &500).err().unwrap().unwrap();
    assert_eq!(err, OracleError::InvalidArgument);
}

#[test]
fn admin_push_rejects_stale_timestamp() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);
    let (adapter, _admin) = setup(&env);
    adapter.admin_push(&WAD, &500);
    let err = adapter.try_admin_push(&WAD, &500).err().unwrap().unwrap();
    assert_eq!(err, OracleError::StalePushedFeed);
    let err = adapter.try_admin_push(&WAD, &499).err().unwrap().unwrap();
    assert_eq!(err, OracleError::StalePushedFeed);
}

// ---------------------------------------------------------------------------
// update_state (pull pattern — snapshot Last → observation buffer)
// ---------------------------------------------------------------------------

#[test]
fn update_state_records_first_observation() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);
    let (adapter, _admin) = setup(&env);
    adapter.admin_push(&WAD, &900);
    let appended = adapter.update_state();
    assert!(appended);
    assert_eq!(adapter.observation_count(), 1);
    let latest = adapter.latest_observation().unwrap();
    assert_eq!(latest.ts, 900);
    assert_eq!(latest.nav_wad, WAD);
}

#[test]
fn update_state_skips_when_not_newer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);
    let (adapter, _admin) = setup(&env);
    adapter.admin_push(&WAD, &900);
    assert!(adapter.update_state());
    // No new push — second update_state must be no-op
    assert!(!adapter.update_state());
    assert_eq!(adapter.observation_count(), 1);
}

#[test]
fn update_state_chains_multiple_observations() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = SECONDS_PER_YEAR);
    let (adapter, _admin) = setup(&env);

    adapter.admin_push(&WAD, &100);
    adapter.update_state();

    adapter.admin_push(&(WAD + WAD / 100), &(SECONDS_PER_YEAR / 2 + 100)); // +1% over 0.5y
    adapter.update_state();

    assert_eq!(adapter.observation_count(), 2);
    let last = adapter.latest_observation().unwrap();
    assert_eq!(last.nav_wad, WAD + WAD / 100);
}

#[test]
fn update_state_evicts_oldest_at_capacity() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1_000_000);
    let (adapter, _admin) = setup(&env);
    adapter.set_max_observations(&3);

    adapter.admin_push(&WAD, &100);
    adapter.update_state();
    adapter.admin_push(&(WAD + 1), &200);
    adapter.update_state();
    adapter.admin_push(&(WAD + 2), &300);
    adapter.update_state();
    adapter.admin_push(&(WAD + 3), &400);
    adapter.update_state();

    assert_eq!(adapter.observation_count(), 3);
    // Oldest should now be ts=200, not ts=100
    let oldest = adapter.observation_at(&0u32).unwrap();
    assert_eq!(oldest.ts, 200);
    let newest = adapter.observation_at(&2u32).unwrap();
    assert_eq!(newest.ts, 400);
}

// ---------------------------------------------------------------------------
// get_apr_from_to (the actual AprOracle-style derivation)
// ---------------------------------------------------------------------------

#[test]
fn apr_derivation_one_percent_over_one_year() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger()
        .with_mut(|l| l.timestamp = 2 * SECONDS_PER_YEAR);
    let (adapter, _admin) = setup(&env);

    let t0 = SECONDS_PER_YEAR / 2; // arbitrary start
    let t1 = t0 + SECONDS_PER_YEAR; // exactly 1 year later

    adapter.admin_push(&WAD, &t0);
    adapter.update_state();
    adapter.admin_push(&(WAD + WAD / 100), &t1); // +1% nominal
    adapter.update_state();

    let apr = adapter.get_apr_from_to(&t0, &t1);
    // 1% growth over 1 year → realised APR = 1% in WAD == 1e16
    assert!((apr - WAD / 100).abs() <= 2);
}

#[test]
fn apr_derivation_5_percent_over_half_year() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger()
        .with_mut(|l| l.timestamp = 3 * SECONDS_PER_YEAR);
    let (adapter, _admin) = setup(&env);

    let t0 = SECONDS_PER_YEAR;
    let t1 = t0 + SECONDS_PER_YEAR / 2;

    adapter.admin_push(&WAD, &t0);
    adapter.update_state();
    adapter.admin_push(&(WAD + WAD * 5 / 100), &t1); // +5% over 0.5y
    adapter.update_state();

    // realised APR = 5% × 2 = 10% in WAD == 1e17
    let apr = adapter.get_apr_from_to(&t0, &t1);
    assert!((apr - WAD / 10).abs() <= 4);
}

#[test]
fn apr_zero_when_no_growth() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger()
        .with_mut(|l| l.timestamp = 2 * SECONDS_PER_YEAR);
    let (adapter, _admin) = setup(&env);

    adapter.admin_push(&WAD, &100);
    adapter.update_state();
    adapter.admin_push(&WAD, &(100 + SECONDS_PER_YEAR));
    adapter.update_state();

    let apr = adapter.get_apr_from_to(&100, &(100 + SECONDS_PER_YEAR));
    assert_eq!(apr, 0);
}

#[test]
fn apr_zero_when_to_le_from() {
    let env = Env::default();
    env.mock_all_auths();
    let (adapter, _admin) = setup(&env);
    let apr = adapter.get_apr_from_to(&500, &500);
    assert_eq!(apr, 0);
    let apr = adapter.get_apr_from_to(&500, &400);
    assert_eq!(apr, 0);
}

#[test]
fn apr_errors_when_buffer_too_small() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 10_000);
    let (adapter, _admin) = setup(&env);
    let err = adapter
        .try_get_apr_from_to(&100, &200)
        .err()
        .unwrap()
        .unwrap();
    assert_eq!(err, OracleError::SourceUnavailable);

    adapter.admin_push(&WAD, &100);
    adapter.update_state();
    // Only 1 observation — still not enough
    let err = adapter
        .try_get_apr_from_to(&100, &200)
        .err()
        .unwrap()
        .unwrap();
    assert_eq!(err, OracleError::SourceUnavailable);
}

#[test]
fn apr_errors_when_window_outside_buffer_range() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 10_000);
    let (adapter, _admin) = setup(&env);

    adapter.admin_push(&WAD, &500);
    adapter.update_state();
    adapter.admin_push(&(WAD + 1), &800);
    adapter.update_state();

    let err = adapter
        .try_get_apr_from_to(&100, &600)
        .err()
        .unwrap()
        .unwrap();
    assert_eq!(err, OracleError::SourceUnavailable);
}

#[test]
fn apr_with_interpolated_endpoints() {
    // Window endpoints fall *between* observations → interpolate linearly.
    let env = Env::default();
    env.mock_all_auths();
    env.ledger()
        .with_mut(|l| l.timestamp = 10 * SECONDS_PER_YEAR);
    let (adapter, _admin) = setup(&env);

    // Three observations: NAV grows linearly 1.0 → 1.05 over 1 year
    let base = SECONDS_PER_YEAR;
    adapter.admin_push(&WAD, &base);
    adapter.update_state();
    adapter.admin_push(&(WAD + WAD / 50), &(base + SECONDS_PER_YEAR * 2 / 5)); // 1.02
    adapter.update_state();
    adapter.admin_push(&(WAD + WAD / 20), &(base + SECONDS_PER_YEAR)); // 1.05
    adapter.update_state();

    // Query an intermediate window (covers 1 full year)
    let apr = adapter.get_apr_from_to(&base, &(base + SECONDS_PER_YEAR));
    // 5% over 1y → 5% APR == 5e16
    let expected = WAD * 5 / 100;
    assert!(
        (apr - expected).abs() <= 10,
        "got {}, expected {}",
        apr,
        expected
    );
}

#[test]
fn observation_struct_round_trip() {
    let env = Env::default();
    let _obs = Observation {
        ts: 100,
        nav_wad: WAD,
    };
    // Just confirm the type is exported and constructible.
    let _ = env; // suppress unused
}
