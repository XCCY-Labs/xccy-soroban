#![no_std]

//! OracleHub — singleton aggregator for the XCCY Soroban deployment.
//!
//! Holds a registry of `OracleId → Address` and dispatches `get_rate` /
//! `get_price` calls to the appropriate adapter contract via
//! `env.invoke_contract`. Implements:
//!
//! - Two-step admin transfer (`propose_admin` → `accept_admin`).
//! - 24h-timelocked Soroban-native upgrades.
//! - Emergency `pause` / `unpause` that gates all reads.
//! - Persistent-storage TTL extension on every successful read.

use oraclehub_types::{OracleError, OracleId, OracleKind, PriceData, RateData, SepAsset};
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, vec, Address, BytesN, Env, IntoVal, Symbol,
    Val, Vec,
};

const TIMELOCK_SECONDS: u64 = 24 * 60 * 60;

// Persistent storage TTL configuration — values are deliberate placeholders;
// production deployment should tune to the actual ledger close cadence.
const TTL_BUMP_LEDGERS: u32 = 30 * 17_280;
const TTL_THRESHOLD_LEDGERS: u32 = 7 * 17_280;

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone)]
pub enum InstanceKey {
    Admin,
    PendingAdmin,
    Paused,
    PendingUpgrade, // (BytesN<32> wasm_hash, u64 executable_at)
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
// Events
// ---------------------------------------------------------------------------

const T_INIT: Symbol = symbol_short!("init");
const T_ADM_PROP: Symbol = symbol_short!("adm_prop");
const T_ADM_ACC: Symbol = symbol_short!("adm_acc");
const T_REG: Symbol = symbol_short!("reg");
const T_UNREG: Symbol = symbol_short!("unreg");
const T_UPG_PROP: Symbol = symbol_short!("upg_prop");
const T_UPG_COMMIT: Symbol = symbol_short!("upg_comm");
const T_PAUSED: Symbol = symbol_short!("paused");
const T_UNPAUSED: Symbol = symbol_short!("unpaused");

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct OracleHub;

#[contractimpl]
impl OracleHub {
    pub fn __constructor(env: Env, admin: Address) {
        if env.storage().instance().has(&InstanceKey::Admin) {
            soroban_sdk::panic_with_error!(env, OracleError::AlreadyInitialized);
        }
        env.storage().instance().set(&InstanceKey::Admin, &admin);
        env.storage().instance().set(&InstanceKey::Paused, &false);
        env.events().publish((T_INIT,), admin);
    }

    // ------------- Admin: two-step transfer -------------

    pub fn propose_admin(env: Env, new_admin: Address) -> Result<(), OracleError> {
        require_admin(&env)?;
        env.storage()
            .instance()
            .set(&InstanceKey::PendingAdmin, &new_admin);
        env.events().publish((T_ADM_PROP,), new_admin);
        Ok(())
    }

    pub fn accept_admin(env: Env) -> Result<(), OracleError> {
        let pending: Address = env
            .storage()
            .instance()
            .get(&InstanceKey::PendingAdmin)
            .ok_or(OracleError::Unauthorized)?;
        pending.require_auth();
        env.storage().instance().set(&InstanceKey::Admin, &pending);
        env.storage().instance().remove(&InstanceKey::PendingAdmin);
        env.events().publish((T_ADM_ACC,), pending);
        Ok(())
    }

    pub fn admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&InstanceKey::Admin)
    }

    pub fn pending_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&InstanceKey::PendingAdmin)
    }

    // ------------- Pause -------------

    pub fn pause(env: Env) -> Result<(), OracleError> {
        require_admin(&env)?;
        env.storage().instance().set(&InstanceKey::Paused, &true);
        env.events().publish((T_PAUSED,), ());
        Ok(())
    }

    pub fn unpause(env: Env) -> Result<(), OracleError> {
        require_admin(&env)?;
        env.storage().instance().set(&InstanceKey::Paused, &false);
        env.events().publish((T_UNPAUSED,), ());
        Ok(())
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&InstanceKey::Paused)
            .unwrap_or(false)
    }

    // ------------- Upgrade with timelock -------------

    pub fn propose_upgrade(env: Env, wasm_hash: BytesN<32>) -> Result<(), OracleError> {
        require_admin(&env)?;
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
        Ok(())
    }

    /// Commits a previously proposed upgrade. Anyone may call after the
    /// timelock has elapsed, but the call must not be paused.
    pub fn commit_upgrade(env: Env) -> Result<(), OracleError> {
        if Self::is_paused(env.clone()) {
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
        env.deployer()
            .update_current_contract_wasm(pending.wasm_hash.clone());
        env.storage()
            .instance()
            .remove(&InstanceKey::PendingUpgrade);
        env.events().publish((T_UPG_COMMIT,), pending.wasm_hash);
        Ok(())
    }

    pub fn pending_upgrade(env: Env) -> Option<PendingUpgrade> {
        env.storage().instance().get(&InstanceKey::PendingUpgrade)
    }

    pub fn cancel_upgrade(env: Env) -> Result<(), OracleError> {
        require_admin(&env)?;
        env.storage()
            .instance()
            .remove(&InstanceKey::PendingUpgrade);
        Ok(())
    }

    // ------------- Registry -------------

    pub fn register_oracle(env: Env, id: OracleId, adapter: Address) -> Result<(), OracleError> {
        require_admin(&env)?;
        env.storage()
            .persistent()
            .set(&PersistentKey::Registry(id.clone()), &adapter);
        env.storage().persistent().extend_ttl(
            &PersistentKey::Registry(id.clone()),
            TTL_THRESHOLD_LEDGERS,
            TTL_BUMP_LEDGERS,
        );
        env.events().publish((T_REG,), (id, adapter));
        Ok(())
    }

    pub fn unregister_oracle(env: Env, id: OracleId) -> Result<(), OracleError> {
        require_admin(&env)?;
        env.storage()
            .persistent()
            .remove(&PersistentKey::Registry(id.clone()));
        env.events().publish((T_UNREG,), id);
        Ok(())
    }

    pub fn lookup(env: Env, id: OracleId) -> Option<Address> {
        env.storage().persistent().get(&PersistentKey::Registry(id))
    }

    // ------------- Reads -------------

    pub fn get_rate(env: Env, id: OracleId, key: Address) -> Result<RateData, OracleError> {
        require_unpaused(&env)?;
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
    pub fn get_price(env: Env, id: OracleId, asset: SepAsset) -> Result<PriceData, OracleError> {
        require_unpaused(&env)?;
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn require_admin(env: &Env) -> Result<(), OracleError> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&InstanceKey::Admin)
        .ok_or(OracleError::AdminNotSet)?;
    admin.require_auth();
    Ok(())
}

fn require_unpaused(env: &Env) -> Result<(), OracleError> {
    let paused: bool = env
        .storage()
        .instance()
        .get(&InstanceKey::Paused)
        .unwrap_or(false);
    if paused {
        Err(OracleError::Paused)
    } else {
        Ok(())
    }
}

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
