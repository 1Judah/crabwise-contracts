//! End-to-end integration scenario demonstrating the framework (#663).
//!
//! Walks the full escrow lifecycle through contract boundaries: registry-based
//! dependency injection (#662), correlated events (#661), the classic release
//! path, and the atomic escrow→commission flow (#656).

use escrow::storage::CommissionStatus;
use integration_framework::assertions as a;
use integration_framework::dsl::Steps;
use integration_framework::{deploy_commission_stub, Environment, Scenario};
use platform_config::types::AddressEnvironment;
use soroban_sdk::{symbol_short, Bytes, BytesN};
use campaign_factory::{CampaignFactoryContract, CampaignFactoryContractClient};
use soroban_sdk::testutils::Events as _;
use soroban_sdk::TryFromVal;

const AMOUNT: i128 = 10_000;
const FEE_BPS: u32 = 500;
const FEE: i128 = 500;

#[test]
fn full_escrow_lifecycle_with_registry_and_correlation() {
    let env = Environment::new();
    let admin = env.address();
    let platform_wallet = env.address();
    let client = env.address();
    let artist = env.address();

    env.mint(&client, 1_000_000);

    // ── Scenario 1: registry-backed dependency injection (#662) ────────────
    let mut scenario = Scenario::begin(&env, &admin, &platform_wallet, FEE_BPS);
    scenario.step("registry: register & resolve usdc for both namespaces");

    scenario.world().config.register_address(
        &AddressEnvironment::Production,
        &symbol_short!("usdc"),
        &env.usdc,
    );
    scenario.world().config.register_address(
        &AddressEnvironment::Test,
        &symbol_short!("usdc"),
        &env.usdc,
    );
    scenario.world().config.set_environment(&AddressEnvironment::Test);

    a::assert_address_resolves(&scenario.world().config, &symbol_short!("usdc"), &env.usdc);
    a::assert_event_emitted(
        &env,
        &scenario.world().config_addr,
        &[symbol_short!("addrreg"), symbol_short!("usdc")],
    );

    // ── Scenario 2: classic create → release with correlated events (#661) ─
    scenario.step("escrow: create + release");
    let mut steps = Steps::new(scenario.world());
    let cid = Bytes::from_slice(&env.env, b"comm-001");

    steps
        .create(&cid, &client, &artist, AMOUNT)
        .release(&cid);

    a::assert_balance(&env, &artist, AMOUNT - FEE);
    a::assert_balance(&env, &platform_wallet, FEE);
    a::assert_correlation_event_present(
        &env,
        symbol_short!("escrow"),
        symbol_short!("created"),
    );

    // ── Scenario 3: atomic escrow→commission migration (#656) ─────────────
    scenario.step("atomic: two-party commit then single-transaction migration");
    let (commission, commission_client) = deploy_commission_stub(&env, AMOUNT);

    let cid2 = Bytes::from_slice(&env.env, b"comm-002");
    let mut steps = Steps::new(scenario.world());
    steps
        .create(&cid2, &client, &artist, AMOUNT)
        .begin_atomic(&cid2)
        .confirm_atomic(&cid2, &client)
        .confirm_atomic(&cid2, &artist);

    let marker = steps.migrate_atomically(&cid2, &commission);
    assert_eq!(marker.participants, 2);

    a::assert_balance(&env, &commission, AMOUNT - FEE);
    a::assert_balance(&env, &platform_wallet, 2 * FEE);
    a::assert_escrow_status(&env, &scenario.world().escrow, &cid, CommissionStatus::Released);
    let _ = commission_client;

    // Regression guard: released escrows surface an atomic marker.
    let settled = scenario.world().escrow.get_atomic_commit(&cid2);
    assert_eq!(settled.state, escrow::AtomicCommitState::Settled);
}
#[test]
fn failed_multi_contract_operation_rolls_back_participating_state() {
    let env = Environment::new();
    let admin = env.address();
    let platform_wallet = env.address();
    let client = env.address();
    let artist = env.address();

    // Client has less than the requested escrow amount, so the token transfer
    // (and the overall create_escrow transaction) must fail and roll back.
    env.mint(&client, AMOUNT - 1);

    let scenario = Scenario::begin(&env, &admin, &platform_wallet, FEE_BPS);
    let world = scenario.finish();
    let cid = Bytes::from_slice(&env.env, b"rollback-001");

    let client_balance_before = env.balance(&client);
    let escrow_balance_before = env.balance(&world.escrow_addr);

    let result = world.escrow.try_create_escrow(
        &cid,
        &client,
        &artist,
        &AMOUNT,
        &world.config_stub_addr,
    );
    assert!(result.is_err(), "create_escrow must fail when client balance is insufficient");

    // Focused rollback assertions: escrow storage and participating token balances
    // must all return to their pre-operation state.
    a::assert_escrow_create_rolled_back(
        &env,
        &world.escrow,
        &world.escrow_addr,
        &cid,
        &client,
        client_balance_before,
        escrow_balance_before,
    );
    a::assert_balances_unchanged(
        &env,
        &[
            (&platform_wallet, 0),
            (&artist, 0),
        ],
    );
}

#[test]
fn factory_rejects_invalid_deployment_inputs_before_creating_child() {
    let env = Environment::new();
    let admin = env.address();
    let treasury = env.address();
    let creator = env.address();
    let salt = BytesN::from_array(&env.env, [7u8; 32]);

    let factory_id = env.env.register_contract(None, CampaignFactoryContract);
    let factory = CampaignFactoryContractClient::new(&env.env, &factory_id);

    // Not initialized -> rejected before child creation.
    let before = env.env.events().all().len();
    assert!(factory
        .try_deploy_campaign(&creator, &env.usdc, &salt)
        .is_err());
    assert!(factory.get_deployed_campaign(&salt).is_none());
    assert_eq!(env.env.events().all().len(), before, "failed input validation must not emit success event");

    factory.initialize(&admin, &treasury);

    // Initialized but unsupported/missing deployment version (no wasm hash configured)
    // -> rejected before child creation.
    let before = env.env.events().all().len();
    assert!(factory
        .try_deploy_campaign(&creator, &env.usdc, &salt)
        .is_err());
    assert!(factory.get_deployed_campaign(&salt).is_none());
    assert_eq!(env.env.events().all().len(), before, "rejected request must not emit success event");

    // A valid request is one with required deployment inputs configured and a
    // previously-unused salt; if deployment cannot proceed, rejection must still
    // leave no registered address or success event behind.
    let wasm_hash = BytesN::from_array(&env.env, [9u8; 32]);
    factory.update_wasm_hash(&admin, &wasm_hash);
    let before = env.env.events().all().len();
    let _ = factory.try_deploy_campaign(&creator, &env.usdc, &salt);
    assert!(
        factory.get_deployed_campaign(&salt).is_none(),
        "deployment failure must not register a child contract"
    );
    assert_eq!(env.env.events().all().len(), before, "deployment failure must not emit success event");
}
