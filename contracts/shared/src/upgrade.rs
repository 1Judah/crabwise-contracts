//! State migration and upgrade safety helpers (closes #595).
//!
//! This module provides:
//!
//! 1. `ContractVersion` — a semantic version struct stored in instance storage
//!    so the current WASM version can always be read on-chain.
//! 2. `require_upgrade_safe` — a pre-upgrade check that confirms the contract
//!    is paused before the WASM hash is replaced, preventing in-flight
//!    transactions from being processed against a half-migrated state.
//! 3. `record_upgrade` — writes the new version to instance storage and emits
//!    an `upgrade_complete` event after the WASM replacement.
//! 4. `export_storage_keys` — returns a list of all known top-level storage
//!    keys so an off-chain migration script can verify record counts before and
//!    after a schema change.
//!
//! ## Upgrade procedure (closes #595)
//!
//! ```text
//! 1.  Call `pause` to halt in-flight transactions.
//! 2.  Deploy the new WASM to a *new* contract address (never upgrade in-place
//!     on mainnet without prior testnet validation).
//! 3.  Call `migrate_v1_to_v2` (or the appropriate migration function) on the
//!     new contract to populate any new storage keys from exported data.
//! 4.  Verify record counts match via `export_storage_keys`.
//! 5.  Redirect clients to the new contract address.
//! 6.  Call `unpause` on the new contract to resume operations.
//! 7.  Emit `upgrade_complete` with old and new version numbers for audit.
//! ```

use soroban_sdk::{contracterror, contracttype, symbol_short, Address, Env, Symbol};

// ── Version tracking ─────────────────────────────────────────────────────────

/// Semantic version stored in instance storage (closes #595, #682).
///
/// Comparison and compatibility helpers live in [`crate::version`].
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContractVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

/// Compatibility classification for a proposed contract upgrade per
/// `docs/VERSIONING.md`.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpgradeDecision {
    /// Same storage schema and backward-compatible semver progression (patch
    /// bump, same-version re-deploy, or `>= 1.0` minor bump). Safe for a
    /// paused in-place WASM swap with no migration step.
    InPlaceAllowed,
    /// Valid forward upgrade that changes storage schema, bumps the major
    /// version, or bumps a pre-1.0 (`0.x`) minor version. Must deploy with an
    /// explicit migration step rather than a bare in-place swap.
    MigrationRequired,
}

/// Typed failures for upgrade and client compatibility checks.
#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpgradeError {
    /// The contract must be paused before an upgrade can proceed.
    ContractNotPaused = 1,
    /// Target semantic version is older than the currently installed version.
    VersionDowngrade = 2,
    /// Target storage schema is lower than the currently stored schema.
    SchemaDowngrade = 3,
    /// The upgrade requires a schema or breaking-version migration and cannot
    /// be performed as a bare in-place swap.
    MigrationRequired = 4,
    /// The running contract version cannot satisfy the client's required semver.
    IncompatibleClientVersion = 5,
    /// The contract's storage schema does not match what the caller expects.
    IncompatibleStorageSchema = 6,
}

impl core::fmt::Display for UpgradeError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::ContractNotPaused => write!(f, "contract must be paused before upgrading"),
            Self::VersionDowngrade => write!(f, "target contract version is older than current"),
            Self::SchemaDowngrade => write!(f, "target storage schema is lower than current"),
            Self::MigrationRequired => {
                write!(f, "upgrade requires explicit migration before activation")
            }
            Self::IncompatibleClientVersion => {
                write!(f, "contract version is incompatible with required client version")
            }
            Self::IncompatibleStorageSchema => {
                write!(f, "storage schema does not match expected schema")
            }
        }
    }
}

pub fn get_suggestion(error: UpgradeError) -> Symbol {
    match error {
        UpgradeError::ContractNotPaused => symbol_short!("NOT_PAUS"),
        UpgradeError::VersionDowngrade => symbol_short!("VER_DOWN"),
        UpgradeError::SchemaDowngrade => symbol_short!("SCH_DOWN"),
        UpgradeError::MigrationRequired => symbol_short!("MIGRATE"),
        UpgradeError::IncompatibleClientVersion => symbol_short!("BAD_VER"),
        UpgradeError::IncompatibleStorageSchema => symbol_short!("BAD_SCH"),
    }
}

#[contracttype]
pub enum UpgradeKey {
    /// Current contract semantic version.
    Version,
    /// Ledger sequence number at which the last upgrade was performed.
    LastUpgradeLedger,
    /// Active storage-schema generation persisted alongside `Version`.
    StorageSchema,
}

/// Evaluate whether upgrading from `(current_version, current_schema)` to
/// `(target_version, target_schema)` is allowed in-place, requires migration,
/// or must be rejected, following `docs/VERSIONING.md`.
pub fn check_upgrade_compatibility(
    current_version: &ContractVersion,
    current_schema: u32,
    target_version: &ContractVersion,
    target_schema: u32,
) -> Result<UpgradeDecision, UpgradeError> {
    if target_version.cmp_semver(current_version) == core::cmp::Ordering::Less {
        return Err(UpgradeError::VersionDowngrade);
    }
    if target_schema < current_schema {
        return Err(UpgradeError::SchemaDowngrade);
    }
    if target_schema > current_schema {
        return Ok(UpgradeDecision::MigrationRequired);
    }
    if target_version.major != current_version.major {
        return Ok(UpgradeDecision::MigrationRequired);
    }
    // Pre-1.0 (`0.x`) minor bumps may introduce breaking ABI changes per SemVer
    // and `docs/VERSIONING.md`.
    if current_version.major == 0 && target_version.minor != current_version.minor {
        return Ok(UpgradeDecision::MigrationRequired);
    }
    Ok(UpgradeDecision::InPlaceAllowed)
}

/// Enforce that an upgrade is safe for a bare in-place WASM swap (same storage
/// schema and backward-compatible semver progression).
pub fn require_in_place_upgrade_compatible(
    current_version: &ContractVersion,
    current_schema: u32,
    target_version: &ContractVersion,
    target_schema: u32,
) -> Result<(), UpgradeError> {
    match check_upgrade_compatibility(
        current_version,
        current_schema,
        target_version,
        target_schema,
    )? {
        UpgradeDecision::InPlaceAllowed => Ok(()),
        UpgradeDecision::MigrationRequired => Err(UpgradeError::MigrationRequired),
    }
}

/// Enforce both storage-schema equality and semver client compatibility per
/// `docs/VERSIONING.md`.
pub fn check_client_compatibility(
    contract_version: &ContractVersion,
    contract_schema: u32,
    required_client_version: &ContractVersion,
    expected_schema: u32,
) -> Result<(), UpgradeError> {
    if contract_schema != expected_schema {
        return Err(UpgradeError::IncompatibleStorageSchema);
    }
    if !contract_version.is_compatible_with(required_client_version) {
        return Err(UpgradeError::IncompatibleClientVersion);
    }
    Ok(())
}

/// Persist the active storage schema generation in instance storage.
pub fn set_storage_schema(env: &Env, schema: u32) {
    env.storage()
        .instance()
        .set(&UpgradeKey::StorageSchema, &schema);
}

/// Return the stored storage schema generation, defaulting to
/// [`crate::version::CURRENT_STORAGE_SCHEMA`] when not explicitly written yet.
pub fn get_storage_schema(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&UpgradeKey::StorageSchema)
        .unwrap_or(crate::version::CURRENT_STORAGE_SCHEMA)
}

/// Write `version` to instance storage and emit `upgrade_complete`.
///
/// Must be called by the admin from inside a guarded upgrade entry point.
/// Closes #595. Version constraint rules: `docs/VERSIONING.md` (#682).
pub fn record_upgrade(env: &Env, admin: &Address, version: ContractVersion) {
    admin.require_auth();
    let prev: Option<ContractVersion> = env.storage().instance().get(&UpgradeKey::Version);
    env.storage().instance().set(&UpgradeKey::Version, &version);
    env.storage().instance().set(&UpgradeKey::LastUpgradeLedger, &env.ledger().sequence());
    env.events().publish(
        (symbol_short!("upgraded"),),
        (prev, version, env.ledger().sequence()),
    );
}

/// Enforce pause state and compatibility rules before recording an upgrade.
///
/// Rejects unpaused upgrades, version/schema downgrades, and breaking or
/// schema-bumping upgrades unless `migration_applied` is `true`, leaving
/// stored version and schema untouched on refusal.
pub fn record_upgrade_checked(
    env: &Env,
    admin: &Address,
    target_version: ContractVersion,
    target_schema: u32,
    migration_applied: bool,
) -> Result<UpgradeDecision, UpgradeError> {
    require_paused_for_upgrade_typed(env)?;
    let current_version = get_version(env);
    let current_schema = get_storage_schema(env);
    let decision = check_upgrade_compatibility(
        &current_version,
        current_schema,
        &target_version,
        target_schema,
    )?;
    if decision == UpgradeDecision::MigrationRequired && !migration_applied {
        return Err(UpgradeError::MigrationRequired);
    }
    record_upgrade(env, admin, target_version);
    set_storage_schema(env, target_schema);
    Ok(decision)
}

/// Return the current stored version, or `{0,0,0}` if never set.
pub fn get_version(env: &Env) -> ContractVersion {
    env.storage()
        .instance()
        .get(&UpgradeKey::Version)
        .unwrap_or(ContractVersion { major: 0, minor: 0, patch: 0 })
}

/// Typed pre-upgrade pause check: returns [`UpgradeError::ContractNotPaused`]
/// if the shared pause flag is not currently set.
pub fn require_paused_for_upgrade_typed(env: &Env) -> Result<(), UpgradeError> {
    // Read the pause flag written by `shared::pause`.
    let paused: bool = env
        .storage()
        .instance()
        .get(&crate::pause::PauseDataKey::Paused)
        .unwrap_or(false);
    if !paused {
        Err(UpgradeError::ContractNotPaused)
    } else {
        Ok(())
    }
}

/// Panic if the contract is not paused.
///
/// Call this at the top of any admin-only `upgrade` entry point to ensure
/// no operations are in progress during WASM replacement (closes #595).
pub fn require_paused_for_upgrade(env: &Env) {
    if let Err(err) = require_paused_for_upgrade_typed(env) {
        panic!("{}", err);
    }
}

/// Emit a `migration_needed` event indicating that the caller should run the
/// off-chain data migration script before resuming operations.
///
/// Closes #595.
pub fn signal_migration_needed(env: &Env, from_version: ContractVersion, to_version: ContractVersion) {
    env.events().publish(
        (symbol_short!("mig_need"),),
        (from_version, to_version),
    );
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::pause;
    use soroban_sdk::testutils::Address as _;

    fn v(major: u32, minor: u32, patch: u32) -> ContractVersion {
        ContractVersion::new(major, minor, patch)
    }

    fn setup() -> (Env, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TestContract);
        let admin = Address::generate(&env);
        (env, contract_id, admin)
    }

    fn on<T>(env: &Env, contract_id: &Address, f: impl FnOnce() -> T) -> T {
        env.as_contract(contract_id, f)
    }

    #[test]
    fn compatible_in_place_upgrades_are_accepted() {
        // Pre-1.0 patch upgrade with identical schema.
        assert_eq!(
            check_upgrade_compatibility(&v(0, 1, 0), 1, &v(0, 1, 3), 1),
            Ok(UpgradeDecision::InPlaceAllowed)
        );
        assert!(require_in_place_upgrade_compatible(&v(0, 1, 0), 1, &v(0, 1, 3), 1).is_ok());

        // Stable >=1.0 patch and minor upgrades with identical schema.
        assert_eq!(
            check_upgrade_compatibility(&v(1, 2, 0), 2, &v(1, 2, 1), 2),
            Ok(UpgradeDecision::InPlaceAllowed)
        );
        assert_eq!(
            check_upgrade_compatibility(&v(1, 2, 0), 2, &v(1, 5, 0), 2),
            Ok(UpgradeDecision::InPlaceAllowed)
        );
        // Same-version idempotent re-deploy on identical schema.
        assert_eq!(
            check_upgrade_compatibility(&v(1, 2, 0), 2, &v(1, 2, 0), 2),
            Ok(UpgradeDecision::InPlaceAllowed)
        );
    }

    #[test]
    fn schema_bumps_major_bumps_and_pre_1_minor_bumps_require_migration() {
        // Storage schema bump on a patch release still requires migration.
        assert_eq!(
            check_upgrade_compatibility(&v(1, 0, 0), 1, &v(1, 0, 1), 2),
            Ok(UpgradeDecision::MigrationRequired)
        );
        assert_eq!(
            require_in_place_upgrade_compatible(&v(1, 0, 0), 1, &v(1, 0, 1), 2),
            Err(UpgradeError::MigrationRequired)
        );

        // Major bump requires migration even when storage schema is unchanged.
        assert_eq!(
            check_upgrade_compatibility(&v(1, 4, 2), 1, &v(2, 0, 0), 1),
            Ok(UpgradeDecision::MigrationRequired)
        );
        assert_eq!(
            require_in_place_upgrade_compatible(&v(1, 4, 2), 1, &v(2, 0, 0), 1),
            Err(UpgradeError::MigrationRequired)
        );

        // Pre-1.0 minor bump is breaking under SemVer 0.y.z.
        assert_eq!(
            check_upgrade_compatibility(&v(0, 1, 0), 1, &v(0, 2, 0), 1),
            Ok(UpgradeDecision::MigrationRequired)
        );
        assert_eq!(
            require_in_place_upgrade_compatible(&v(0, 1, 0), 1, &v(0, 2, 0), 1),
            Err(UpgradeError::MigrationRequired)
        );
    }

    #[test]
    fn version_and_schema_downgrades_are_rejected() {
        assert_eq!(
            check_upgrade_compatibility(&v(1, 2, 3), 2, &v(1, 2, 2), 2),
            Err(UpgradeError::VersionDowngrade)
        );
        assert_eq!(
            check_upgrade_compatibility(&v(2, 0, 0), 2, &v(1, 9, 9), 2),
            Err(UpgradeError::VersionDowngrade)
        );
        assert_eq!(
            check_upgrade_compatibility(&v(0, 2, 0), 1, &v(0, 1, 9), 1),
            Err(UpgradeError::VersionDowngrade)
        );
        assert_eq!(
            check_upgrade_compatibility(&v(1, 0, 0), 2, &v(1, 1, 0), 1),
            Err(UpgradeError::SchemaDowngrade)
        );
        assert_eq!(
            require_in_place_upgrade_compatible(&v(1, 0, 0), 2, &v(1, 0, 1), 1),
            Err(UpgradeError::SchemaDowngrade)
        );
    }

    #[test]
    fn client_compatibility_enforces_both_schema_and_semver() {
        // Compatible combinations.
        assert_eq!(check_client_compatibility(&v(0, 1, 4), 1, &v(0, 1, 0), 1), Ok(()));
        assert_eq!(check_client_compatibility(&v(0, 1, 4), 1, &v(0, 1, 4), 1), Ok(()));
        assert_eq!(check_client_compatibility(&v(1, 3, 1), 2, &v(1, 0, 0), 2), Ok(()));
        assert_eq!(check_client_compatibility(&v(1, 3, 1), 2, &v(1, 3, 1), 2), Ok(()));

        // Incompatible storage schema rejected even when semver matches exactly.
        assert_eq!(
            check_client_compatibility(&v(1, 3, 1), 2, &v(1, 3, 1), 1),
            Err(UpgradeError::IncompatibleStorageSchema)
        );
        assert_eq!(
            check_client_compatibility(&v(0, 1, 0), 1, &v(0, 1, 0), 2),
            Err(UpgradeError::IncompatibleStorageSchema)
        );

        // Incompatible client semver rejected even when schema matches.
        assert_eq!(
            check_client_compatibility(&v(0, 1, 4), 1, &v(0, 1, 5), 1),
            Err(UpgradeError::IncompatibleClientVersion)
        );
        assert_eq!(
            check_client_compatibility(&v(0, 1, 4), 1, &v(0, 2, 0), 1),
            Err(UpgradeError::IncompatibleClientVersion)
        );
        assert_eq!(
            check_client_compatibility(&v(1, 3, 1), 2, &v(1, 4, 0), 2),
            Err(UpgradeError::IncompatibleClientVersion)
        );
        assert_eq!(
            check_client_compatibility(&v(1, 3, 1), 2, &v(2, 0, 0), 2),
            Err(UpgradeError::IncompatibleClientVersion)
        );
    }

    #[test]
    fn record_upgrade_checked_enforces_pause_compatibility_and_migration() {
        let (env, id, admin) = setup();
        on(&env, &id, || {
            record_upgrade(&env, &admin, v(1, 0, 0));
            set_storage_schema(&env, 1);
        });

        // Unpaused contract rejects any upgrade and leaves state unchanged.
        assert_eq!(
            on(&env, &id, || record_upgrade_checked(&env, &admin, v(1, 0, 1), 1, false)),
            Err(UpgradeError::ContractNotPaused)
        );
        assert_eq!(on(&env, &id, || get_version(&env)), v(1, 0, 0));
        assert_eq!(on(&env, &id, || get_storage_schema(&env)), 1);

        // Pause the contract: compatible in-place minor upgrade succeeds.
        on(&env, &id, || pause::pause(&env, &admin));
        assert_eq!(
            on(&env, &id, || record_upgrade_checked(&env, &admin, v(1, 1, 0), 1, false)),
            Ok(UpgradeDecision::InPlaceAllowed)
        );
        assert_eq!(on(&env, &id, || get_version(&env)), v(1, 1, 0));
        assert_eq!(on(&env, &id, || get_storage_schema(&env)), 1);

        // Schema + major bump without migration is rejected cleanly.
        assert_eq!(
            on(&env, &id, || record_upgrade_checked(&env, &admin, v(2, 0, 0), 2, false)),
            Err(UpgradeError::MigrationRequired)
        );
        assert_eq!(on(&env, &id, || get_version(&env)), v(1, 1, 0));
        assert_eq!(on(&env, &id, || get_storage_schema(&env)), 1);

        // Same upgrade with `migration_applied = true` succeeds and updates schema.
        assert_eq!(
            on(&env, &id, || record_upgrade_checked(&env, &admin, v(2, 0, 0), 2, true)),
            Ok(UpgradeDecision::MigrationRequired)
        );
        assert_eq!(on(&env, &id, || get_version(&env)), v(2, 0, 0));
        assert_eq!(on(&env, &id, || get_storage_schema(&env)), 2);

        // Downgrades are still rejected even with `migration_applied = true`.
        assert_eq!(
            on(&env, &id, || record_upgrade_checked(&env, &admin, v(1, 9, 0), 2, true)),
            Err(UpgradeError::VersionDowngrade)
        );
        assert_eq!(
            on(&env, &id, || record_upgrade_checked(&env, &admin, v(2, 0, 1), 1, true)),
            Err(UpgradeError::SchemaDowngrade)
        );
        assert_eq!(on(&env, &id, || get_version(&env)), v(2, 0, 0));
        assert_eq!(on(&env, &id, || get_storage_schema(&env)), 2);
    }

    #[test]
    fn upgrade_error_codes_and_suggestions_are_stable() {
        assert_eq!(UpgradeError::ContractNotPaused as u32, 1);
        assert_eq!(UpgradeError::VersionDowngrade as u32, 2);
        assert_eq!(UpgradeError::SchemaDowngrade as u32, 3);
        assert_eq!(UpgradeError::MigrationRequired as u32, 4);
        assert_eq!(UpgradeError::IncompatibleClientVersion as u32, 5);
        assert_eq!(UpgradeError::IncompatibleStorageSchema as u32, 6);
        assert_eq!(
            get_suggestion(UpgradeError::IncompatibleStorageSchema),
            symbol_short!("BAD_SCH")
        );
    }
}
