extern crate std;

use crate::{OracleHub, OracleHubClient, PendingUpgrade};
use oraclehub_types::{OracleError, OracleId, OracleKind, SepAsset, WAD};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, BytesN as _, Ledger as _},
    Address, BytesN, Env, Symbol,
};

use oraclehub_custom_apr::{CustomApr, CustomAprClient};
use oraclehub_reflector_price::{ReflectorPrice, ReflectorPriceClient};

const KEY_PYUSD: Symbol = symbol_short!("PYUSD");
const KEY_BLEND_S: Symbol = symbol_short!("blendS");

// ---------------------------------------------------------------------------
// Inline mock contracts for cross-crate hub testing.
//
// Each mock is wrapped in its own module because `#[contractimpl]` on a type
// with `__constructor` emits module-scope helpers (`____constructor`,
// `__SPEC_XDR_FN___CONSTRUCTOR`) that would otherwise collide if two
// constructor-bearing contracts lived in the same module.
// ---------------------------------------------------------------------------

mod mock_sep40 {
    use oraclehub_types::{PriceData, SepAsset};
    use soroban_sdk::{contract, contractimpl, Env};

    #[contract]
    pub struct MockSep40;

    #[contractimpl]
    impl MockSep40 {
        pub fn __constructor(env: Env, price: i128, timestamp: u64, decimals: u32) {
            env.storage().instance().set(&"price", &price);
            env.storage().instance().set(&"ts", &timestamp);
            env.storage().instance().set(&"dec", &decimals);
        }
        pub fn lastprice(env: Env, _asset: SepAsset) -> Option<PriceData> {
            let price: i128 = env.storage().instance().get(&"price")?;
            let ts: u64 = env.storage().instance().get(&"ts").unwrap_or(0);
            if price == 0 && ts == 0 {
                return None;
            }
            Some(PriceData {
                price,
                timestamp: ts,
            })
        }
        pub fn decimals(env: Env) -> u32 {
            env.storage().instance().get(&"dec").unwrap_or(14)
        }
    }
}

use mock_sep40::MockSep40;

// ---------------------------------------------------------------------------

fn deploy_hub<'a>(env: &'a Env, admin: &Address) -> OracleHubClient<'a> {
    let id = env.register(OracleHub, (admin,));
    OracleHubClient::new(env, &id)
}

fn rid(kind: OracleKind, key: Symbol) -> OracleId {
    OracleId { kind, key }
}

#[test]
fn constructor_sets_owner() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let hub = deploy_hub(&env, &admin);
    // Auto-derived from `impl Ownable for OracleHub` — OZ standard accessor.
    assert_eq!(hub.get_owner(), Some(admin));
}

#[test]
fn ownership_two_step_transfer_via_oz_ownable() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.sequence_number = 100);
    let admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let hub = deploy_hub(&env, &admin);

    // OZ Ownable's `transfer_ownership` requires a `live_until_ledger` deadline
    // for the pending transfer. 1000 ledgers is comfortably past current_seq=100.
    hub.transfer_ownership(&new_admin, &1000);

    // The new owner must explicitly accept — same 2-step shape as our
    // hand-rolled propose/accept, just standardised through OZ.
    hub.accept_ownership();

    assert_eq!(hub.get_owner(), Some(new_admin));
}

#[test]
fn pause_blocks_reads() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let hub = deploy_hub(&env, &admin);

    hub.pause();
    assert!(hub.is_paused());

    // The `#[when_not_paused]` macro from `stellar-macros` panics with
    // `PausableError::EnforcedPause` (a non-OracleError variant) — assert the
    // call fails rather than matching the exact error code, which is internal
    // to the OZ pausable module.
    let key = Address::generate(&env);
    let id = rid(OracleKind::CustomApr, KEY_PYUSD);
    assert!(hub.try_get_rate(&id, &key).is_err());
}

#[test]
fn unpause_restores_reads() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    let admin = Address::generate(&env);
    let hub = deploy_hub(&env, &admin);

    hub.pause();
    hub.unpause();

    let custom = env.register(CustomApr, (&admin, 0u32));
    let custom_client = CustomAprClient::new(&env, &custom);
    custom_client.set_apr(&50_000_000_000_000_000_i128, &200);
    env.ledger().with_mut(|l| l.timestamp = 250);
    custom_client.promote();

    let id = rid(OracleKind::CustomApr, KEY_PYUSD);
    hub.register_oracle(&id, &custom);

    let placeholder_key = Address::generate(&env);
    let r = hub.get_rate(&id, &placeholder_key);
    assert_eq!(r.value, 50_000_000_000_000_000);
}

#[test]
fn unregister_blocks_subsequent_reads() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    let admin = Address::generate(&env);
    let hub = deploy_hub(&env, &admin);

    let custom = env.register(CustomApr, (&admin, 0u32));
    let id = rid(OracleKind::CustomApr, KEY_PYUSD);
    hub.register_oracle(&id, &custom);
    hub.unregister_oracle(&id);

    let key = Address::generate(&env);
    let err = hub.try_get_rate(&id, &key).err().unwrap().unwrap();
    assert_eq!(err, OracleError::OracleNotRegistered);
}

#[test]
fn upgrade_timelock_rejects_early_commit() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    let admin = Address::generate(&env);
    let hub = deploy_hub(&env, &admin);

    let wasm_hash = BytesN::<32>::random(&env);
    hub.propose_upgrade(&wasm_hash);

    let err = hub.try_commit_upgrade().err().unwrap().unwrap();
    assert_eq!(err, OracleError::TimelockNotElapsed);

    env.ledger()
        .with_mut(|l| l.timestamp = 100 + 24 * 60 * 60 - 1);
    let err = hub.try_commit_upgrade().err().unwrap().unwrap();
    assert_eq!(err, OracleError::TimelockNotElapsed);
}

#[test]
fn upgrade_pending_clears_after_cancel() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    let admin = Address::generate(&env);
    let hub = deploy_hub(&env, &admin);

    let wasm_hash = BytesN::<32>::random(&env);
    hub.propose_upgrade(&wasm_hash);
    assert!(hub.pending_upgrade().is_some());

    hub.cancel_upgrade();
    assert!(hub.pending_upgrade().is_none());
}

#[test]
fn upgrade_pending_returns_correct_executable_at() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);
    let admin = Address::generate(&env);
    let hub = deploy_hub(&env, &admin);
    let wasm_hash = BytesN::<32>::random(&env);
    hub.propose_upgrade(&wasm_hash);
    let p: PendingUpgrade = hub.pending_upgrade().unwrap();
    assert_eq!(p.executable_at, 1000 + 24 * 60 * 60);
    assert_eq!(p.wasm_hash, wasm_hash);
}

#[test]
fn pause_blocks_upgrade_commit_even_after_timelock() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    let admin = Address::generate(&env);
    let hub = deploy_hub(&env, &admin);
    let wasm_hash = BytesN::<32>::random(&env);
    hub.propose_upgrade(&wasm_hash);
    hub.pause();
    env.ledger()
        .with_mut(|l| l.timestamp = 100 + 24 * 60 * 60 + 1);
    let err = hub.try_commit_upgrade().err().unwrap().unwrap();
    assert_eq!(err, OracleError::Paused);
}

#[test]
fn get_rate_kind_mismatch_rejects_reflector_price_id() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let hub = deploy_hub(&env, &admin);
    let id = rid(OracleKind::ReflectorPrice, KEY_PYUSD);
    let key = Address::generate(&env);
    let err = hub.try_get_rate(&id, &key).err().unwrap().unwrap();
    assert_eq!(err, OracleError::InvalidArgument);
}

#[test]
fn get_price_kind_mismatch_rejects_rate_id() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let hub = deploy_hub(&env, &admin);
    let id = rid(OracleKind::BlendRate, KEY_BLEND_S);
    let asset = SepAsset::Stellar(Address::generate(&env));
    let err = hub.try_get_price(&id, &asset).err().unwrap().unwrap();
    assert_eq!(err, OracleError::InvalidArgument);
}

// Note: blend_rate cross-contract dispatch is exercised end-to-end in
// `oraclehub-blend-rate`'s own integration tests. Here we keep the hub-side
// dispatch coverage via `unpause_restores_reads` (which dispatches to a
// custom_apr adapter). Re-introducing a blend-shaped MockPool here would
// require pulling in `blend-contract-sdk` as a hub dev-dep, which we avoid
// for compile-time hygiene.

#[test]
fn full_dispatch_reflector_price_through_hub() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 100);
    let admin = Address::generate(&env);
    let hub = deploy_hub(&env, &admin);

    let feed = env.register(MockSep40, (100_000_000_000_000_i128, 95_u64, 14_u32));
    let reflector = env.register(ReflectorPrice, (&admin,));
    let reflector_client = ReflectorPriceClient::new(&env, &reflector);
    reflector_client.set_feed(&feed);

    let id = rid(OracleKind::ReflectorPrice, KEY_PYUSD);
    hub.register_oracle(&id, &reflector);

    // Query as a Stellar-native asset.
    let asset = SepAsset::Stellar(Address::generate(&env));
    let p = hub.get_price(&id, &asset);
    assert_eq!(p.price, WAD);
    assert_eq!(p.timestamp, 95);
}
