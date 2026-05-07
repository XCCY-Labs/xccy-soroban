#![allow(deprecated)] // soroban-sdk 25: events().publish migrating to #[contractevent], TODO
#![no_std]

//! UsdeRate adapter — surfaces sUSDe ↔ USDe exchange rate (Ethena yield).
//!
//! Operates in one of two modes:
//! - `Mode::OnChain`: reads `convert_to_assets(WAD)` from a configured sUSDe
//!   vault contract (ERC-4626 semantics preserved by Allbridge wrapper or
//!   Ethena's native Stellar OFT, depending on which lands first).
//! - `Mode::Signed` (default for v0.1): a relayer pushes signed
//!   `(rate_wad, ts, nonce)` tuples; the contract verifies via Ed25519.
//!
//! `rate_wad` is the **growth above 1.0**, e.g. 5e16 means sUSDe is 5% above
//! USDe in value. The hub interprets this as the yield earned over the
//! observation window.

use oraclehub_types::{OracleError, RateData, WAD};
use soroban_sdk::{
    contract, contractclient, contractimpl, contracttype, symbol_short, xdr::ToXdr, Address, Bytes,
    BytesN, Env, Symbol,
};
use stellar_access::ownable::{self as ownable, Ownable};
use stellar_macros::only_owner;

#[contractclient(name = "Erc4626Client")]
pub trait Erc4626Vault {
    /// Returns assets equivalent to the given share amount (WAD shares in,
    /// asset units out). For sUSDe-native this is the canonical exchange
    /// reading.
    fn convert_to_assets(env: Env, shares_wad: i128) -> i128;
    fn last_update(env: Env) -> u64;
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Mode,
    SignerPubkey,
    Vault,
    Last,
    Nonce,
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
    pub rate_wad: i128,
    pub updated_at: u64,
    pub nonce: u64,
}

const TOPIC_PUSH: Symbol = symbol_short!("pushed");

#[contract]
pub struct UsdeRate;

#[contractimpl]
impl UsdeRate {
    pub fn __constructor(env: Env, admin: Address, signer_pubkey: BytesN<32>) {
        ownable::set_owner(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::SignerPubkey, &signer_pubkey);
        env.storage()
            .instance()
            .set(&DataKey::Mode, &SourceMode::Signed);
        env.storage().instance().set(&DataKey::Nonce, &0u64);
    }

    #[only_owner]
    pub fn set_mode(env: Env, mode: SourceMode) {
        env.storage().instance().set(&DataKey::Mode, &mode);
    }

    #[only_owner]
    pub fn set_signer(env: Env, pubkey: BytesN<32>) {
        env.storage()
            .instance()
            .set(&DataKey::SignerPubkey, &pubkey);
    }

    #[only_owner]
    pub fn set_vault(env: Env, vault: Address) {
        env.storage().instance().set(&DataKey::Vault, &vault);
    }

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
        env.crypto().ed25519_verify(&pubkey, &msg, &signature);
        env.storage()
            .instance()
            .set(&DataKey::Nonce, &payload.nonce);
        env.storage().instance().set(
            &DataKey::Last,
            &RateData {
                value: payload.rate_wad,
                updated_at: payload.updated_at,
            },
        );
        env.events().publish(
            (TOPIC_PUSH,),
            (payload.rate_wad, payload.updated_at, payload.nonce),
        );
        Ok(())
    }

    pub fn get_rate(env: Env) -> Result<RateData, OracleError> {
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
                let vault: Address = env
                    .storage()
                    .instance()
                    .get(&DataKey::Vault)
                    .ok_or(OracleError::SourceUnavailable)?;
                let client = Erc4626Client::new(&env, &vault);
                let assets_per_share = client.convert_to_assets(&WAD);
                let updated_at = client.last_update();
                if assets_per_share <= 0 {
                    return Err(OracleError::SourceUnavailable);
                }
                // Growth above WAD = (assets_per_share - WAD); guard against negatives.
                let growth = if assets_per_share >= WAD {
                    assets_per_share - WAD
                } else {
                    0
                };
                Ok(RateData {
                    value: growth,
                    updated_at,
                })
            }
        }
    }

    pub fn peek_rate(env: Env, _key: Address) -> Result<RateData, OracleError> {
        Self::get_rate(env)
    }

    pub fn mode(env: Env) -> SourceMode {
        env.storage()
            .instance()
            .get(&DataKey::Mode)
            .unwrap_or(SourceMode::Signed)
    }

    pub fn last_nonce(env: Env) -> u64 {
        env.storage().instance().get(&DataKey::Nonce).unwrap_or(0)
    }
}

#[contractimpl(contracttrait)]
impl Ownable for UsdeRate {}

#[cfg(test)]
mod test;
