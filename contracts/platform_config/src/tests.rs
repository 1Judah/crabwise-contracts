use crate::types::{AddressEnvironment, FeeTier, Promotion, ReferralConfig, ResolutionCacheEntry};
use crate::PlatformConfigContractClient;
use soroban_sdk::testutils::{Address as _, Events as _, Ledger};
use soroban_sdk::{symbol_short, Address, Env, FromVal, Symbol};

fn setup(
    env: &Env,
) -> (
    crate::PlatformConfigContractClient<'_>,
    Address,
    Address,
    Address,
) {
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::PlatformConfigContract);
    let client = PlatformConfigContractClient::new(env, &contract_id);
    let admin = Address::generate(env);
    let wallet = Address::generate(env);
    let token = Address::generate(env);
    client.initialize(&admin, &500, &wallet, &token);
    (client, admin, wallet, token)
}

#[test]
fn test_set_fee_bps_success() {
    let env = Env::default();
    let (client, _, _, _) = setup(&env);
    client.set_fee_bps(&200);
    assert_eq!(client.get_config().fee_bps, 200);
}

#[test]
fn test_set_fee_bps_accepts_maximum() {
    let env = Env::default();
    let (client, _, _, _) = setup(&env);
    client.set_fee_bps(&1000);
    assert_eq!(client.get_config().fee_bps, 1000);
}

#[test]
fn test_initialize_success() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::PlatformConfigContract);
    let client = PlatformConfigContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let wallet = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &500, &wallet, &token);
    let config = client.get_config();
    assert_eq!(config.admin, admin);
    assert_eq!(config.fee_bps, 500);
}

#[test]
fn test_set_fee_bps_rejects_above_maximum_without_changing_fee() {
    let env = Env::default();
    let (client, _, _, _) = setup(&env);
    let err = client
        .try_set_fee_bps(&1001)
        .err()
        .unwrap()
        .unwrap();
    assert_eq!(err, crate::ConfigError::InvalidFeeBps);
    assert_eq!(client.get_config().fee_bps, 500);
}

#[test]
fn test_transfer_admin_sets_pending() {
    let env = Env::default();
    let (client, _admin, _, _) = setup(&env);
    let new_admin = Address::generate(&env);
    client.transfer_admin(&new_admin);
    client.accept_admin();
    assert_eq!(client.get_config().admin, new_admin);
}

#[test]
fn test_accept_admin_updates_admin() {
    let env = Env::default();
    let (client, _old_admin, _, _) = setup(&env);
    let new_admin = Address::generate(&env);
    client.transfer_admin(&new_admin);
    client.accept_admin();
    let config = client.get_config();
    assert_eq!(config.admin, new_admin);
}

#[test]
fn test_initialize_rejects_repeated_attempt_without_side_effects() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::PlatformConfigContract);
    let client = PlatformConfigContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let wallet = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &500, &wallet, &token);

    let init_event_count = || {
        env.events()
            .all()
            .iter()
            .filter(|(_, topics, _)| {
                Symbol::from_val(&env, &topics.get(0).unwrap()) == symbol_short!("init")
            })
            .count()
    };
    assert_eq!(init_event_count(), 1);

    let replacement_admin = Address::generate(&env);
    let replacement_wallet = Address::generate(&env);
    let replacement_token = Address::generate(&env);
    let result = client.try_initialize(
        &replacement_admin,
        &900,
        &replacement_wallet,
        &replacement_token,
    );
    assert_eq!(
        result.err().unwrap().unwrap(),
        crate::ConfigError::AlreadyInitialized
    );

    let config = client.get_config();
    assert_eq!(config.admin, admin);
    assert_eq!(config.fee_bps, 500);
    assert_eq!(config.platform_wallet, wallet);
    assert_eq!(config.usdc_token, token);
    assert_eq!(init_event_count(), 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_initialize_fee_bps_too_high() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::PlatformConfigContract);
    let client = PlatformConfigContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let wallet = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &1001, &wallet, &token);
}

#[test]
fn test_get_config_returns_correct_values() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::PlatformConfigContract);
    let client = PlatformConfigContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let wallet = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &250, &wallet, &token);
    let config = client.get_config();
    assert_eq!(config.fee_bps, 250);
    assert_eq!(config.platform_wallet, wallet);
    assert_eq!(config.usdc_token, token);
}

#[test]
fn health_check_is_healthy_after_init() {
    let env = Env::default();
    let (client, admin, _, _) = setup(&env);
    let report = client.health_check();
    assert_eq!(report.status, shared::HealthStatus::Healthy);
    client.report_ok(&admin);
    assert_eq!(client.get_health_metrics().ok_count, 1);
}

// ── Address registry + resolution caching (#662) ────────────────────────────

const ESCROW: Symbol = symbol_short!("escrow");

#[test]
fn register_and_resolve_address_roundtrips() {
    let env = Env::default();
    let (client, _admin, _, _) = setup(&env);
    let escrow_addr = Address::generate(&env);
    client.register_address(&AddressEnvironment::Production, &ESCROW, &escrow_addr);
    let resolved = client.resolve_address(&AddressEnvironment::Production, &ESCROW);
    assert_eq!(resolved, escrow_addr);
}

#[test]
fn test_and_production_namespaces_are_independent() {
    let env = Env::default();
    let (client, _admin, _, _) = setup(&env);
    let prod = Address::generate(&env);
    let test = Address::generate(&env);
    client.register_address(&AddressEnvironment::Production, &ESCROW, &prod);
    client.register_address(&AddressEnvironment::Test, &ESCROW, &test);
    assert_eq!(
        client.resolve_address(&AddressEnvironment::Production, &ESCROW),
        prod
    );
    assert_eq!(
        client.resolve_address(&AddressEnvironment::Test, &ESCROW),
        test
    );
    assert_eq!(
        client.get_registered_address(&AddressEnvironment::Production, &ESCROW),
        prod
    );
}

#[test]
fn resolve_unregistered_address_fails() {
    let env = Env::default();
    let (client, _admin, _, _) = setup(&env);
    let err = client
        .try_resolve_address(&AddressEnvironment::Production, &ESCROW)
        .err()
        .unwrap()
        .unwrap();
    assert_eq!(err, crate::ConfigError::AddressNotRegistered);
}

#[test]
fn unregister_address_removes_entry() {
    let env = Env::default();
    let (client, _admin, _, _) = setup(&env);
    let escrow_addr = Address::generate(&env);
    client.register_address(&AddressEnvironment::Production, &ESCROW, &escrow_addr);
    client.unregister_address(&AddressEnvironment::Production, &ESCROW);
    let err = client
        .try_resolve_address(&AddressEnvironment::Production, &ESCROW)
        .err()
        .unwrap()
        .unwrap();
    assert_eq!(err, crate::ConfigError::AddressNotRegistered);
}

#[test]
fn register_overwrites_existing_address() {
    let env = Env::default();
    let (client, _admin, _, _) = setup(&env);
    let v1 = Address::generate(&env);
    let v2 = Address::generate(&env);
    client.register_address(&AddressEnvironment::Production, &ESCROW, &v1);
    client.register_address(&AddressEnvironment::Production, &ESCROW, &v2);
    assert_eq!(
        client.resolve_address(&AddressEnvironment::Production, &ESCROW),
        v2
    );
}

#[test]
fn resolve_populates_the_resolution_cache() {
    let env = Env::default();
    let (client, _admin, _, _) = setup(&env);
    let escrow_addr = Address::generate(&env);
    client.register_address(&AddressEnvironment::Production, &ESCROW, &escrow_addr);
    client.resolve_address(&AddressEnvironment::Production, &ESCROW);
    let cached: ResolutionCacheEntry =
        client.resolution_cache(&AddressEnvironment::Production, &ESCROW);
    assert_eq!(cached.address, escrow_addr);
    assert_eq!(cached.resolved_ledger, env.ledger().sequence());
}

#[test]
fn re_register_invalidates_the_cache() {
    let env = Env::default();
    let (client, _admin, _, _) = setup(&env);
    let v1 = Address::generate(&env);
    let v2 = Address::generate(&env);
    client.register_address(&AddressEnvironment::Production, &ESCROW, &v1);
    client.resolve_address(&AddressEnvironment::Production, &ESCROW);
    client.register_address(&AddressEnvironment::Production, &ESCROW, &v2);
    let resolved = client.resolve_address(&AddressEnvironment::Production, &ESCROW);
    assert_eq!(
        resolved, v2,
        "re-register must not be masked by a stale cache"
    );
}

#[test]
fn stale_cache_is_refreshed_from_the_registry() {
    let env = Env::default();
    // Move the ledger forward so a stamp of 0 looks stale against the TTL.
    env.ledger()
        .set_sequence_number(crate::storage::RESOLUTION_CACHE_TTL_LEDGERS + 10);
    let (client, _admin, _, _) = setup(&env);
    let escrow_addr = Address::generate(&env);
    client.register_address(&AddressEnvironment::Production, &ESCROW, &escrow_addr);
    client.resolve_address(&AddressEnvironment::Production, &ESCROW);

    // Forge an outdated cache stamp to exercise the staleness refresh branch.
    env.as_contract(&client.address, || {
        env.storage().instance().set(
            &crate::storage::DataKey::ResolutionCache(AddressEnvironment::Production, ESCROW),
            &ResolutionCacheEntry {
                address: escrow_addr.clone(),
                resolved_ledger: 0,
            },
        );
    });

    let cached: ResolutionCacheEntry =
        client.resolution_cache(&AddressEnvironment::Production, &ESCROW);
    assert!(
        env.ledger().sequence() - cached.resolved_ledger
            > crate::storage::RESOLUTION_CACHE_TTL_LEDGERS
    );

    // The re-resolve returns the same address (still registered) and refreshes the stamp.
    assert_eq!(
        client.resolve_address(&AddressEnvironment::Production, &ESCROW),
        escrow_addr
    );
    let fresh: ResolutionCacheEntry =
        client.resolution_cache(&AddressEnvironment::Production, &ESCROW);
    assert_eq!(fresh.resolved_ledger, env.ledger().sequence());
}

#[test]
fn active_environment_resolution_switches_with_set_environment() {
    let env = Env::default();
    let (client, _admin, _, _) = setup(&env);
    let prod = Address::generate(&env);
    let test = Address::generate(&env);
    client.register_address(&AddressEnvironment::Production, &ESCROW, &prod);
    client.register_address(&AddressEnvironment::Test, &ESCROW, &test);

    assert_eq!(client.resolve_for_environment(&ESCROW), prod);
    client.set_environment(&AddressEnvironment::Test);
    assert_eq!(client.get_environment(), AddressEnvironment::Test);
    assert_eq!(client.resolve_for_environment(&ESCROW), test);
}

#[test]
fn registry_entry_returns_full_record() {
    let env = Env::default();
    let (client, _admin, _, _) = setup(&env);
    let escrow_addr = Address::generate(&env);
    client.register_address(&AddressEnvironment::Test, &ESCROW, &escrow_addr);
    let entry = client.registry_entry(&AddressEnvironment::Test, &ESCROW);
    assert_eq!(entry.env, AddressEnvironment::Test);
    assert_eq!(entry.name, ESCROW);
    assert_eq!(entry.address, escrow_addr);
}

// ── Advanced fee structures (#690) ─────────────────────────────────────────

#[test]
fn fee_tiers_are_exported_and_lower_the_fee() {
    let env = Env::default();
    let (client, admin, _, _) = setup(&env);

    let five_pc = FeeTier {
        min_volume: 0,
        fee_bps: 500,
    };
    let four_pc = FeeTier {
        min_volume: 50_000,
        fee_bps: 400,
    };
    assert!(!client.upsert_fee_tier(&admin, &five_pc));
    assert!(!client.upsert_fee_tier(&admin, &four_pc));
    // Replacing an existing threshold reports `true`.
    assert!(client.upsert_fee_tier(&admin, &four_pc));

    assert_eq!(client.get_fee_tiers().len(), 2);
    // Base tier below threshold volume.
    assert_eq!(client.resolve_effective_fee_bps(&10_000), 500);
    // High-volume payer gets the cheaper tier.
    assert_eq!(client.resolve_effective_fee_bps(&60_000), 400);

    // Removal works and re-list reflects it.
    assert!(client.remove_fee_tier(&admin, &50_000));
    assert_eq!(client.get_fee_tiers().len(), 1);
}

#[test]
fn volume_tracks_cumulative_and_drives_discounts() {
    let env = Env::default();
    let (client, admin, _, _) = setup(&env);
    let payer = Address::generate(&env);

    let tier = FeeTier {
        min_volume: 100_000,
        fee_bps: 300,
    };
    client.upsert_fee_tier(&admin, &tier);

    assert_eq!(client.get_volume(&payer), 0);
    client.record_volume(&admin, &payer, &75_000);
    client.record_volume(&admin, &payer, &75_000);
    assert_eq!(client.get_volume(&payer), 150_000);
    // 150k >= 100k -> discounted tier.
    assert_eq!(client.resolve_effective_fee_bps(&150_000), 300);
}

#[test]
fn promotion_applies_only_inside_its_window() {
    let env = Env::default();
    let (client, admin, _, _) = setup(&env);

    env.ledger().with_mut(|l| l.sequence_number = 1_000);
    let promo = Promotion {
        start_ledger: 1_000,
        end_ledger: 2_000,
        fee_bps: 100,
    };
    client.set_promotion(&admin, &promo);
    assert!(client.is_promotion_active());

    let b = client.compute_fees(&1_000_000, &0, &None);
    assert_eq!(b.effective_fee_bps, 100);
    assert_eq!(b.fee, 10_000);

    // After the window the promotion no longer applies.
    env.ledger().with_mut(|l| l.sequence_number = 2_001);
    assert!(!client.is_promotion_active());
    let b = client.compute_fees(&1_000_000, &0, &None);
    assert_eq!(b.effective_fee_bps, 500);

    client.clear_promotion(&admin);
    assert!(!client.is_promotion_active());
}

#[test]
fn referral_config_splits_the_platform_fee() {
    let env = Env::default();
    let (client, admin, referrer, _) = setup(&env);

    client.set_referral_config(&admin, &ReferralConfig { bps: 2000 });
    assert_eq!(client.get_referral_config().unwrap().bps, 2000);

    // 1_000_000 * 500bps = 50_000 fee; referrer gets 20% = 10_000.
    let b = client.compute_fees(&1_000_000, &0, &Some(referrer));
    assert_eq!(b.effective_fee_bps, 500);
    assert_eq!(b.fee, 50_000);
    assert_eq!(b.referral_fee, 10_000);
    assert_eq!(b.platform_fee, 40_000);
    assert_eq!(b.payout, 950_000);
}

#[test]
fn invalid_fee_configs_are_rejected() {
    let env = Env::default();
    let (client, admin, _, _) = setup(&env);

    let bad_tier = FeeTier {
        min_volume: 0,
        fee_bps: 1001,
    };
    assert_eq!(
        client
            .try_upsert_fee_tier(&admin, &bad_tier)
            .err()
            .unwrap()
            .unwrap(),
        crate::errors::ConfigError::InvalidTier
    );

    let inverted = Promotion {
        start_ledger: 100,
        end_ledger: 50,
        fee_bps: 100,
    };
    assert_eq!(
        client
            .try_set_promotion(&admin, &inverted)
            .err()
            .unwrap()
            .unwrap(),
        crate::errors::ConfigError::InvalidPromotion
    );

    assert_eq!(
        client
            .try_set_referral_config(&admin, &ReferralConfig { bps: 15_000 })
            .err()
            .unwrap()
            .unwrap(),
        crate::errors::ConfigError::InvalidReferralBps
    );
}

// ── Circuit-breaker boundary and recovery tests (closes #8) ───────────────────

fn on<T>(env: &Env, contract_id: &Address, f: impl FnOnce() -> T) -> T {
    env.as_contract(contract_id, f)
}

#[test]
fn test_circuit_breaker_boundary_thresholds_and_insufficient_samples() {
    let env = Env::default();
    let (client, admin, _, _) = setup(&env);

    on(&env, &client.address, || {
        // Initial defaults
        assert_eq!(
            shared::circuit_breaker::get_halt_error_bps(&env),
            shared::circuit_breaker::DEFAULT_HALT_ERROR_BPS
        );
        assert!(!shared::circuit_breaker::is_halted(&env));
        assert_eq!(shared::circuit_breaker::require_not_halted(&env), Ok(()));

        // Configure custom threshold (300 bps = 3%)
        shared::circuit_breaker::set_config(
            &env,
            &shared::circuit_breaker::CircuitBreakerConfig {
                admin: admin.clone(),
                halt_error_bps: 300,
            },
        );
        assert_eq!(shared::circuit_breaker::get_halt_error_bps(&env), 300);

        // Record 8 failures (below MIN_SAMPLE_FOR_HALT = 10)
        for _ in 0..8 {
            let halted = shared::circuit_breaker::record_failure(&env);
            assert!(!halted, "must not halt when sample count is insufficient");
        }

        assert!(!shared::circuit_breaker::is_halted(&env));
        let state = shared::circuit_breaker::get_state(&env);
        assert_eq!(state.total_calls, 8);
        assert_eq!(state.error_count, 8);
        assert_eq!(state.ok_count, 0);
        assert!(!state.halted);
        assert_eq!(state.halt_error_bps, 300);
    });

    // Cross-check health metrics reflected via contract client
    let metrics = client.get_health_metrics();
    assert_eq!(metrics.error_count, 8);
}

#[test]
fn test_circuit_breaker_boundary_exact_threshold_vs_halt() {
    let env = Env::default();
    let (client, admin, _, _) = setup(&env);

    on(&env, &client.address, || {
        shared::circuit_breaker::set_config(
            &env,
            &shared::circuit_breaker::CircuitBreakerConfig {
                admin: admin.clone(),
                halt_error_bps: 500, // 5%
            },
        );
    });

    on(&env, &client.address, || {
        // 95 successes and 5 failures = exactly 500 bps (5%)
        for _ in 0..95 {
            shared::circuit_breaker::record_success(&env);
        }
        for _ in 0..5 {
            let halted = shared::circuit_breaker::record_failure(&env);
            assert!(!halted, "exact threshold rate must not halt");
        }
    });

    assert!(
        !on(
            &env,
            &client.address,
            || shared::circuit_breaker::is_halted(&env)
        ),
        "circuit breaker must not halt exactly at threshold"
    );
    assert_eq!(
        on(&env, &client.address, || {
            shared::circuit_breaker::require_not_halted(&env)
        }),
        Ok(())
    );

    // 1 additional failure crosses the boundary (6 errors / 101 total ~ 594 bps > 500)
    let did_halt = on(&env, &client.address, || {
        shared::circuit_breaker::record_failure(&env)
    });
    assert!(did_halt, "crossing threshold must trigger auto-halt");
    assert!(on(&env, &client.address, || {
        shared::circuit_breaker::is_halted(&env)
    }));
    assert_eq!(
        on(&env, &client.address, || {
            shared::circuit_breaker::require_not_halted(&env)
        }),
        Err(shared::circuit_breaker::CircuitBreakerError::Halted)
    );

    let halt_seq = on(&env, &client.address, || {
        shared::circuit_breaker::last_halt_ledger(&env)
    });
    assert_eq!(halt_seq, env.ledger().sequence());

    // Repeated failures while already halted must not trigger duplicate halts
    let duplicate = on(&env, &client.address, || {
        shared::circuit_breaker::record_failure(&env)
    });
    assert!(
        !duplicate,
        "repeated failure while halted must return false"
    );
    assert_eq!(
        on(&env, &client.address, || {
            shared::circuit_breaker::last_halt_ledger(&env)
        }),
        halt_seq
    );

    let err = on(&env, &client.address, || {
        shared::circuit_breaker::halt(&env, &admin).unwrap_err()
    });
    assert_eq!(
        err,
        shared::circuit_breaker::CircuitBreakerError::AlreadyHalted
    );

    // Health check reflects paused state
    let report = client.health_check();
    assert_eq!(report.status, shared::HealthStatus::Unhealthy);
}

#[test]
fn test_circuit_breaker_boundary_timelocked_recovery_and_transient_failures() {
    let env = Env::default();
    let (client, admin, _, _) = setup(&env);

    on(&env, &client.address, || {
        shared::circuit_breaker::set_config(
            &env,
            &shared::circuit_breaker::CircuitBreakerConfig {
                admin: admin.clone(),
                halt_error_bps: 500,
            },
        );
    });

    // Cause auto-halt with 10 failures
    on(&env, &client.address, || {
        for _ in 0..10 {
            shared::circuit_breaker::record_failure(&env);
        }
    });
    assert!(on(&env, &client.address, || {
        shared::circuit_breaker::is_halted(&env)
    }));

    // Schedule recovery for 20 ledgers
    let start_seq = env.ledger().sequence();
    let eta = on(&env, &client.address, || {
        shared::circuit_breaker::schedule_recovery(&env, &admin, 20).unwrap()
    });
    assert_eq!(eta, start_seq + 20);
    assert_eq!(
        on(&env, &client.address, || {
            shared::circuit_breaker::get_state(&env).recovery_eta
        }),
        Some(eta)
    );

    // Resume before maturity is rejected
    env.ledger().with_mut(|l| l.sequence_number = eta - 1);
    let err = on(&env, &client.address, || {
        shared::circuit_breaker::resume(&env, &admin).unwrap_err()
    });
    assert_eq!(
        err,
        shared::circuit_breaker::CircuitBreakerError::RecoveryPending
    );
    assert!(on(&env, &client.address, || {
        shared::circuit_breaker::is_halted(&env)
    }));

    // Resume allowed at exact maturity ledger
    env.ledger().with_mut(|l| l.sequence_number = eta);
    on(&env, &client.address, || {
        shared::circuit_breaker::resume(&env, &admin).unwrap();
    });

    assert!(!on(&env, &client.address, || {
        shared::circuit_breaker::is_halted(&env)
    }));
    assert_eq!(
        on(&env, &client.address, || {
            shared::circuit_breaker::require_not_halted(&env)
        }),
        Ok(())
    );
    assert_eq!(
        on(&env, &client.address, || {
            shared::circuit_breaker::get_state(&env).recovery_eta
        }),
        None
    );

    // Sample counters must be reset upon recovery
    let state = on(&env, &client.address, || {
        shared::circuit_breaker::get_state(&env)
    });
    assert_eq!(state.total_calls, 0);
    assert_eq!(state.ok_count, 0);
    assert_eq!(state.error_count, 0);
    assert_eq!(state.error_bps, 0);

    // Advance sequence past the minimum halt interval
    env.ledger().with_mut(|l| {
        l.sequence_number += shared::circuit_breaker::MIN_HALT_INTERVAL_LEDGERS + 5;
    });

    // A new transient failure must not immediately re-halt the breaker
    let rehalted = on(&env, &client.address, || {
        shared::circuit_breaker::record_failure(&env)
    });
    assert!(
        !rehalted,
        "transient failure after recovery must not cause immediate re-halt"
    );
    assert!(!on(&env, &client.address, || {
        shared::circuit_breaker::is_halted(&env)
    }));
    assert_eq!(
        on(&env, &client.address, || {
            shared::circuit_breaker::require_not_halted(&env)
        }),
        Ok(())
    );

    let state_after = on(&env, &client.address, || {
        shared::circuit_breaker::get_state(&env)
    });
    assert_eq!(state_after.total_calls, 1);
    assert_eq!(state_after.error_count, 1);
    assert_eq!(state_after.ok_count, 0);
}

#[test]
fn test_circuit_breaker_boundary_flapping_guard_across_recovery_cycles() {
    let env = Env::default();
    let (client, admin, _, _) = setup(&env);

    env.ledger().with_mut(|l| l.sequence_number = 500);

    on(&env, &client.address, || {
        shared::circuit_breaker::set_config(
            &env,
            &shared::circuit_breaker::CircuitBreakerConfig {
                admin: admin.clone(),
                halt_error_bps: 500,
            },
        );
    });

    // Auto-halt at ledger 500
    on(&env, &client.address, || {
        for _ in 0..10 {
            shared::circuit_breaker::record_failure(&env);
        }
    });
    assert!(on(&env, &client.address, || {
        shared::circuit_breaker::is_halted(&env)
    }));
    assert_eq!(
        on(&env, &client.address, || {
            shared::circuit_breaker::last_halt_ledger(&env)
        }),
        500
    );

    // Schedule recovery for 10 ledgers
    let eta = on(&env, &client.address, || {
        shared::circuit_breaker::schedule_recovery(&env, &admin, 10).unwrap()
    });
    assert_eq!(eta, 510);
    env.ledger().with_mut(|l| l.sequence_number = 510);
    on(&env, &client.address, || {
        shared::circuit_breaker::resume(&env, &admin).unwrap();
    });
    assert!(!on(&env, &client.address, || {
        shared::circuit_breaker::is_halted(&env)
    }));

    // Advance to ledger 530 (< 60 ledgers since halt at 500)
    env.ledger().with_mut(|l| l.sequence_number = 530);

    // Rapid failures: 10 errors
    on(&env, &client.address, || {
        for _ in 0..10 {
            shared::circuit_breaker::record_failure(&env);
        }
    });

    // Flapping guard must suppress auto-halt
    assert!(
        !on(
            &env,
            &client.address,
            || shared::circuit_breaker::is_halted(&env)
        ),
        "flapping guard must suppress halt within 60 ledgers of previous halt"
    );

    // Advance past flapping interval: 560 - 500 = 60 >= MIN_HALT_INTERVAL_LEDGERS
    env.ledger().with_mut(|l| l.sequence_number = 560);

    // Subsequent failure now triggers a valid halt
    let halted = on(&env, &client.address, || {
        shared::circuit_breaker::record_failure(&env)
    });
    assert!(
        halted,
        "valid halt should fire once flapping interval expires"
    );
    assert!(on(&env, &client.address, || {
        shared::circuit_breaker::is_halted(&env)
    }));
    assert_eq!(
        on(&env, &client.address, || {
            shared::circuit_breaker::last_halt_ledger(&env)
        }),
        560
    );
}
