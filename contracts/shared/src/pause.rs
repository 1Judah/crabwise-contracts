//! Emergency pause and time-locked recovery (closes #594, #711).
//!
//! ## Pause
//!
//! [`pause`] sets the flag, [`is_paused`] reads it, and [`require_not_paused`]
//! is the guard every mutating entry point calls. Consuming contracts
//! (escrow, campaign, donation, withdrawal, revenue_sharing) all call
//! `pause::pause(&env, &admin)` and `pause::unpause(&env, &admin)` with the
//! same two arguments, so the existing [`pause`] / [`unpause`] signatures are
//! frozen and must not change.
//!
//! ## Time-locked recovery (#711)
//!
//! An immediate `unpause` means a single compromised — or simply mistaken —
//! admin keypress undoes an emergency stop. [`schedule_recovery`] arms a
//! time-lock instead: it records the ledger at which recovery matures, and
//! [`unpause`] then *refuses* to run before that ledger. The chosen flow is:
//!
//! 1. `pause(&env, &admin)` — stop the contract.
//! 2. `schedule_recovery(&env, &admin, delay_ledgers)` — declare the intent to
//!    resume. Returns the ledger at which recovery matures and emits
//!    `recovery_scheduled`. Further pauses clear the pending lock.
//! 3. Wait for the lock to mature. Monitors can watch `recovery_eta`.
//! 4. `unpause(&env, &admin)` (or [`try_unpause`]) resumes and emits both
//!    `recovery_completed` and the original `contract_unpaused`.
//!
//! A contract that has never scheduled a recovery has no lock, so
//! [`unpause`] keeps behaving exactly as it did before #711 and the five
//! consumer contracts need no change. [`cancel_recovery`] disarms a lock that
//! was armed in error without unpausing anything.
//!
//! ## Typed errors
//!
//! [`PauseError`] is the typed failure mode, and [`require_not_paused_typed`] /
//! [`try_unpause`] return it instead of aborting. [`require_not_paused`] and
//! [`unpause`] keep their historical infallible signatures and abort with the
//! typed error's description, which is why the guard is still safe for the
//! five consumers.
//!
//! ## Known limitation
//!
//! The five consumer contracts have not been migrated to
//! [`require_not_paused_typed`] yet — that is deliberately **not** done in the
//! #711 change, because those contracts are out of scope for it. They still
//! see an abort (a `HostError`) instead of a `PauseError` they can inspect.
//! Adopting the typed error is a follow-up that touches `escrow`,
//! `campaign`, `donation`, `withdrawal` and `revenue_sharing`.
//!
//! [`shared::rollout::trigger_rollback`] also sets the pause flag directly; it
//! deliberately does not arm or clear a recovery lock.

use soroban_sdk::{contracterror, contracttype, symbol_short, Address, Env, Symbol};

/// Default time-lock for recovering from a pause: ~24 h at 5 s/ledger.
pub const DEFAULT_RECOVERY_DELAY_LEDGERS: u32 = 17_280;
/// Longest time-lock an admin may arm: ~7 days at 5 s/ledger. A longer pause
/// is an incident response, not a recovery, and should be handled by
/// escalating rather than by setting a number nobody will wait out.
pub const MAX_RECOVERY_DELAY_LEDGERS: u32 = 120_960;

/// Typed failures of the pause mechanism (#711).
///
/// The matching entries in the unified catalogue
/// (`shared::errors::SharedErrorCode`) are `Unauthorized = 1`,
/// `ContractPaused = 6`, `RecoveryPending = 15`,
/// `RecoveryNotScheduled = 16`, `InvalidRecoveryDelay = 17` and
/// `ContractNotPaused = 18`.
#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PauseError {
    /// The contract is paused; the operation is not permitted.
    ContractPaused = 1,
    /// A recovery is scheduled but has not matured yet; the time-lock holds.
    RecoveryPending = 2,
    /// No recovery is scheduled, so there is nothing to cancel.
    RecoveryNotScheduled = 3,
    /// The requested delay is zero or beyond [`MAX_RECOVERY_DELAY_LEDGERS`].
    InvalidRecoveryDelay = 4,
    /// The contract is not paused, so it cannot be recovered from.
    NotPaused = 5,
    /// Caller does not match the configured pause authority.
    Unauthorized = 6,
}

impl core::fmt::Display for PauseError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::ContractPaused => write!(f, "contract is paused; operation not permitted"),
            Self::RecoveryPending => write!(
                f,
                "pause recovery is time-locked; the contract cannot be resumed yet"
            ),
            Self::RecoveryNotScheduled => write!(f, "no pause recovery is scheduled"),
            Self::InvalidRecoveryDelay => write!(f, "recovery delay is out of range"),
            Self::NotPaused => write!(f, "contract is not paused"),
            Self::Unauthorized => write!(f, "caller is not authorized to change pause state"),
        }
    }
}

pub fn get_suggestion(error: PauseError) -> Symbol {
    match error {
        PauseError::ContractPaused => symbol_short!("PAUSED"),
        PauseError::RecoveryPending => symbol_short!("LOCKED"),
        PauseError::RecoveryNotScheduled => symbol_short!("NO_RECOV"),
        PauseError::InvalidRecoveryDelay => symbol_short!("BAD_DELAY"),
        PauseError::NotPaused => symbol_short!("UNPAUSED"),
        PauseError::Unauthorized => symbol_short!("AUTH"),
    }
}

#[derive(Clone)]
#[contracttype]
pub enum PauseDataKey {
    Paused,
    /// Ledger at which a scheduled recovery matures. `0` — the value stored by
    /// `pause` and by `cancel_recovery` — means "no recovery armed". A scheduled
    /// recovery always lands on `now + delay` with `delay >= 1`, so `0` is an
    /// unambiguous sentinel (#711).
    RecoveryEta,
    /// Authorized pause admin when registered with [`set_admin`].
    ///
    /// When present, every pause/recovery transition checks the caller against
    /// this address before calling `require_auth` or mutating state.
    Admin,
}

#[derive(Clone)]
#[contracttype]
pub struct ContractPausedEvent {
    pub admin: Address,
}

#[derive(Clone)]
#[contracttype]
pub struct ContractUnpausedEvent {
    pub admin: Address,
}

/// Emitted when [`schedule_recovery`] arms the time-lock (#711).
#[derive(Clone)]
#[contracttype]
pub struct RecoveryScheduledEvent {
    pub admin: Address,
    /// Ledger at which `unpause` will be allowed to run.
    pub eta_ledger: u32,
    /// Ledgers the admin asked to wait, for off-chain reporting.
    pub delay_ledgers: u32,
}

/// Emitted when a time-locked recovery completes, i.e. when a scheduled
/// `unpause` is finally allowed through (#711).
#[derive(Clone)]
#[contracttype]
pub struct RecoveryCompletedEvent {
    pub admin: Address,
    /// The matured lock that was cleared by this call.
    pub eta_ledger: u32,
}

// ── Visibility ───────────────────────────────────────────────────────────────

/// `true` when the contract is currently paused.
///
/// Public counterpart to the private helper `shared::health` uses, so monitors
/// and integrators do not have to reach into storage keys (#711).
pub fn is_paused(env: &Env) -> bool {
    env.storage()
        .instance()
        .get(&PauseDataKey::Paused)
        .unwrap_or(false)
}

/// Ledger at which a scheduled recovery matures, or `None` when no recovery is
/// armed. Lets an operator or an off-chain monitor see the time-lock before
/// trying to resume (#711).
pub fn recovery_eta(env: &Env) -> Option<u32> {
    match env.storage().instance().get(&PauseDataKey::RecoveryEta) {
        Some(eta) if eta != 0 => Some(eta),
        _ => None,
    }
}

// ── Guards ───────────────────────────────────────────────────────────────────

/// Typed guard: returns [`PauseError::ContractPaused`] instead of aborting.
///
/// The five consumer contracts should adopt this once their error enums carry a
/// pause variant; that migration is deliberately out of scope for #711.
pub fn require_not_paused_typed(env: &Env) -> Result<(), PauseError> {
    if is_paused(env) {
        Err(PauseError::ContractPaused)
    } else {
        Ok(())
    }
}

/// Guard kept for the existing call sites that use it as a bare statement
/// (`campaign`, `donation`, `withdrawal`, `revenue_sharing`).
///
/// It now aborts with the typed error's description rather than the bare
/// `"contract is paused"` string, so a failed call is attributable to
/// `PauseError::ContractPaused` (#711).
pub fn require_not_paused(env: &Env) {
    if let Err(err) = require_not_paused_typed(env) {
        panic!("{}", err);
    }
}

// ── Pause authority ──────────────────────────────────────────────────────────

/// Register or rotate the authorized pause admin in instance storage.
///
/// Once configured, [`try_pause`], [`pause`], [`try_unpause`], [`unpause`],
/// [`schedule_recovery`], and [`cancel_recovery`] all reject any caller other
/// than this address with [`PauseError::Unauthorized`] before mutating state
/// or publishing events.
///
/// Callers invoking this from a contract entry point (e.g. `initialize` or
/// `accept_admin`) are responsible for authorizing the admin lifecycle change
/// at the contract layer.
pub fn set_admin(env: &Env, admin: &Address) {
    env.storage().instance().set(&PauseDataKey::Admin, admin);
}

/// The configured pause authority, if registered via [`set_admin`].
pub fn admin(env: &Env) -> Option<Address> {
    env.storage().instance().get(&PauseDataKey::Admin)
}

/// Enforce the documented pause authority consistently across every pause and
/// recovery entry point:
/// 1. If a pause admin is configured under [`PauseDataKey::Admin`], `caller`
///    must match it or fail immediately with [`PauseError::Unauthorized`].
/// 2. `caller.require_auth()` is always enforced so an unsigned invocation
///    cannot mutate pause state even when supplying the stored admin address.
fn require_pause_authority(env: &Env, caller: &Address) -> Result<(), PauseError> {
    if let Some(stored) = admin(env) {
        if stored != *caller {
            return Err(PauseError::Unauthorized);
        }
    }
    caller.require_auth();
    Ok(())
}

// ── Pause / recovery ─────────────────────────────────────────────────────────

/// Typed [`pause`]: enforces pause authority and returns
/// [`PauseError::Unauthorized`] if a configured pause admin does not match
/// `admin`. Any pending recovery is cleared on success.
pub fn try_pause(env: &Env, admin: &Address) -> Result<(), PauseError> {
    require_pause_authority(env, admin)?;
    env.storage().instance().set(&PauseDataKey::Paused, &true);
    env.storage().instance().set(&PauseDataKey::RecoveryEta, &0u32);
    env.events().publish(
        (Symbol::new(env, "contract_paused"),),
        ContractPausedEvent { admin: admin.clone() },
    );
    Ok(())
}

/// Pause the contract. Any pending recovery is cleared: a fresh stop invalidates
/// whatever the admin had scheduled before (#711).
///
/// Signature frozen: the consumer contracts call `pause(&env, &admin)` as a
/// bare statement. Delegates to [`try_pause`] and aborts with the typed error
/// description if authorization fails; new call sites should prefer
/// [`try_pause`].
pub fn pause(env: &Env, admin: &Address) {
    if let Err(err) = try_pause(env, admin) {
        panic!("{}", err);
    }
}

/// Arm a time-locked recovery and return the ledger at which it matures.
///
/// Must be called by the authorized pause admin while the contract is paused.
/// `delay_ledgers` is bounded to `1..=MAX_RECOVERY_DELAY_LEDGERS` so that a
/// zero delay cannot be used to dress an instant unpause up as a scheduled one
/// (#711).
pub fn schedule_recovery(
    env: &Env,
    admin: &Address,
    delay_ledgers: u32,
) -> Result<u32, PauseError> {
    require_pause_authority(env, admin)?;
    if !is_paused(env) {
        return Err(PauseError::NotPaused);
    }
    if delay_ledgers == 0 || delay_ledgers > MAX_RECOVERY_DELAY_LEDGERS {
        return Err(PauseError::InvalidRecoveryDelay);
    }
    let eta = env
        .ledger()
        .sequence()
        .checked_add(delay_ledgers)
        .ok_or(PauseError::InvalidRecoveryDelay)?;
    env.storage().instance().set(&PauseDataKey::RecoveryEta, &eta);
    env.events().publish(
        (Symbol::new(env, "recovery_scheduled"),),
        RecoveryScheduledEvent {
            admin: admin.clone(),
            eta_ledger: eta,
            delay_ledgers,
        },
    );
    Ok(eta)
}

/// Disarm a pending recovery without resuming the contract. Lets an admin undo
/// a mistaken schedule; the contract stays paused (#711).
pub fn cancel_recovery(env: &Env, admin: &Address) -> Result<(), PauseError> {
    require_pause_authority(env, admin)?;
    if recovery_eta(env).is_none() {
        return Err(PauseError::RecoveryNotScheduled);
    }
    env.storage().instance().set(&PauseDataKey::RecoveryEta, &0u32);
    Ok(())
}

/// Typed [`unpause`]: enforces pause authority and returns
/// [`PauseError::Unauthorized`] for non-admin callers or
/// [`PauseError::RecoveryPending`] while the time-lock is still running instead
/// of aborting (#711).
pub fn try_unpause(env: &Env, admin: &Address) -> Result<(), PauseError> {
    require_pause_authority(env, admin)?;
    // `>=` so recovery becomes possible exactly on the maturing ledger, matching
    // the `created + voting_ledgers` convention used by the rest of the repo.
    if let Some(eta) = recovery_eta(env) {
        if env.ledger().sequence() < eta {
            return Err(PauseError::RecoveryPending);
        }
        env.storage().instance().set(&PauseDataKey::RecoveryEta, &0u32);
        env.events().publish(
            (Symbol::new(env, "recovery_completed"),),
            RecoveryCompletedEvent {
                admin: admin.clone(),
                eta_ledger: eta,
            },
        );
    }
    env.storage().instance().set(&PauseDataKey::Paused, &false);
    env.events().publish(
        (Symbol::new(env, "contract_unpaused"),),
        ContractUnpausedEvent { admin: admin.clone() },
    );
    Ok(())
}

/// Resume the contract.
///
/// Signature frozen: the five consumer contracts call `unpause(&env, &admin)`
/// as a bare statement, so this cannot return a `Result` without breaking them
/// and failing `clippy -D warnings`. It refuses unauthorized callers and
/// refuses while a time-locked recovery is still running by aborting with the
/// typed error's description; new call sites should prefer [`try_unpause`] (#711).
pub fn unpause(env: &Env, admin: &Address) {
    if let Err(err) = try_unpause(env, admin) {
        panic!("{}", err);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use soroban_sdk::testutils::{Address as _, Events as _, Ledger as _};

    fn setup() -> (Env, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TestContract);
        let admin = Address::generate(&env);
        env.as_contract(&contract_id, || set_admin(&env, &admin));
        (env, contract_id, admin)
    }

    fn on<T>(env: &Env, contract_id: &Address, f: impl FnOnce() -> T) -> T {
        env.as_contract(contract_id, f)
    }

    #[test]
    fn authorized_admin_can_pause_and_unpause() {
        let (env, contract_id, admin) = setup();
        assert_eq!(
            on(&env, &contract_id, || super::admin(&env)),
            Some(admin.clone())
        );
        assert!(!on(&env, &contract_id, || is_paused(&env)));
        assert_eq!(
            on(&env, &contract_id, || require_not_paused_typed(&env)),
            Ok(())
        );

        on(&env, &contract_id, || try_pause(&env, &admin).unwrap());
        assert!(on(&env, &contract_id, || is_paused(&env)));
        assert_eq!(
            on(&env, &contract_id, || require_not_paused_typed(&env)),
            Err(PauseError::ContractPaused)
        );

        on(&env, &contract_id, || try_unpause(&env, &admin).unwrap());
        assert!(!on(&env, &contract_id, || is_paused(&env)));
        assert_eq!(
            on(&env, &contract_id, || require_not_paused_typed(&env)),
            Ok(())
        );
    }

    #[test]
    fn unauthorized_caller_cannot_pause_or_emit_events() {
        let (env, contract_id, _admin) = setup();
        let intruder = Address::generate(&env);

        let before = env.events().all().len();
        assert_eq!(
            on(&env, &contract_id, || try_pause(&env, &intruder)),
            Err(PauseError::Unauthorized)
        );
        assert!(!on(&env, &contract_id, || is_paused(&env)));
        assert_eq!(
            env.events().all().len(),
            before,
            "rejected pause must not publish any event"
        );
    }

    #[test]
    fn unauthorized_caller_cannot_unpause_or_clear_recovery() {
        let (env, contract_id, admin) = setup();
        let intruder = Address::generate(&env);

        on(&env, &contract_id, || try_pause(&env, &admin).unwrap());
        let eta = on(&env, &contract_id, || {
            schedule_recovery(&env, &admin, 10).unwrap()
        });

        let before = env.events().all().len();
        assert_eq!(
            on(&env, &contract_id, || try_unpause(&env, &intruder)),
            Err(PauseError::Unauthorized)
        );
        assert!(on(&env, &contract_id, || is_paused(&env)));
        assert_eq!(on(&env, &contract_id, || recovery_eta(&env)), Some(eta));
        assert_eq!(
            env.events().all().len(),
            before,
            "rejected unpause must not publish any event"
        );
    }

    #[test]
    fn unauthorized_caller_cannot_schedule_or_cancel_recovery() {
        let (env, contract_id, admin) = setup();
        let intruder = Address::generate(&env);

        // Even while unpaused, authority is checked before `NotPaused`.
        assert_eq!(
            on(&env, &contract_id, || schedule_recovery(&env, &intruder, 10)),
            Err(PauseError::Unauthorized)
        );
        assert_eq!(
            on(&env, &contract_id, || cancel_recovery(&env, &intruder)),
            Err(PauseError::Unauthorized)
        );

        on(&env, &contract_id, || try_pause(&env, &admin).unwrap());
        assert_eq!(
            on(&env, &contract_id, || schedule_recovery(&env, &intruder, 10)),
            Err(PauseError::Unauthorized)
        );
        assert_eq!(on(&env, &contract_id, || recovery_eta(&env)), None);

        let eta = on(&env, &contract_id, || {
            schedule_recovery(&env, &admin, 10).unwrap()
        });
        assert_eq!(
            on(&env, &contract_id, || cancel_recovery(&env, &intruder)),
            Err(PauseError::Unauthorized)
        );
        assert_eq!(on(&env, &contract_id, || recovery_eta(&env)), Some(eta));
    }

    #[test]
    #[should_panic(expected = "caller is not authorized to change pause state")]
    fn infallible_pause_aborts_for_unauthorized_caller() {
        let (env, contract_id, _admin) = setup();
        let intruder = Address::generate(&env);
        on(&env, &contract_id, || pause(&env, &intruder));
    }

    #[test]
    #[should_panic(expected = "caller is not authorized to change pause state")]
    fn infallible_unpause_aborts_for_unauthorized_caller() {
        let (env, contract_id, admin) = setup();
        let intruder = Address::generate(&env);
        on(&env, &contract_id, || pause(&env, &admin));
        on(&env, &contract_id, || unpause(&env, &intruder));
    }

    #[test]
    #[should_panic]
    fn pause_enforces_host_signature_even_for_stored_admin() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TestContract);
        let admin = Address::generate(&env);
        on(&env, &contract_id, || set_admin(&env, &admin));

        // Without mocking auth, calling `try_pause` as the stored admin must trap.
        let _ = on(&env, &contract_id, || try_pause(&env, &admin));
    }

    #[test]
    fn rotating_pause_admin_transfers_authority() {
        let (env, contract_id, old_admin) = setup();
        let new_admin = Address::generate(&env);

        on(&env, &contract_id, || try_pause(&env, &old_admin).unwrap());
        on(&env, &contract_id, || set_admin(&env, &new_admin));

        assert_eq!(
            on(&env, &contract_id, || try_unpause(&env, &old_admin)),
            Err(PauseError::Unauthorized)
        );
        assert!(on(&env, &contract_id, || is_paused(&env)));

        on(&env, &contract_id, || try_unpause(&env, &new_admin).unwrap());
        assert!(!on(&env, &contract_id, || is_paused(&env)));
    }

    #[test]
    fn time_locked_recovery_enforces_delay_and_matures() {
        let (env, contract_id, admin) = setup();
        on(&env, &contract_id, || try_pause(&env, &admin).unwrap());

        assert_eq!(
            on(&env, &contract_id, || schedule_recovery(&env, &admin, 0)),
            Err(PauseError::InvalidRecoveryDelay)
        );
        assert_eq!(
            on(&env, &contract_id, || schedule_recovery(
                &env,
                &admin,
                MAX_RECOVERY_DELAY_LEDGERS + 1
            )),
            Err(PauseError::InvalidRecoveryDelay)
        );

        let eta = on(&env, &contract_id, || {
            schedule_recovery(&env, &admin, 5).unwrap()
        });
        assert_eq!(
            on(&env, &contract_id, || try_unpause(&env, &admin)),
            Err(PauseError::RecoveryPending)
        );

        env.ledger().with_mut(|li| {
            li.sequence_number = eta;
        });
        on(&env, &contract_id, || try_unpause(&env, &admin).unwrap());
        assert!(!on(&env, &contract_id, || is_paused(&env)));
        assert_eq!(on(&env, &contract_id, || recovery_eta(&env)), None);
    }

    #[test]
    fn error_codes_and_suggestions_are_stable() {
        assert_eq!(PauseError::ContractPaused as u32, 1);
        assert_eq!(PauseError::RecoveryPending as u32, 2);
        assert_eq!(PauseError::RecoveryNotScheduled as u32, 3);
        assert_eq!(PauseError::InvalidRecoveryDelay as u32, 4);
        assert_eq!(PauseError::NotPaused as u32, 5);
        assert_eq!(PauseError::Unauthorized as u32, 6);
        assert_eq!(get_suggestion(PauseError::Unauthorized), symbol_short!("AUTH"));
    }
}
