//! Regression suite run against an upgraded instance.
//!
//! After an upgrade simulation the suite re-probes every surface that must not
//! regress: version queries, schema, admin, roster reads, migration markers,
//! and migration idempotency.

use soroban_sdk::{Address, Bytes, Env};

use crate::v2::V2Client;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegressionReport {
    pub version: (u32, u32, u32),
    pub schema: u32,
    pub rosters_ok: u32,
    pub members_ok: u32,
    pub migrated_ok: u32,
    pub migration_idempotent: bool,
    pub admin_preserved: bool,
    pub all_passed: bool,
}

/// Runs the post-upgrade regression battery for `roster_ids` whose
/// `members[rid]` must still resolve to `expected_members`.
pub fn regression_suite(
    env: &Env,
    contract: Address,
    admin: &Address,
    roster_ids: &[Bytes],
    expected_members: &[(Bytes, Address)],
) -> RegressionReport {
    let client = V2Client::new(env, &contract);

    let version = client.version();
    let schema = client.storage_schema();
    let admin_preserved = client.admin() == *admin;

    let mut rosters_ok = 0u32;
    let mut members_ok = 0u32;
    let mut migrated_ok = 0u32;

    for rid in roster_ids {
        if client.get_roster(rid).is_some() {
            rosters_ok += 1;
        }
        for (rid2, member) in expected_members {
            if rid2 == rid && client.get_member(rid2, member).is_some() {
                members_ok += 1;
            }
        }
        if client.is_migrated(rid) {
            migrated_ok += 1;
        }
    }

    // Idempotency: migrating a roster twice (and then a roster that is already
    // migrated) must succeed both times and keep shares stable.
    let reference = roster_ids.first().cloned();
    let migration_idempotent = match reference {
        Some(rid) => {
            client.migrate(admin, &rid)
                && client.migrate(admin, &rid)
                && client.is_migrated(&rid)
        }
        None => true,
    };

    let all_passed = version == (2, 0, 0)
        && schema == 2
        && admin_preserved
        && rosters_ok == roster_ids.len() as u32
        && members_ok == expected_members.len() as u32
        && migrated_ok == roster_ids.len() as u32
        && migration_idempotent;

    RegressionReport {
        version,
        schema,
        rosters_ok,
        members_ok,
        migrated_ok,
        migration_idempotent,
        admin_preserved,
        all_passed,
    }
}

/// `true` when the upgraded instance satisfies the whole regression battery.
pub fn verify(report: &RegressionReport) -> bool {
    report.all_passed
}

/// Compatibility classification mirroring `docs/VERSIONING.md` and
/// `shared::upgrade::check_upgrade_compatibility`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpgradeCompatibility {
    InPlaceAllowed,
    MigrationRequired,
    RejectedVersionDowngrade,
    RejectedSchemaDowngrade,
}

/// Client-to-contract compatibility outcome for `(version, storage_schema)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientCompatibility {
    Compatible,
    IncompatibleStorageSchema,
    IncompatibleClientVersion,
}

/// Evaluate an upgrade transition `(from_version, from_schema) -> (to_version, to_schema)`.
pub fn evaluate_upgrade_compatibility(
    from_version: (u32, u32, u32),
    from_schema: u32,
    to_version: (u32, u32, u32),
    to_schema: u32,
) -> UpgradeCompatibility {
    if to_version < from_version {
        return UpgradeCompatibility::RejectedVersionDowngrade;
    }
    if to_schema < from_schema {
        return UpgradeCompatibility::RejectedSchemaDowngrade;
    }
    if to_schema > from_schema {
        return UpgradeCompatibility::MigrationRequired;
    }
    if to_version.0 != from_version.0 {
        return UpgradeCompatibility::MigrationRequired;
    }
    if from_version.0 == 0 && to_version.1 != from_version.1 {
        return UpgradeCompatibility::MigrationRequired;
    }
    UpgradeCompatibility::InPlaceAllowed
}

/// Evaluate whether a contract at `(contract_version, contract_schema)` can
/// serve a client built for `(required_version, expected_schema)`.
pub fn evaluate_client_compatibility(
    contract_version: (u32, u32, u32),
    contract_schema: u32,
    required_version: (u32, u32, u32),
    expected_schema: u32,
) -> ClientCompatibility {
    if contract_schema != expected_schema {
        return ClientCompatibility::IncompatibleStorageSchema;
    }
    let semver_ok = if contract_version.0 != required_version.0 {
        false
    } else if contract_version.0 == 0 {
        contract_version.1 == required_version.1 && contract_version.2 >= required_version.2
    } else {
        contract_version >= required_version
    };
    if !semver_ok {
        return ClientCompatibility::IncompatibleClientVersion;
    }
    ClientCompatibility::Compatible
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulator::{migrate_RosterToShares, UpgradeSimulator};
    use crate::v1::V1Client;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::vec;

    fn seed_one_roster(sim: &UpgradeSimulator, v1: &Address, admin: &Address) -> (Bytes, Address) {
        let rid = Bytes::from_slice(&sim.env, b"compat-roster");
        let member = Address::generate(&sim.env);
        V1Client::new(&sim.env, v1).add_member(&rid, &vec![&sim.env, member.clone()], admin);
        (rid, member)
    }

    #[test]
    fn accepted_v1_to_v2_upgrade_passes_regression_and_client_checks() {
        let sim = UpgradeSimulator::new();
        sim.env.mock_all_auths();
        let admin = Address::generate(&sim.env);
        let v1 = sim.deploy_v1();
        let v1_client = V1Client::new(&sim.env, &v1);
        v1_client.initialize(&admin);
        let (rid, member) = seed_one_roster(&sim, &v1, &admin);

        let from_ver = v1_client.version();
        let from_schema = v1_client.storage_schema();

        let v2 = sim.upgrade_in_place(v1, std::slice::from_ref(&rid), migrate_RosterToShares);
        let report = regression_suite(
            &sim.env,
            v2,
            &admin,
            std::slice::from_ref(&rid),
            &[(rid.clone(), member)],
        );

        assert!(verify(&report), "migrated v1 -> v2 upgrade must pass regression");
        assert_eq!(
            evaluate_upgrade_compatibility(from_ver, from_schema, report.version, report.schema),
            UpgradeCompatibility::MigrationRequired
        );
        // Upgraded v2 contract serves v2/schema-2 clients, but rejects stale v1 or schema-1 clients.
        assert_eq!(
            evaluate_client_compatibility(report.version, report.schema, (2, 0, 0), 2),
            ClientCompatibility::Compatible
        );
        assert_eq!(
            evaluate_client_compatibility(report.version, report.schema, (1, 0, 0), 2),
            ClientCompatibility::IncompatibleClientVersion
        );
        assert_eq!(
            evaluate_client_compatibility(report.version, report.schema, (2, 0, 0), 1),
            ClientCompatibility::IncompatibleStorageSchema
        );
    }

    #[test]
    fn rejected_upgrade_without_schema_migration_fails_regression() {
        let sim = UpgradeSimulator::new();
        sim.env.mock_all_auths();
        let admin = Address::generate(&sim.env);
        let v1 = sim.deploy_v1();
        V1Client::new(&sim.env, &v1).initialize(&admin);
        let (rid, member) = seed_one_roster(&sim, &v1, &admin);

        // Restore v1 snapshot directly into v2 without bumping storage schema or running migration.
        let unmigrated_snap = sim.snapshot(&v1, std::slice::from_ref(&rid));
        let v2 = sim.deploy_v2();
        sim.restore(&v2, &unmigrated_snap);

        let report = regression_suite(
            &sim.env,
            v2,
            &admin,
            std::slice::from_ref(&rid),
            &[(rid.clone(), member)],
        );
        assert_eq!(report.schema, 1);
        assert_eq!(report.migrated_ok, 0);
        assert!(!verify(&report), "unmigrated schema mismatch must fail regression");
        assert_eq!(
            evaluate_client_compatibility(report.version, report.schema, (2, 0, 0), 2),
            ClientCompatibility::IncompatibleStorageSchema
        );
    }

    #[test]
    fn rejected_upgrade_with_missing_member_shares_or_wrong_admin_fails_regression() {
        let sim = UpgradeSimulator::new();
        sim.env.mock_all_auths();
        let admin = Address::generate(&sim.env);
        let wrong_admin = Address::generate(&sim.env);
        let v1 = sim.deploy_v1();
        V1Client::new(&sim.env, &v1).initialize(&admin);
        let (rid, member) = seed_one_roster(&sim, &v1, &admin);

        // Scenario A: schema bumped and relay marked true, but derived member shares dropped.
        let mut partial_snap = sim.snapshot(&v1, std::slice::from_ref(&rid));
        partial_snap.schema = 2;
        partial_snap.relays.push((rid.clone(), true));
        let v2_partial = sim.deploy_v2();
        sim.restore(&v2_partial, &partial_snap);

        let partial_report = regression_suite(
            &sim.env,
            v2_partial,
            &admin,
            std::slice::from_ref(&rid),
            &[(rid.clone(), member.clone())],
        );
        assert_eq!(partial_report.members_ok, 0);
        assert!(
            !verify(&partial_report),
            "upgrade that drops member share records must fail regression"
        );

        // Scenario B: migration ran, but admin key was overwritten during restore.
        let mut bad_admin_snap = sim.snapshot(&v1, std::slice::from_ref(&rid));
        migrate_RosterToShares(&mut bad_admin_snap);
        bad_admin_snap.admin = Some(wrong_admin);
        let v2_bad_admin = sim.deploy_v2();
        sim.restore(&v2_bad_admin, &bad_admin_snap);

        let bad_admin_report = regression_suite(
            &sim.env,
            v2_bad_admin,
            &admin,
            std::slice::from_ref(&rid),
            &[(rid.clone(), member)],
        );
        assert!(!bad_admin_report.admin_preserved);
        assert!(
            !verify(&bad_admin_report),
            "upgrade that corrupts stored admin must fail regression"
        );
    }

    #[test]
    fn compatible_and_incompatible_schema_and_client_version_matrix() {
        // Accepted in-place upgrades.
        assert_eq!(
            evaluate_upgrade_compatibility((0, 1, 0), 1, (0, 1, 2), 1),
            UpgradeCompatibility::InPlaceAllowed
        );
        assert_eq!(
            evaluate_upgrade_compatibility((1, 0, 0), 1, (1, 3, 0), 1),
            UpgradeCompatibility::InPlaceAllowed
        );

        // Migration-required upgrades.
        assert_eq!(
            evaluate_upgrade_compatibility((0, 1, 0), 1, (0, 2, 0), 1),
            UpgradeCompatibility::MigrationRequired
        );
        assert_eq!(
            evaluate_upgrade_compatibility((1, 2, 0), 1, (1, 2, 1), 2),
            UpgradeCompatibility::MigrationRequired
        );
        assert_eq!(
            evaluate_upgrade_compatibility((1, 4, 0), 2, (2, 0, 0), 2),
            UpgradeCompatibility::MigrationRequired
        );

        // Rejected downgrades.
        assert_eq!(
            evaluate_upgrade_compatibility((1, 2, 0), 2, (1, 1, 9), 2),
            UpgradeCompatibility::RejectedVersionDowngrade
        );
        assert_eq!(
            evaluate_upgrade_compatibility((2, 0, 0), 2, (2, 0, 1), 1),
            UpgradeCompatibility::RejectedSchemaDowngrade
        );

        // Client compatibility across pre-1.0, stable >=1.0, and schema mismatches.
        assert_eq!(
            evaluate_client_compatibility((0, 1, 4), 1, (0, 1, 0), 1),
            ClientCompatibility::Compatible
        );
        assert_eq!(
            evaluate_client_compatibility((0, 1, 4), 1, (0, 1, 5), 1),
            ClientCompatibility::IncompatibleClientVersion
        );
        assert_eq!(
            evaluate_client_compatibility((0, 1, 4), 1, (0, 2, 0), 1),
            ClientCompatibility::IncompatibleClientVersion
        );
        assert_eq!(
            evaluate_client_compatibility((1, 3, 1), 2, (1, 0, 0), 2),
            ClientCompatibility::Compatible
        );
        assert_eq!(
            evaluate_client_compatibility((1, 3, 1), 2, (1, 4, 0), 2),
            ClientCompatibility::IncompatibleClientVersion
        );
        assert_eq!(
            evaluate_client_compatibility((1, 3, 1), 2, (1, 3, 1), 3),
            ClientCompatibility::IncompatibleStorageSchema
        );
    }
}
