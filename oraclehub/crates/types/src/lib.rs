#![no_std]

//! Shared types for the OracleHub Soroban port.
//!
//! Provides:
//! - `OracleId` / `OracleKind` — registry identifiers
//! - `RateData` / `PriceData` — adapter return shapes
//! - `OracleError` — `#[contracterror]` enum mapped 1:1 from Solidity custom errors plus
//!   Soroban-specific failure modes
//! - SEP-40-compatible `PriceFeed` client trait

use soroban_sdk::{contracterror, contracttype, Address, Env, Symbol};

pub const WAD: i128 = 1_000_000_000_000_000_000;

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OracleKind {
    ReflectorPrice = 0,
    BlendRate = 1,
    BenjiYield = 2,
    UsdeRate = 3,
    CustomApr = 4,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OracleId {
    pub kind: OracleKind,
    pub key: Symbol,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateData {
    pub value: i128,
    pub updated_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PriceData {
    pub price: i128,
    pub updated_at: u64,
}

/// Minimal SEP-40-compatible price feed surface that adapters consume.
///
/// SEP-40 defines additional methods (`prices`, `x_last_price`, `resolution`,
/// `base`, `assets`); we expose only the two we actually call in v0.1.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SepAsset {
    Stellar(Address),
    Other(Symbol),
}

#[contracterror]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u32)]
pub enum OracleError {
    ZeroAddress = 1,
    InvalidGuardBand = 2,
    GuardBpsOutOfRange = 3,
    SourceUnavailable = 4,
    FutureTimestamp = 5,
    StaleData = 6,
    Overflow = 7,
    DivideByZero = 8,
    Unauthorized = 9,
    OracleNotRegistered = 10,
    AlreadyInitialized = 11,
    TimelockNotElapsed = 12,
    Paused = 13,
    DeviationExceeded = 14,
    BadSignature = 15,
    StalePushedFeed = 16,
    InvalidEffectiveAt = 17,
    AdminNotSet = 18,
    AssetNotSupported = 19,
    InvalidArgument = 20,
}

/// Heartbeat / clamp profile applied to a price source.
///
/// Mirrors `IOracleHub.PriceGuard` from the Solidity reference. `clamp_min_bps`
/// can be negative (e.g. -30 bps to clamp a stablecoin from below).
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PriceGuard {
    pub heartbeat_sec: u32,
    pub clamp_min_bps: i32,
    pub clamp_max_bps: i32,
}

impl PriceGuard {
    pub fn unguarded() -> Self {
        Self {
            heartbeat_sec: 0,
            clamp_min_bps: 0,
            clamp_max_bps: 0,
        }
    }

    pub fn validate(&self) -> Result<(), OracleError> {
        if self.clamp_min_bps > self.clamp_max_bps {
            return Err(OracleError::InvalidGuardBand);
        }
        if self.clamp_min_bps < -10_000 || self.clamp_max_bps > 10_000 {
            return Err(OracleError::GuardBpsOutOfRange);
        }
        Ok(())
    }
}

/// Heartbeat staleness check, mirroring `PriceGuardsLib.applyStaleness`.
///
/// Returns `Err(FutureTimestamp)` for source timestamps in the future,
/// `Err(StaleData)` if older than `heartbeat_sec`, `Ok(())` otherwise.
/// `heartbeat_sec == 0` disables the check.
pub fn apply_staleness(env: &Env, updated_at: u64, heartbeat_sec: u32) -> Result<(), OracleError> {
    let now = env.ledger().timestamp();
    if updated_at > now {
        return Err(OracleError::FutureTimestamp);
    }
    if heartbeat_sec == 0 {
        return Ok(());
    }
    if updated_at == 0 {
        return Err(OracleError::StaleData);
    }
    if now - updated_at > heartbeat_sec as u64 {
        return Err(OracleError::StaleData);
    }
    Ok(())
}

/// Bps-band membership check, mirroring `PriceGuardsLib.withinBpsBand`.
///
/// Returns `true` iff `value ∈ [anchor·(1+min/1e4), anchor·(1+max/1e4)]`.
/// `min == 0 && max == 0` returns `true` unconditionally.
pub fn within_bps_band(value: i128, anchor: i128, min_bps: i32, max_bps: i32) -> bool {
    if min_bps == 0 && max_bps == 0 {
        return true;
    }
    let lower = scale_by_bps(anchor, min_bps);
    let upper = scale_by_bps(anchor, max_bps);
    value >= lower && value <= upper
}

fn scale_by_bps(anchor: i128, delta_bps: i32) -> i128 {
    const BPS_DENOMINATOR: i128 = 10_000;
    let adjusted_bps = BPS_DENOMINATOR + delta_bps as i128;
    // Anchor is non-negative in our usage; clamp negative inputs to zero defensively.
    if anchor <= 0 || adjusted_bps <= 0 {
        return 0;
    }
    // Both `anchor * adjusted_bps` and the divide are safe within i128 because
    // `anchor` ≤ ~i128::MAX/2e4 in any realistic use; the intermediate fits.
    anchor.saturating_mul(adjusted_bps) / BPS_DENOMINATOR
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Ledger as _, Env};

    #[test]
    fn guard_validation_rejects_min_above_max() {
        let g = PriceGuard {
            heartbeat_sec: 0,
            clamp_min_bps: 10,
            clamp_max_bps: 5,
        };
        assert_eq!(g.validate(), Err(OracleError::InvalidGuardBand));
    }

    #[test]
    fn guard_validation_rejects_out_of_range() {
        let g = PriceGuard {
            heartbeat_sec: 0,
            clamp_min_bps: -10_001,
            clamp_max_bps: 0,
        };
        assert_eq!(g.validate(), Err(OracleError::GuardBpsOutOfRange));
        let g = PriceGuard {
            heartbeat_sec: 0,
            clamp_min_bps: 0,
            clamp_max_bps: 10_001,
        };
        assert_eq!(g.validate(), Err(OracleError::GuardBpsOutOfRange));
    }

    #[test]
    fn guard_validation_accepts_well_formed() {
        let g = PriceGuard {
            heartbeat_sec: 0,
            clamp_min_bps: -30,
            clamp_max_bps: 30,
        };
        assert!(g.validate().is_ok());
    }

    #[test]
    fn bps_band_no_clamp_passes() {
        assert!(within_bps_band(7, 1, 0, 0));
    }

    #[test]
    fn bps_band_inside() {
        // 0.997 within ±30 bps of 1.0
        assert!(within_bps_band(997 * (WAD / 1000), WAD, -30, 30));
    }

    #[test]
    fn bps_band_just_below_lower() {
        // anchor·(1-30/1e4) = 0.997·WAD exactly; one wei below should fail.
        let lower = WAD - (WAD * 30 / 10_000);
        assert!(!within_bps_band(lower - 1, WAD, -30, 30));
        assert!(within_bps_band(lower, WAD, -30, 30));
    }

    #[test]
    fn staleness_passes_when_disabled() {
        let env = Env::default();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        assert!(apply_staleness(&env, 100, 0).is_ok());
    }

    #[test]
    fn staleness_rejects_future_timestamp() {
        let env = Env::default();
        env.ledger().with_mut(|l| l.timestamp = 1000);
        assert_eq!(
            apply_staleness(&env, 1001, 60),
            Err(OracleError::FutureTimestamp)
        );
    }

    #[test]
    fn staleness_rejects_old_timestamp() {
        let env = Env::default();
        env.ledger().with_mut(|l| l.timestamp = 5000);
        assert_eq!(apply_staleness(&env, 100, 60), Err(OracleError::StaleData));
    }

    #[test]
    fn staleness_accepts_fresh_timestamp() {
        let env = Env::default();
        env.ledger().with_mut(|l| l.timestamp = 5000);
        assert!(apply_staleness(&env, 4990, 60).is_ok());
    }
}
