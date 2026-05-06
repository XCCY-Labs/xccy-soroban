//! Differential tests: load `fixtures/solidity_vectors.json` and assert that
//! every vector reproduces within 1 wei of the Solidity reference.

use oraclehub_types::{apply_staleness, within_bps_band, OracleError, PriceGuard, WAD};
use oraclehub_wad::{div_wad, mul_div_i128, mul_wad, to_wad};
use serde_json::Value;
use soroban_sdk::{testutils::Ledger as _, Env};
use std::fs;

const FIXTURE_PATH: &str = "../../fixtures/solidity_vectors.json";
const EPSILON: i128 = 1; // 1 wei at WAD

fn load_vectors() -> Vec<Value> {
    let text = fs::read_to_string(FIXTURE_PATH).expect("missing fixtures/solidity_vectors.json");
    serde_json::from_str(&text).expect("invalid JSON")
}

fn as_i128(v: &Value) -> i128 {
    if let Some(s) = v.as_str() {
        if s == "i128_max" {
            return i128::MAX;
        }
        s.parse().expect("i128 string")
    } else {
        v.as_i64()
            .map(|x| x as i128)
            .unwrap_or_else(|| v.as_u64().expect("numeric input") as i128)
    }
}

fn within_epsilon(actual: i128, expected: i128) -> bool {
    (actual - expected).abs() <= EPSILON
}

#[test]
fn all_solidity_vectors_within_one_wei() {
    let env = Env::default();
    let vectors = load_vectors();
    let mut passed = 0;

    for v in &vectors {
        let name = v["name"].as_str().unwrap();
        let category = v["category"].as_str().unwrap();
        match category {
            "price_guard" => check_price_guard(&env, v, name),
            "price_guard_validation" => check_guard_validation(v, name),
            "wad_math" => check_wad_math(&env, v, name),
            "rate_normalisation" => check_rate_normalisation(&env, v, name),
            "deviation_guard" => check_deviation_guard(v, name),
            other => panic!("unknown vector category: {other} ({name})"),
        }
        passed += 1;
    }

    assert_eq!(passed, vectors.len());
    assert!(passed >= 20, "spec mandates ≥20 vectors; got {passed}");
}

fn check_price_guard(env: &Env, v: &Value, name: &str) {
    let input = &v["input"];
    if input.get("heartbeat_sec").is_some() && input.get("now").is_some() {
        let now = input["now"].as_u64().unwrap();
        env.ledger().with_mut(|l| l.timestamp = now);
        let updated_at = input["updated_at"].as_u64().unwrap();
        let heartbeat = input["heartbeat_sec"].as_u64().unwrap() as u32;
        let actual = apply_staleness(env, updated_at, heartbeat);
        match v["expected"].as_str().unwrap() {
            "Ok" => assert!(actual.is_ok(), "{name}: expected Ok, got {actual:?}"),
            "StaleData" => assert_eq!(actual, Err(OracleError::StaleData), "{name}"),
            "FutureTimestamp" => {
                assert_eq!(actual, Err(OracleError::FutureTimestamp), "{name}")
            }
            other => panic!("{name}: unexpected expected value {other}"),
        }
    } else {
        let value = as_i128(&input["value_wad"]);
        let anchor = as_i128(&input["anchor_wad"]);
        let min = input["min_bps"].as_i64().unwrap() as i32;
        let max = input["max_bps"].as_i64().unwrap() as i32;
        let actual = within_bps_band(value, anchor, min, max);
        let expected = v["expected_within"].as_bool().unwrap();
        assert_eq!(actual, expected, "{name}");
    }
}

fn check_guard_validation(v: &Value, name: &str) {
    let input = &v["input"];
    let g = PriceGuard {
        heartbeat_sec: input["heartbeat_sec"].as_u64().unwrap() as u32,
        clamp_min_bps: input["min_bps"].as_i64().unwrap() as i32,
        clamp_max_bps: input["max_bps"].as_i64().unwrap() as i32,
    };
    let actual = g.validate();
    match v["expected"].as_str().unwrap() {
        "Ok" => assert!(actual.is_ok(), "{name}: expected Ok, got {actual:?}"),
        "InvalidGuardBand" => assert_eq!(actual, Err(OracleError::InvalidGuardBand), "{name}"),
        "GuardBpsOutOfRange" => {
            assert_eq!(actual, Err(OracleError::GuardBpsOutOfRange), "{name}")
        }
        other => panic!("{name}: unexpected expected value {other}"),
    }
}

fn check_wad_math(env: &Env, v: &Value, name: &str) {
    let input = &v["input"];
    if let Some(d_v) = input.get("d") {
        // mul_div vector
        let a = as_i128(&input["a"]);
        let b = as_i128(&input["b"]);
        let d = as_i128(d_v);
        let actual = mul_div_i128(env, a, b, d);
        match v["expected"].as_str() {
            Some("Overflow") => assert_eq!(actual, Err(OracleError::Overflow), "{name}"),
            Some("DivideByZero") => {
                assert_eq!(actual, Err(OracleError::DivideByZero), "{name}")
            }
            _ => panic!("{name}: missing or unexpected expected"),
        }
        return;
    }
    // identity-style: mul or div based on context
    let a = as_i128(&input["a"]);
    let b = as_i128(&input["b"]);
    let expected = as_i128(&v["expected_value"]);
    let actual = if name.contains("div") {
        div_wad(env, a, b).expect("vector should not error")
    } else {
        mul_wad(env, a, b).expect("vector should not error")
    };
    assert!(
        within_epsilon(actual, expected),
        "{name}: actual={actual} expected={expected}"
    );
}

fn check_rate_normalisation(env: &Env, v: &Value, name: &str) {
    let input = &v["input"];
    let expected = as_i128(&v["expected_value"]);
    let actual = if let Some(s7) = input.get("scalar7_value") {
        // Blend SCALAR_7 → WAD: multiply by 1e11 (i.e. to_wad with from_decimals=7)
        to_wad(env, as_i128(s7), 7).expect("decimals conversion fits")
    } else if let Some(aps) = input.get("assets_per_share_wad") {
        // ERC-4626 growth: max(0, aps - WAD)
        let aps_v = as_i128(aps);
        if aps_v >= WAD {
            aps_v - WAD
        } else {
            0
        }
    } else {
        panic!("{name}: rate_normalisation needs scalar7_value or assets_per_share_wad");
    };
    assert!(
        within_epsilon(actual, expected),
        "{name}: actual={actual} expected={expected}"
    );
}

fn check_deviation_guard(v: &Value, name: &str) {
    let input = &v["input"];
    let current = as_i128(&input["current_wad"]);
    let new_v = as_i128(&input["new_wad"]);
    let max_dev_bps = input["max_dev_bps"].as_u64().unwrap() as i128;

    // Replicates the deviation check in custom_apr/src/lib.rs:
    let result: Result<(), OracleError> = if current != 0 && max_dev_bps > 0 {
        let delta = (new_v - current).abs();
        let lhs = delta.saturating_mul(10_000);
        let rhs = current.saturating_mul(max_dev_bps);
        if lhs > rhs {
            Err(OracleError::DeviationExceeded)
        } else {
            Ok(())
        }
    } else {
        Ok(())
    };

    match v["expected"].as_str().unwrap() {
        "Ok" => assert!(result.is_ok(), "{name}: expected Ok, got {result:?}"),
        "DeviationExceeded" => {
            assert_eq!(result, Err(OracleError::DeviationExceeded), "{name}")
        }
        other => panic!("{name}: unexpected expected value {other}"),
    }
}
