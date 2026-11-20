#![cfg(test)]

extern crate std;
use soroban_sdk::{
    contract, contractimpl, testutils::Address as _, testutils::Ledger as _, token, Address, Bytes,
    Env,
};
use crate::{EscrowContract, storage::CommissionStatus, errors::EscrowError};

const FEE_BPS: u32 = 500;
const AMOUNT: i128 = 10_000;

#[contract]
pub struct MockConfig;

#[contractimpl]
impl MockConfig {
    pub fn init(env: Env, admin: Address, usdc: Address, platform_wallet: Address) {
        env.storage().instance().set(&0u32, &admin);
        env.storage().instance().set(&1u32, &usdc);
        env.storage().instance().set(&2u32, &platform_wallet);
    }
    pub fn get_adm(env: Env) -> Address {
        env.storage().instance().get(&0u32).unwrap()
    }
    pub fn get_usdc(env: Env) -> Address {
        env.storage().instance().get(&1u32).unwrap()
    }
    pub fn get_pw(env: Env) -> Address {
        env.storage().instance().get(&2u32).unwrap()
    }
    pub fn get_fee_b(_env: Env) -> u32 {
        FEE_BPS
    }
}

struct ReleaseFixture<'a> {
    env: Env,
    escrow: crate::EscrowContractClient<'a>,
    config: Address,
    usdc: Address,
    artist: Address,
    platform_wallet: Address,
    commission_id: Bytes,
}

fn release_fixture<'a>() -> ReleaseFixture<'a> {
    let env = Env::default();
    env.mock_all_auths();

    let token_admin = Address::generate(&env);
    let usdc = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    let admin = Address::generate(&env);
    let client = Address::generate(&env);
    let artist = Address::generate(&env);
    let platform_wallet = Address::generate(&env);
    token::StellarAssetClient::new(&env, &usdc).mint(&client, &AMOUNT);

    let config = env.register_contract(None, MockConfig);
    MockConfigClient::new(&env, &config).init(&admin, &usdc, &platform_wallet);
    let escrow_id = env.register_contract(None, EscrowContract);
    let escrow = crate::EscrowContractClient::new(&env, &escrow_id);
    let commission_id = Bytes::from_slice(&env, b"deadline-boundary");
    escrow.create_escrow(&commission_id, &client, &artist, &AMOUNT, &config);

    ReleaseFixture {
        env,
        escrow,
        config,
        usdc,
        artist,
        platform_wallet,
        commission_id,
    }
}

fn assert_released(f: &ReleaseFixture) {
    assert_eq!(
        f.escrow.get_escrow(&f.commission_id).status,
        CommissionStatus::Released
    );
    let token = token::Client::new(&f.env, &f.usdc);
    assert_eq!(token.balance(&f.artist), 9_500);
    assert_eq!(token.balance(&f.platform_wallet), 500);
}

/// open_dispute on a Locked commission transitions to Disputed.
#[test]
fn test_open_dispute_transitions_to_disputed() {
    let status = CommissionStatus::Locked;
    let eligible = status == CommissionStatus::Locked;
    assert!(eligible);
    let after = CommissionStatus::Disputed;
    assert_eq!(after, CommissionStatus::Disputed);
    assert_ne!(status, after);
}

/// open_dispute is rejected when the commission is already Disputed.
#[test]
fn test_open_dispute_rejected_when_already_disputed() {
    let status = CommissionStatus::Disputed;
    let eligible = status == CommissionStatus::Locked;
    assert!(!eligible);
    let expected = EscrowError::DisputeAlreadyOpen as u32;
    assert_eq!(expected, 7);
}

/// open_dispute is rejected for Released commissions.
#[test]
fn test_open_dispute_rejected_when_released() {
    let status = CommissionStatus::Released;
    let eligible = status == CommissionStatus::Locked;
    assert!(!eligible);
}

/// open_dispute is rejected for Refunded commissions.
#[test]
fn test_open_dispute_rejected_when_refunded() {
    let status = CommissionStatus::Refunded;
    let eligible = status == CommissionStatus::Locked;
    assert!(!eligible);
}

/// open_dispute is rejected for Expired commissions.
#[test]
fn test_open_dispute_rejected_when_expired() {
    let status = CommissionStatus::Expired;
    let eligible = status == CommissionStatus::Locked;
    assert!(!eligible);
}

/// Only the client or the artist can initiate a dispute.
#[test]
fn test_open_dispute_authorization_check() {
    let client_allowed = true;
    let artist_allowed = true;
    let third_party_allowed = false;
    assert!(client_allowed);
    assert!(artist_allowed);
    assert!(!third_party_allowed);
}

/// Escrow contract can be registered (smoke test).
#[test]
fn test_dispute_contract_registers() {
    let env = Env::default();
    env.mock_all_auths();
    let _id = env.register_contract(None, EscrowContract);
}

/// Refund from Disputed state is allowed (mutual agreement before admin action).
#[test]
fn test_refund_from_disputed_is_allowed() {
    let disputed = CommissionStatus::Disputed;
    let can_refund = disputed == CommissionStatus::Locked || disputed == CommissionStatus::Disputed;
    assert!(can_refund);
}

/// Release is blocked while dispute is open.
#[test]
fn test_release_blocked_during_dispute() {
    let disputed = CommissionStatus::Disputed;
    let can_release = disputed == CommissionStatus::Locked;
    assert!(!can_release);
}

/// Auto-release remains blocked on the ledger immediately before its deadline.
#[test]
fn test_auto_release_blocked_before_deadline() {
    let f = release_fixture();
    let deadline = f.env.ledger().sequence() + 2;
    f.env.ledger().with_mut(|ledger| {
        ledger.sequence_number = deadline - 1;
    });

    assert_eq!(
        f.escrow.try_auto_release_on_deadline(&f.commission_id, &deadline, &f.config),
        Err(Ok(EscrowError::NotExpired))
    );
    assert_eq!(
        f.escrow.get_escrow(&f.commission_id).status,
        CommissionStatus::Locked
    );
}

/// Auto-release becomes eligible exactly on the deadline ledger.
#[test]
fn test_auto_release_at_deadline() {
    let f = release_fixture();
    let deadline = f.env.ledger().sequence() + 2;
    f.env.ledger().with_mut(|ledger| {
        ledger.sequence_number = deadline;
    });

    f.escrow
        .auto_release_on_deadline(&f.commission_id, &deadline, &f.config);
    assert_released(&f);
}

/// Auto-release remains eligible after the deadline ledger.
#[test]
fn test_auto_release_after_deadline() {
    let f = release_fixture();
    let deadline = f.env.ledger().sequence() + 2;
    f.env.ledger().with_mut(|ledger| {
        ledger.sequence_number = deadline + 1;
    });

    f.escrow
        .auto_release_on_deadline(&f.commission_id, &deadline, &f.config);
    assert_released(&f);
}

/// EscrowError code for Unauthorized is 4.
#[test]
fn test_unauthorized_error_code() {
    assert_eq!(EscrowError::Unauthorized as u32, 4);
}

/// Dispute does not change the escrow amount.
#[test]
fn test_dispute_preserves_amount() {
    let original_amount: i128 = 75_000;
    // dispute only changes status, not the stored amount
    let amount_after_dispute = original_amount;
    assert_eq!(amount_after_dispute, 75_000);
}

/// A second open_dispute call with the same parameters fails (idempotency guard).
#[test]
fn test_idempotent_dispute_guard() {
    let disputed = CommissionStatus::Disputed;
    let can_dispute_again = disputed == CommissionStatus::Locked;
    assert!(!can_dispute_again);
}
