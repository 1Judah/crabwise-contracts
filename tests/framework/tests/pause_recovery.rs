//! Pause and recovery integration coverage for the escrow contract (#707).
//!
//! Escrow is used here because it keeps its **own** pause key
//! (`PauseKey::{Paused, Admin}`) rather than delegating to `shared::pause`, and
//! because it returns a *typed* error — `EscrowError::ContractPaused` (14) —
//! instead of the shared module's bare `panic!("contract is paused")`. A typed
//! error is what lets this file assert *which* guard fired, which is the whole
//! point of the procedure written up in `docs/EMERGENCY_PROCEDURES.md`.
//!
//! What these tests cover:
//!   1. a guarded operation is blocked while paused, and leaves no partial state;
//!   2. the same operation succeeds again after `unpause`;
//!   3. a non-admin address can neither pause nor unpause, and a rejected call
//!      has no side effect;
//!   4. `("esc", "paused")` and `("esc", "unpaused")` are emitted, and only by
//!      the action that is supposed to emit them;
//!   5. delayed two-step admin handover: a proposed admin cannot exercise
//!      admin authority before accepting, non-recipients cannot accept, and
//!      acceptance transfers active admin authority to the successor.
//!
//! What they deliberately do NOT cover, and why:
//!
//!   * The time-locked recovery described in `docs/EMERGENCY_PROCEDURES.md` §5.
//!     It does not exist yet — it is issue #711, implemented in a companion PR
//!     that owns `contracts/shared/src/pause.rs`. Writing a test for it now
//!     would be inventing an API.
//!
//!   * Signature-level authorisation. `Environment::new()` calls
//!     `mock_all_auths()`, which satisfies every `require_auth`. The non-admin
//!     test therefore proves the **stored-admin comparison** in
//!     `EscrowContract::pause`, not that a forged signature is rejected. The
//!     signature check is the host's job and needs a real signer to test.
//!
//!   * The `shared::pause` module driven through a consuming contract. Nothing
//!     here exercises `campaign`, `donation`, `withdrawal` or
//!     `revenue_sharing`.

use escrow::errors::EscrowError;
use escrow::storage::CommissionStatus;
use integration_framework::assertions as a;
use integration_framework::{Environment, World};
use soroban_sdk::testutils::Events as _;
use soroban_sdk::{symbol_short, Bytes};

const AMOUNT: i128 = 10_000;
const FEE_BPS: u32 = 500;

#[test]
fn paused_escrow_blocks_create_escrow_and_leaves_no_state() {
    let env = Environment::new();
    let admin = env.address();
    let platform_wallet = env.address();
    let client = env.address();
    let artist = env.address();
    env.mint(&client, 1_000_000);

    let world = World::deploy(&env, &admin, &platform_wallet, FEE_BPS);
    // Escrow's pause key is only usable once an admin has been stored.
    world.escrow.initialize(&admin);
    assert!(!world.escrow.is_paused());

    world.escrow.pause(&admin);
    assert!(world.escrow.is_paused(), "pause did not take effect");

    let cid = Bytes::from_slice(&env.env, b"pause-blocked");
    let result = world.escrow.try_create_escrow(
        &cid,
        &client,
        &artist,
        &AMOUNT,
        &world.config_stub_addr,
    );
    assert_eq!(
        result,
        Err(Ok(EscrowError::ContractPaused)),
        "create_escrow must be refused with the typed paused error",
    );

    // A refused call must not have created a half-written record. `get_escrow`
    // unwraps internally, so probe it through the fallible client instead of
    // asserting on a value type that does not derive PartialEq.
    assert!(
        world.escrow.try_get_escrow(&cid).is_err(),
        "a refused create_escrow must not leave a record behind",
    );

    // And no funds may have moved: the client keeps its whole balance.
    assert_eq!(env.balance(&client), 1_000_000);
}

#[test]
fn create_escrow_is_permitted_again_after_unpause() {
    let env = Environment::new();
    let admin = env.address();
    let platform_wallet = env.address();
    let client = env.address();
    let artist = env.address();
    env.mint(&client, 1_000_000);

    let world = World::deploy(&env, &admin, &platform_wallet, FEE_BPS);
    world.escrow.initialize(&admin);

    // A record created before the pause must survive it untouched.
    let before = Bytes::from_slice(&env.env, b"before-pause");
    world.fund_escrow(&before, &client, &artist, AMOUNT);
    a::assert_escrow_status(&env, &world.escrow, &before, CommissionStatus::Locked);

    world.escrow.pause(&admin);
    let during = Bytes::from_slice(&env.env, b"during-pause");
    assert_eq!(
        world
            .escrow
            .try_create_escrow(&during, &client, &artist, &AMOUNT, &world.config_stub_addr),
        Err(Ok(EscrowError::ContractPaused)),
    );

    // Resume. `unpause` is immediate — there is no delay to wait out today.
    world.escrow.unpause(&admin);
    assert!(!world.escrow.is_paused(), "unpause did not take effect");

    world
        .escrow
        .create_escrow(&during, &client, &artist, &AMOUNT, &world.config_stub_addr);
    a::assert_escrow_status(&env, &world.escrow, &during, CommissionStatus::Locked);

    // The pre-pause record is unchanged: a pause must not alter state.
    a::assert_escrow_status(&env, &world.escrow, &before, CommissionStatus::Locked);
    let record = world.escrow.get_escrow(&before);
    assert_eq!(record.amount, AMOUNT);
    assert_eq!(record.artist, artist);

    // Both escrows funded, so the client is out two full amounts.
    assert_eq!(env.balance(&client), 1_000_000 - 2 * AMOUNT);
}

#[test]
fn a_non_admin_can_neither_pause_nor_unpause() {
    let env = Environment::new();
    let admin = env.address();
    let platform_wallet = env.address();
    let intruder = env.address();

    let world = World::deploy(&env, &admin, &platform_wallet, FEE_BPS);
    world.escrow.initialize(&admin);

    // Pause attempt by a non-admin: refused, and the flag is untouched.
    assert_eq!(
        world.escrow.try_pause(&intruder),
        Err(Ok(EscrowError::Unauthorized)),
    );
    assert!(!world.escrow.is_paused(), "a refused pause must not set the flag");

    // The admin can.
    world.escrow.pause(&admin);
    assert!(world.escrow.is_paused());

    // A non-admin cannot undo it either, and the refusal has no side effect.
    assert_eq!(
        world.escrow.try_unpause(&intruder),
        Err(Ok(EscrowError::Unauthorized)),
    );
    assert!(
        world.escrow.is_paused(),
        "a refused unpause must not clear the flag",
    );

    // A second `initialize` cannot be used to seize the pause admin: it is
    // rejected while the key already exists.
    assert_eq!(
        world.escrow.try_initialize(&intruder),
        Err(Ok(EscrowError::AlreadyExists)),
    );
    assert!(world.escrow.is_paused(), "re-initialise must not disturb the flag");
}

#[test]
fn pause_and_unpause_each_emit_their_own_event() {
    let env = Environment::new();
    let admin = env.address();
    let platform_wallet = env.address();

    let world = World::deploy(&env, &admin, &platform_wallet, FEE_BPS);
    world.escrow.initialize(&admin);

    // `EscrowContract::pause` performs one instance write and publishes exactly
    // one event, so the delta is 1 — not 0 (event missing) and not 2 (a second,
    // unaccounted emission). Combined with the topic check below this pins both
    // the presence and the count.
    let before_pause = env.env.events().all().len();
    world.escrow.pause(&admin);
    let after_pause = env.env.events().all().len();
    assert_eq!(
        after_pause - before_pause,
        1,
        "pause must emit exactly one event",
    );
    a::assert_event_emitted(
        &env,
        &world.escrow_addr,
        &[symbol_short!("esc"), symbol_short!("paused")],
    );

    let before_unpause = env.env.events().all().len();
    world.escrow.unpause(&admin);
    let after_unpause = env.env.events().all().len();
    assert_eq!(
        after_unpause - before_unpause,
        1,
        "unpause must emit exactly one event",
    );
    a::assert_event_emitted(
        &env,
        &world.escrow_addr,
        &[symbol_short!("esc"), symbol_short!("unpaused")],
    );
}

#[test]
fn delayed_admin_acceptance_withholds_authority_until_accept() {
    let env = Environment::new();
    let admin = env.address();
    let successor = env.address();
    let non_recipient = env.address();
    let platform_wallet = env.address();
    let client = env.address();
    let artist = env.address();
    env.mint(&client, 1_000_000);

    let world = World::deploy(&env, &admin, &platform_wallet, FEE_BPS);
    world.escrow.initialize(&admin);

    // Step 1: active admin proposes a successor.
    world.escrow.transfer_admin(&admin, &successor);
    assert_eq!(
        world.escrow.get_pending_admin(),
        Some(successor.clone()),
        "proposed successor must be recorded as pending",
    );
    a::assert_event_emitted(
        &env,
        &world.escrow_addr,
        &[symbol_short!("esc"), symbol_short!("adm_prop")],
    );

    // Before acceptance, the proposed successor cannot exercise any admin authority.
    assert_eq!(
        world.escrow.try_pause(&successor),
        Err(Ok(EscrowError::Unauthorized)),
        "proposed admin must not be able to pause before accepting",
    );
    assert!(
        !world.escrow.is_paused(),
        "rejected pause by pending admin must leave contract unpaused",
    );
    assert_eq!(
        world.escrow.try_transfer_admin(&successor, &non_recipient),
        Err(Ok(EscrowError::Unauthorized)),
        "proposed admin must not be able to re-propose before accepting",
    );
    assert_eq!(
        world.escrow.get_pending_admin(),
        Some(successor.clone()),
        "failed re-proposal must preserve the pending successor",
    );

    // The original admin remains active while acceptance is delayed.
    world.escrow.pause(&admin);
    assert!(world.escrow.is_paused());
    assert_eq!(
        world.escrow.try_unpause(&successor),
        Err(Ok(EscrowError::Unauthorized)),
        "proposed admin must not be able to unpause before accepting",
    );
    assert!(world.escrow.is_paused());

    // Non-recipients — including the active admin itself — cannot accept on
    // the successor's behalf, and rejections leave the pending proposal intact.
    assert_eq!(
        world.escrow.try_accept_admin(&non_recipient),
        Err(Ok(EscrowError::Unauthorized)),
        "non-recipient must be rejected by accept_admin",
    );
    assert_eq!(
        world.escrow.try_accept_admin(&admin),
        Err(Ok(EscrowError::Unauthorized)),
        "active admin must not be able to force-accept on successor's behalf",
    );
    assert_eq!(
        world.escrow.get_pending_admin(),
        Some(successor.clone()),
        "rejected acceptance attempts must not clear pending admin",
    );

    // Step 2: the proposed successor accepts and becomes the active admin.
    world.escrow.accept_admin(&successor);
    assert_eq!(
        world.escrow.get_pending_admin(),
        None,
        "accept_admin must clear the pending admin slot",
    );
    a::assert_event_emitted(
        &env,
        &world.escrow_addr,
        &[symbol_short!("esc"), symbol_short!("adm_rot")],
    );

    // Replaying acceptance fails once the handover has completed.
    assert_eq!(
        world.escrow.try_accept_admin(&successor),
        Err(Ok(EscrowError::Unauthorized)),
    );

    // Authority is now revoked from the previous admin.
    assert_eq!(
        world.escrow.try_unpause(&admin),
        Err(Ok(EscrowError::Unauthorized)),
        "former admin must lose unpause authority after acceptance",
    );
    assert_eq!(
        world.escrow.try_transfer_admin(&admin, &non_recipient),
        Err(Ok(EscrowError::Unauthorized)),
        "former admin must lose transfer authority after acceptance",
    );
    assert!(world.escrow.is_paused());

    // The accepted successor now holds active admin authority.
    world.escrow.unpause(&successor);
    assert!(!world.escrow.is_paused());

    // Normal guarded operations resume after the new admin unpauses.
    let cid = Bytes::from_slice(&env.env, b"post-handover");
    world
        .escrow
        .create_escrow(&cid, &client, &artist, &AMOUNT, &world.config_stub_addr);
    a::assert_escrow_status(&env, &world.escrow, &cid, CommissionStatus::Locked);

    // And the new admin can pause again.
    world.escrow.pause(&successor);
    assert!(world.escrow.is_paused());
}

#[test]
fn cancelled_admin_proposal_prevents_subsequent_acceptance() {
    let env = Environment::new();
    let admin = env.address();
    let successor = env.address();
    let platform_wallet = env.address();

    let world = World::deploy(&env, &admin, &platform_wallet, FEE_BPS);
    world.escrow.initialize(&admin);

    world.escrow.transfer_admin(&admin, &successor);
    assert_eq!(world.escrow.get_pending_admin(), Some(successor.clone()));

    // Pending successor cannot cancel the proposal either.
    assert_eq!(
        world.escrow.try_cancel_admin_transfer(&successor),
        Err(Ok(EscrowError::Unauthorized)),
    );
    assert_eq!(world.escrow.get_pending_admin(), Some(successor.clone()));

    // Active admin cancels; successor can no longer accept and admin keeps control.
    world.escrow.cancel_admin_transfer(&admin);
    assert_eq!(world.escrow.get_pending_admin(), None);
    assert_eq!(
        world.escrow.try_accept_admin(&successor),
        Err(Ok(EscrowError::Unauthorized)),
    );

    world.escrow.pause(&admin);
    assert!(world.escrow.is_paused());
    world.escrow.unpause(&admin);
    assert!(!world.escrow.is_paused());
}
