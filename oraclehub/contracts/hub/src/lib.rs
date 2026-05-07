#![allow(deprecated)] // soroban-sdk 25: events().publish migrating to #[contractevent], TODO
#![no_std]

//! OracleHub — singleton aggregator for the XCCY Soroban deployment.
//!
//! Holds a registry of `OracleId → Address` and dispatches `get_rate` /
//! `get_price` calls to the appropriate adapter contract via
//! `env.invoke_contract`. Standard primitives are delegated to peer-reviewed
//! libraries:
//!
//! - **Ownership**: [`stellar_access::ownable::Ownable`] (2-step transfer
//!   with `live_until_ledger` expiry, `#[only_owner]` macro for admin gates).
//! - **Pause**: [`stellar_contract_utils::pausable`] (`#[when_not_paused]`
//!   macro for read paths, owner-gated `pause`/`unpause` mutators).
//! - **Upgrade**: timelocked 2-step (`propose_upgrade` → `commit_upgrade`)
//!   layered on top of [`stellar_contract_utils::upgradeable::upgrade`] for
//!   the actual binary swap. The 24-hour delay is our value-add; OZ's
//!   `Upgradeable` trait does not specify timelock semantics.

use oraclehub_types::{OracleError, OracleId, OracleKind, PriceData, RateData, SepAsset};
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, vec, Address, BytesN, Env, IntoVal, Symbol,
    Val, Vec,
};
use stellar_access::ownable::{self as ownable, Ownable};
use stellar_contract_utils::pausable;
use stellar_contract_utils::upgradeable;
use stellar_macros::{only_owner, when_not_paused};

const TIMELOCK_SECONDS: u64 = 24 * 60 * 60;

// Persistent storage TTL configuration.
const TTL_BUMP_LEDGERS: u32 = 30 * 17_280;
const TTL_THRESHOLD_LEDGERS: u32 = 7 * 17_280;

// ---------------------------------------------------------------------------
// Storage keys (only timelock state lives here; admin/pause delegated to OZ)
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone)]
pub enum InstanceKey {
    PendingUpgrade,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingUpgrade {
    pub wasm_hash: BytesN<32>,
    pub executable_at: u64,
}

#[contracttype]
#[derive(Clone)]
pub enum PersistentKey {
    Registry(OracleId),
}

// ---------------------------------------------------------------------------
// Events (timelock + registry — pause/admin events are emitted by OZ libs)
// ---------------------------------------------------------------------------

const T_REG: Symbol = symbol_short!("reg");
const T_UNREG: Symbol = symbol_short!("unreg");
const T_UPG_PROP: Symbol = symbol_short!("upg_prop");
const T_UPG_COMMIT: Symbol = symbol_short!("upg_comm");

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct OracleHub;

#[contractimpl]
impl OracleHub {
    pub fn __constructor(env: Env, admin: Address) {
        ownable::set_owner(&env, &admin);
    }

    // ------------- Pause (owner-gated, delegates to OZ pausable) -------------

    #[only_owner]
    pub fn pause(env: Env) {
        pausable::pause(&env);
    }

    #[only_owner]
    pub fn unpause(env: Env) {
        pausable::unpause(&env);
    }

    pub fn is_paused(env: Env) -> bool {
        pausable::paused(&env)
    }

    // ------------- Upgrade with timelock -------------

    #[only_owner]
    pub fn propose_upgrade(env: Env, wasm_hash: BytesN<32>) {
        let executable_at = env.ledger().timestamp() + TIMELOCK_SECONDS;
        env.storage().instance().set(
            &InstanceKey::PendingUpgrade,
            &PendingUpgrade {
                wasm_hash: wasm_hash.clone(),
                executable_at,
            },
        );
        env.events()
            .publish((T_UPG_PROP,), (wasm_hash, executable_at));
    }

    /// Commits a previously proposed upgrade after the 24h timelock has
    /// elapsed. Anyone may call, but the call must not be paused. The actual
    /// binary swap goes through OZ `upgradeable::upgrade` for standardised
    /// event emission and future safety extensions.
    pub fn commit_upgrade(env: Env) -> Result<(), OracleError> {
        if pausable::paused(&env) {
            return Err(OracleError::Paused);
        }
        let pending: PendingUpgrade = env
            .storage()
            .instance()
            .get(&InstanceKey::PendingUpgrade)
            .ok_or(OracleError::TimelockNotElapsed)?;
        if env.ledger().timestamp() < pending.executable_at {
            return Err(OracleError::TimelockNotElapsed);
        }
        upgradeable::upgrade(&env, &pending.wasm_hash);
        env.storage()
            .instance()
            .remove(&InstanceKey::PendingUpgrade);
        env.events().publish((T_UPG_COMMIT,), pending.wasm_hash);
        Ok(())
    }

    pub fn pending_upgrade(env: Env) -> Option<PendingUpgrade> {
        env.storage().instance().get(&InstanceKey::PendingUpgrade)
    }

    #[only_owner]
    pub fn cancel_upgrade(env: Env) {
        env.storage()
            .instance()
            .remove(&InstanceKey::PendingUpgrade);
    }

    // ------------- Registry -------------

    #[only_owner]
    pub fn register_oracle(env: Env, id: OracleId, adapter: Address) {
        env.storage()
            .persistent()
            .set(&PersistentKey::Registry(id.clone()), &adapter);
        env.storage().persistent().extend_ttl(
            &PersistentKey::Registry(id.clone()),
            TTL_THRESHOLD_LEDGERS,
            TTL_BUMP_LEDGERS,
        );
        env.events().publish((T_REG,), (id, adapter));
    }

    #[only_owner]
    pub fn unregister_oracle(env: Env, id: OracleId) {
        env.storage()
            .persistent()
            .remove(&PersistentKey::Registry(id.clone()));
        env.events().publish((T_UNREG,), id);
    }

    pub fn lookup(env: Env, id: OracleId) -> Option<Address> {
        env.storage().persistent().get(&PersistentKey::Registry(id))
    }

    // ------------- Reads (paused gate via OZ macro) -------------

    #[when_not_paused]
    pub fn get_rate(env: Env, id: OracleId, key: Address) -> Result<RateData, OracleError> {
        if matches!(id.kind, OracleKind::ReflectorPrice) {
            return Err(OracleError::InvalidArgument);
        }
        let adapter = lookup_with_ttl(&env, &id)?;
        let fn_name = Symbol::new(&env, "peek_rate");
        let args: Vec<Val> = vec![&env, key.into_val(&env)];
        let result: RateData = env.invoke_contract(&adapter, &fn_name, args);
        Ok(result)
    }

    /// Read a SEP-40 price for `asset`. The asset is the SEP-40 standard type
    /// (`SepAsset::Stellar(addr)` for Stellar-native assets, `SepAsset::Other(symbol)`
    /// for external-symbol-keyed feeds like the Reflector External CEX/DEX oracle).
    #[when_not_paused]
    pub fn get_price(env: Env, id: OracleId, asset: SepAsset) -> Result<PriceData, OracleError> {
        if !matches!(id.kind, OracleKind::ReflectorPrice) {
            return Err(OracleError::InvalidArgument);
        }
        let adapter = lookup_with_ttl(&env, &id)?;
        let fn_name = Symbol::new(&env, "peek_price");
        let args: Vec<Val> = vec![&env, asset.into_val(&env)];
        let result: PriceData = env.invoke_contract(&adapter, &fn_name, args);
        Ok(result)
    }
}

// Auto-derives `get_owner`, `transfer_ownership`, `accept_ownership`,
// `renounce_ownership` (all 2-step semantics with `live_until_ledger`).
#[contractimpl(contracttrait)]
impl Ownable for OracleHub {}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn lookup_with_ttl(env: &Env, id: &OracleId) -> Result<Address, OracleError> {
    let key = PersistentKey::Registry(id.clone());
    let addr: Address = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(OracleError::OracleNotRegistered)?;
    env.storage()
        .persistent()
        .extend_ttl(&key, TTL_THRESHOLD_LEDGERS, TTL_BUMP_LEDGERS);
    Ok(addr)
}

#[cfg(test)]
mod test;
