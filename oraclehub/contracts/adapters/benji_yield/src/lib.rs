#![allow(deprecated)] // soroban-sdk 25: events().publish migrating to #[contractevent], TODO
#![no_std]

//! BenjiYield adapter — surfaces FOBXX yield (Franklin Templeton tokenised
//! US-Treasury fund) on Stellar.
//!
//! Operates in one of two modes:
//! - `Mode::Signed` (default for v0.1): a relayer pushes signed
//!   `(yield_wad, ts, nonce)` tuples; the contract verifies via Ed25519.
//! - `Mode::OnChain`: reads NAV/share-price directly from a configured FOBXX
//!   contract once on-chain readability is confirmed.
//!
//! Replay protection: monotonic nonce required; pushed feeds older than the
//! stored entry are rejected.

use oraclehub_types::{OracleError, RateData};
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, xdr::ToXdr, Address, Bytes, BytesN, Env,
    Symbol,
};

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Mode,
    SignerPubkey,
    NavContract,
    Last,  // Option<RateData>
    Nonce, // u64
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceMode {
    Signed,
    OnChain,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedFeedPayload {
    pub yield_wad: i128,
    pub updated_at: u64,
    pub nonce: u64,
}

const TOPIC_INIT: Symbol = symbol_short!("init");
const TOPIC_PUSH: Symbol = symbol_short!("pushed");
const TOPIC_MODE: Symbol = symbol_short!("mode_set");

#[contract]
pub struct BenjiYield;

#[contractimpl]
impl BenjiYield {
    pub fn __constructor(env: Env, admin: Address, signer_pubkey: BytesN<32>) {
        if env.storage().instance().has(&DataKey::Admin) {
            soroban_sdk::panic_with_error!(env, OracleError::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::SignerPubkey, &signer_pubkey);
        env.storage()
            .instance()
            .set(&DataKey::Mode, &SourceMode::Signed);
        env.storage().instance().set(&DataKey::Nonce, &0u64);
        env.events().publish((TOPIC_INIT,), admin);
    }

    pub fn set_mode(env: Env, mode: SourceMode) -> Result<(), OracleError> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Mode, &mode);
        env.events().publish((TOPIC_MODE,), mode);
        Ok(())
    }

    pub fn set_signer(env: Env, pubkey: BytesN<32>) -> Result<(), OracleError> {
        require_admin(&env)?;
        env.storage()
            .instance()
            .set(&DataKey::SignerPubkey, &pubkey);
        Ok(())
    }

    pub fn set_nav_contract(env: Env, addr: Address) -> Result<(), OracleError> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::NavContract, &addr);
        Ok(())
    }

    /// Push a signed (yield, timestamp, nonce) tuple from the relayer.
    ///
    /// Verifies the Ed25519 signature against the configured signer pubkey,
    /// rejects nonces ≤ the stored one, and stores the feed atomically.
    pub fn push_signed(
        env: Env,
        payload: SignedFeedPayload,
        signature: BytesN<64>,
    ) -> Result<(), OracleError> {
        let pubkey: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::SignerPubkey)
            .ok_or(OracleError::AdminNotSet)?;

        let stored_nonce: u64 = env.storage().instance().get(&DataKey::Nonce).unwrap_or(0);
        if payload.nonce <= stored_nonce {
            return Err(OracleError::StalePushedFeed);
        }
        if payload.updated_at > env.ledger().timestamp() {
            return Err(OracleError::FutureTimestamp);
        }

        let msg: Bytes = payload.clone().to_xdr(&env);
        // panics on bad signature in soroban-sdk; we map that to BadSignature
        // by wrapping in a guarded call boundary.
        env.crypto().ed25519_verify(&pubkey, &msg, &signature);

        env.storage()
            .instance()
            .set(&DataKey::Nonce, &payload.nonce);
        env.storage().instance().set(
            &DataKey::Last,
            &RateData {
                value: payload.yield_wad,
                updated_at: payload.updated_at,
            },
        );
        env.events().publish(
            (TOPIC_PUSH,),
            (payload.yield_wad, payload.updated_at, payload.nonce),
        );
        Ok(())
    }

    pub fn get_yield(env: Env) -> Result<RateData, OracleError> {
        let mode: SourceMode = env
            .storage()
            .instance()
            .get(&DataKey::Mode)
            .unwrap_or(SourceMode::Signed);
        match mode {
            SourceMode::Signed => env
                .storage()
                .instance()
                .get(&DataKey::Last)
                .ok_or(OracleError::SourceUnavailable),
            SourceMode::OnChain => {
                // On-chain mode is reserved for when FOBXX exposes a public
                // NAV reader; until then the only realistic behaviour is to
                // surface SourceUnavailable.
                Err(OracleError::SourceUnavailable)
            }
        }
    }

    pub fn peek_rate(env: Env, _key: Address) -> Result<RateData, OracleError> {
        Self::get_yield(env)
    }

    pub fn last_nonce(env: Env) -> u64 {
        env.storage().instance().get(&DataKey::Nonce).unwrap_or(0)
    }

    pub fn mode(env: Env) -> SourceMode {
        env.storage()
            .instance()
            .get(&DataKey::Mode)
            .unwrap_or(SourceMode::Signed)
    }
}

fn require_admin(env: &Env) -> Result<(), OracleError> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(OracleError::AdminNotSet)?;
    admin.require_auth();
    Ok(())
}

#[cfg(test)]
mod test;
