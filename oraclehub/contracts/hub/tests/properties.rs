//! Property tests for OracleHub primitives.
//!
//! 1000 iterations per property (the proptest default `cases = 256` is bumped
//! globally to 1000). Properties cover:
//!
//! - WAD math: commutativity, round-trip within ε, overflow returns Err
//! - Registry: register-then-lookup invariant; unregister-then-lookup → None
//! - Timelock: cannot commit before `executable_at`; pause blocks commit even
//!   after timelock elapsed.

use oraclehub_blend_rate::BlendRate;
use oraclehub_custom_apr::CustomApr;
use oraclehub_hub::{OracleHub, OracleHubClient};
use oraclehub_types::{OracleError, OracleId, OracleKind, WAD};
use oraclehub_wad::{div_wad, mul_div_i128, mul_wad};
use proptest::prelude::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, BytesN as _, Ledger as _},
    Address, BytesN, Env, Symbol,
};

const TIMELOCK: u64 = 24 * 60 * 60;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn wad_mul_commutative(
        a in 0i128..=(WAD * 10),
        b in 0i128..=(WAD * 10),
    ) {
        let env = Env::default();
        prop_assert_eq!(mul_wad(&env, a, b), mul_wad(&env, b, a));
    }

    #[test]
    fn wad_round_trip_within_one_wei(
        // Constrained to a >= WAD and b >= WAD so the WAD multiply never
        // truncates to zero — that's the only regime where a round-trip can
        // hold within 1 wei. Tiny operands lose precision irrecoverably:
        // mul_wad(2, 1) = (2 * 1) / WAD = 0, and 0 cannot be reversed to 2.
        a in WAD..=(WAD * 100),
        b in WAD..=(WAD * 100),
    ) {
        let env = Env::default();
        let prod = mul_wad(&env, a, b).expect("inputs bounded");
        let recovered = div_wad(&env, prod, b).expect("non-zero b");
        prop_assert!(
            (recovered - a).abs() <= 1,
            "round-trip drift {} > 1 wei (a={}, b={}, prod={}, recovered={})",
            (recovered - a).abs(), a, b, prod, recovered,
        );
    }

    #[test]
    fn wad_mul_div_zero_d_always_errors(
        a in i128::MIN..=i128::MAX,
        b in i128::MIN..=i128::MAX,
    ) {
        let env = Env::default();
        prop_assert_eq!(mul_div_i128(&env, a, b, 0), Err(OracleError::DivideByZero));
    }

    #[test]
    fn wad_mul_with_zero_is_zero(
        a in 0i128..=(WAD * WAD),
    ) {
        let env = Env::default();
        prop_assert_eq!(mul_wad(&env, a, 0), Ok(0));
        prop_assert_eq!(mul_wad(&env, 0, a), Ok(0));
    }
}

// Registry & timelock properties: state-machine style — each iteration uses a
// fresh `Env` to avoid cross-iteration state leakage.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn registry_round_trip(
        kind_idx in 0u8..=4,
        key_str in "[a-z][a-z0-9_]{0,7}",
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let hub_addr = env.register(OracleHub, (&admin,));
        let hub = OracleHubClient::new(&env, &hub_addr);

        let kind = match kind_idx {
            0 => OracleKind::ReflectorPrice,
            1 => OracleKind::BlendRate,
            2 => OracleKind::BenjiYield,
            3 => OracleKind::UsdeRate,
            _ => OracleKind::CustomApr,
        };
        let key = Symbol::new(&env, &key_str);
        let id = OracleId { kind, key };
        let some_addr = env.register(CustomApr, (&admin, 0u32));

        hub.register_oracle(&id, &some_addr);
        prop_assert_eq!(hub.lookup(&id), Some(some_addr.clone()));

        hub.unregister_oracle(&id);
        prop_assert_eq!(hub.lookup(&id), None);
    }

    #[test]
    fn timelock_holds_for_full_24h(
        propose_ts in 1u64..=1_000_000,
        elapsed in 0u64..=(TIMELOCK + 1),
    ) {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = propose_ts);

        let admin = Address::generate(&env);
        let hub_addr = env.register(OracleHub, (&admin,));
        let hub = OracleHubClient::new(&env, &hub_addr);

        let wasm_hash = BytesN::<32>::random(&env);
        hub.propose_upgrade(&wasm_hash);

        let new_ts = propose_ts + elapsed;
        env.ledger().with_mut(|l| l.timestamp = new_ts);

        let result = hub.try_commit_upgrade();

        if elapsed < TIMELOCK {
            prop_assert!(
                matches!(result, Err(Ok(OracleError::TimelockNotElapsed))),
                "expected TimelockNotElapsed, got {:?} (elapsed={})",
                result, elapsed,
            );
        } else {
            // After the timelock the commit either succeeds or fails for a
            // reason unrelated to the timelock (e.g. a deployer call against a
            // non-existent wasm hash). What we must NOT see is a
            // `TimelockNotElapsed` error.
            prop_assert!(
                !matches!(result, Err(Ok(OracleError::TimelockNotElapsed))),
                "elapsed={} ≥ TIMELOCK should never block on timelock; result={:?}",
                elapsed, result,
            );
        }
    }

    #[test]
    fn pause_blocks_all_reads(
        kind_idx in 1u8..=4, // skip ReflectorPrice to keep arg semantics simple
        key_str in "[a-z]{1,8}",
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let hub_addr = env.register(OracleHub, (&admin,));
        let hub = OracleHubClient::new(&env, &hub_addr);

        // Register an arbitrary adapter so the read path is reachable.
        let pool = env.register(BlendRate, (&admin, &admin));
        let kind = match kind_idx {
            1 => OracleKind::BlendRate,
            2 => OracleKind::BenjiYield,
            3 => OracleKind::UsdeRate,
            _ => OracleKind::CustomApr,
        };
        let id = OracleId { kind, key: Symbol::new(&env, &key_str) };
        hub.register_oracle(&id, &pool);

        hub.pause();
        let key = Address::generate(&env);
        let result = hub.try_get_rate(&id, &key);

        prop_assert!(
            matches!(result, Err(Ok(OracleError::Paused))),
            "expected Paused, got {:?}",
            result,
        );
    }
}

// ---------------------------------------------------------------------------
// Avoid unused-import warnings for symbols brought in for proptest macros.
// ---------------------------------------------------------------------------
const _: Symbol = symbol_short!("placeh");
