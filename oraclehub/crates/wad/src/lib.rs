#![no_std]

//! WAD-precision (1e18) fixed-point math.
//!
//! Thin shim over [`soroban_fixed_point_math::SorobanFixedPoint`] (Script3 /
//! Blend authors), which routes all `mul_div` operations through `I256`
//! intermediates to defeat phantom overflow. The third-party crate is the
//! canonical Soroban math primitive — used inside Blend itself — so we don't
//! reinvent it; we just give it a more domain-y surface (`mul_wad`, `div_wad`,
//! `to_wad`, `lerp`) and pre-validate edge cases that would otherwise panic.
//!
//! ## Error model
//!
//! `SorobanFixedPoint` *panics* on overflow / divide-by-zero (matching
//! Soroban host semantics). We pre-check `denominator == 0` and surface that
//! as `Err(OracleError::DivideByZero)` so contract callers never get a
//! generic panic for the most common error case. Overflow remains a panic
//! — financial inputs at our protocol scale stay well inside `I256`'s 256-bit
//! intermediate range, so overflow indicates a programming bug, not a user
//! input issue.

use oraclehub_types::{OracleError, WAD};
use soroban_fixed_point_math::SorobanFixedPoint;
use soroban_sdk::Env;

/// `floor(a · b / WAD)` — multiplication of WAD-scaled values.
pub fn mul_wad(env: &Env, a: i128, b: i128) -> Result<i128, OracleError> {
    Ok(a.fixed_mul_floor(env, &b, &WAD))
}

/// `floor(a · WAD / b)` — division of WAD-scaled values.
pub fn div_wad(env: &Env, a: i128, b: i128) -> Result<i128, OracleError> {
    if b == 0 {
        return Err(OracleError::DivideByZero);
    }
    // SorobanFixedPoint::fixed_div_floor(x, y, denom) computes floor(x * denom / y).
    Ok(a.fixed_div_floor(env, &b, &WAD))
}

/// `floor(a · b / d)` with `I256` intermediate. Returns `Err` for `d == 0`;
/// panics on overflow (same semantics as `SorobanFixedPoint`).
pub fn mul_div_i128(env: &Env, a: i128, b: i128, d: i128) -> Result<i128, OracleError> {
    if d == 0 {
        return Err(OracleError::DivideByZero);
    }
    Ok(a.fixed_mul_floor(env, &b, &d))
}

/// Linear ramp: `lerp(a, b, t/WAD)`. `t_wad` must be in `[0, WAD]`.
pub fn lerp(env: &Env, a: i128, b: i128, t_wad: i128) -> Result<i128, OracleError> {
    if !(0..=WAD).contains(&t_wad) {
        return Err(OracleError::InvalidArgument);
    }
    let delta = b.checked_sub(a).ok_or(OracleError::Overflow)?;
    let scaled = mul_div_i128(env, delta, t_wad, WAD)?;
    a.checked_add(scaled).ok_or(OracleError::Overflow)
}

/// Convert `value` from `from_decimals` precision up/down to WAD (18 decimals).
///
/// E.g. `to_wad(env, 5_000_000, 7) = 5_000_000 · 10¹¹ = 5e17` (Blend SCALAR_7 → WAD).
pub fn to_wad(env: &Env, value: i128, from_decimals: u32) -> Result<i128, OracleError> {
    if from_decimals > 38 {
        return Err(OracleError::InvalidArgument);
    }
    if from_decimals == 18 {
        return Ok(value);
    }
    if from_decimals < 18 {
        let factor = ten_pow(18 - from_decimals).ok_or(OracleError::Overflow)?;
        mul_div_i128(env, value, factor, 1)
    } else {
        let factor = ten_pow(from_decimals - 18).ok_or(OracleError::Overflow)?;
        mul_div_i128(env, value, 1, factor)
    }
}

fn ten_pow(exp: u32) -> Option<i128> {
    let mut result: i128 = 1;
    for _ in 0..exp {
        result = result.checked_mul(10)?;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::Env;

    #[test]
    fn mul_wad_identity() {
        let env = Env::default();
        assert_eq!(mul_wad(&env, WAD, WAD), Ok(WAD));
    }

    #[test]
    fn mul_wad_half_times_half() {
        let env = Env::default();
        let half = WAD / 2;
        assert_eq!(mul_wad(&env, half, half), Ok(WAD / 4));
    }

    #[test]
    fn mul_wad_zero() {
        let env = Env::default();
        assert_eq!(mul_wad(&env, 0, WAD), Ok(0));
        assert_eq!(mul_wad(&env, WAD, 0), Ok(0));
    }

    #[test]
    fn mul_wad_commutative() {
        let env = Env::default();
        let (a, b) = (3 * WAD / 7, 11 * WAD / 13);
        assert_eq!(mul_wad(&env, a, b), mul_wad(&env, b, a));
    }

    #[test]
    fn div_wad_identity() {
        let env = Env::default();
        assert_eq!(div_wad(&env, WAD, WAD), Ok(WAD));
    }

    #[test]
    fn div_wad_half() {
        let env = Env::default();
        assert_eq!(div_wad(&env, WAD / 2, WAD), Ok(WAD / 2));
    }

    #[test]
    fn div_wad_by_zero_errors() {
        let env = Env::default();
        assert_eq!(div_wad(&env, WAD, 0), Err(OracleError::DivideByZero));
    }

    #[test]
    fn mul_div_by_zero_errors() {
        let env = Env::default();
        assert_eq!(mul_div_i128(&env, 1, 1, 0), Err(OracleError::DivideByZero));
    }

    #[test]
    #[should_panic] // SorobanFixedPoint panics on overflow (matches host semantics)
    fn mul_div_overflow_panics() {
        let env = Env::default();
        let _ = mul_div_i128(&env, i128::MAX, 2, 1);
    }

    #[test]
    fn round_trip_within_one_wei() {
        let env = Env::default();
        let a = 7 * WAD / 9;
        let b = 13 * WAD / 11;
        let product = mul_wad(&env, a, b).unwrap();
        let recovered = div_wad(&env, product, b).unwrap();
        assert!((recovered - a).abs() <= 1);
    }

    #[test]
    fn lerp_endpoints() {
        let env = Env::default();
        assert_eq!(lerp(&env, 100, 200, 0), Ok(100));
        assert_eq!(lerp(&env, 100, 200, WAD), Ok(200));
    }

    #[test]
    fn lerp_midpoint() {
        let env = Env::default();
        assert_eq!(lerp(&env, 100, 200, WAD / 2), Ok(150));
    }

    #[test]
    fn lerp_out_of_range_errors() {
        let env = Env::default();
        assert_eq!(
            lerp(&env, 100, 200, WAD + 1),
            Err(OracleError::InvalidArgument)
        );
    }

    #[test]
    fn to_wad_from_scalar7_blend() {
        let env = Env::default();
        // 5% APR in Blend SCALAR_7 = 500_000; in WAD = 5e16
        assert_eq!(to_wad(&env, 500_000, 7), Ok(50_000_000_000_000_000));
    }

    #[test]
    fn to_wad_identity_18() {
        let env = Env::default();
        assert_eq!(to_wad(&env, WAD, 18), Ok(WAD));
    }

    #[test]
    fn to_wad_from_high_precision_22() {
        let env = Env::default();
        // 1e27 in 27 decimals → WAD: 1e27 / 1e9 = 1e18
        assert_eq!(
            to_wad(&env, 1_000_000_000_000_000_000_000_000_000, 27),
            Ok(WAD)
        );
    }
}
