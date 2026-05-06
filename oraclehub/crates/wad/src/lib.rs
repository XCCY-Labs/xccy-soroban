#![no_std]

//! WAD-precision (1e18) fixed-point math on `i128`.
//!
//! All operations route through `soroban_sdk::I256` for intermediate `mul_div`
//! to preserve full precision and detect overflow. The crate never panics:
//! every fallible operation returns `Result<i128, OracleError>`.

use oraclehub_types::{OracleError, WAD};
use soroban_sdk::{Env, I256};

/// Multiplies two WAD-scaled values and returns a WAD-scaled product.
///
/// `result = (a * b) / WAD`, computed via `I256` to avoid overflow.
pub fn mul_wad(env: &Env, a: i128, b: i128) -> Result<i128, OracleError> {
    mul_div_i128(env, a, b, WAD)
}

/// Divides two WAD-scaled values, returning a WAD-scaled quotient.
///
/// `result = (a * WAD) / b`, computed via `I256`.
pub fn div_wad(env: &Env, a: i128, b: i128) -> Result<i128, OracleError> {
    if b == 0 {
        return Err(OracleError::DivideByZero);
    }
    mul_div_i128(env, a, WAD, b)
}

/// Computes `(a * b) / d` with `I256` intermediate precision.
///
/// Returns `Err(DivideByZero)` if `d == 0`, `Err(Overflow)` if the result
/// doesn't fit in `i128`.
pub fn mul_div_i128(env: &Env, a: i128, b: i128, d: i128) -> Result<i128, OracleError> {
    if d == 0 {
        return Err(OracleError::DivideByZero);
    }
    let a256 = I256::from_i128(env, a);
    let b256 = I256::from_i128(env, b);
    let d256 = I256::from_i128(env, d);
    let prod = a256.mul(&b256);
    let quot = prod.div(&d256);
    quot.to_i128().ok_or(OracleError::Overflow)
}

/// Linear ramp: `lerp(a, b, t/WAD)`.
///
/// Useful for piecewise-linear IR curves and signed-feed interpolation.
pub fn lerp(env: &Env, a: i128, b: i128, t_wad: i128) -> Result<i128, OracleError> {
    if !(0..=WAD).contains(&t_wad) {
        return Err(OracleError::InvalidArgument);
    }
    let delta = b.checked_sub(a).ok_or(OracleError::Overflow)?;
    let scaled = mul_div_i128(env, delta, t_wad, WAD)?;
    a.checked_add(scaled).ok_or(OracleError::Overflow)
}

/// Convert a value with `from_decimals` decimal precision up to WAD (18 decimals).
///
/// E.g. `to_wad(env, 5_000_000, 7) = 5_000_000 * 10^11 = 5e17` (Blend SCALAR_7 → WAD).
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
    fn mul_div_overflow_returns_err_not_panic() {
        let env = Env::default();
        // i128::MAX * 2 / 1 overflows i128
        assert_eq!(
            mul_div_i128(&env, i128::MAX, 2, 1),
            Err(OracleError::Overflow)
        );
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
        // RAY 1e27 in 27 decimals → WAD: 1e27 / 1e9 = 1e18
        assert_eq!(
            to_wad(&env, 1_000_000_000_000_000_000_000_000_000, 27),
            Ok(WAD)
        );
    }
}
