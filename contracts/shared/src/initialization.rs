//! Shared initialization guards.
//!
//! Contracts can share the check for whether an initialization marker is
//! present or absent, while retaining their own typed error variants.
//!
//! The caller supplies the storage key and the error to return, so each
//! contract keeps its own error conventions while sharing the lookup.

use soroban_sdk::{Env, IntoVal, Val};

/// Returns `Ok(())` if instance storage holds `admin_key`, otherwise `err`.
///
/// The key is generic so a contract can pass its own `DataKey` variant, and the
/// error is generic so the contract keeps its own `Error::NotInitialized`
/// variant.
///
/// ```ignore
/// shared::initialization::require_initialized(&env, &DataKey::Admin, FundError::NotInitialized)?;
/// ```
pub fn require_initialized<K, E>(env: &Env, admin_key: &K, err: E) -> Result<(), E>
where
    K: IntoVal<Env, Val>,
{
    if env.storage().instance().has(admin_key) {
        Ok(())
    } else {
        Err(err)
    }
}

/// Returns `Ok(())` if instance storage does not hold `admin_key`, otherwise `err`.
///
/// Use this before writing initialization state to reject repeated
/// initialization attempts before they can overwrite existing values.
pub fn require_not_initialized<K, E>(env: &Env, admin_key: &K, err: E) -> Result<(), E>
where
    K: IntoVal<Env, Val>,
{
    if env.storage().instance().has(admin_key) {
        Err(err)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    enum TestError {
        NotInitialized,
        AlreadyInitialized,
    }

    /// Instance storage is only reachable from inside a deployed contract.
    fn with_contract<T>(env: &Env, f: impl FnOnce() -> T) -> T {
        let contract_id = env.register_contract(None, crate::TestContract);
        env.as_contract(&contract_id, f)
    }

    #[test]
    fn ok_when_the_key_is_present() {
        let env = Env::default();
        let result: Result<(), TestError> = with_contract(&env, || {
            env.storage().instance().set(&7u32, &true);
            require_initialized(&env, &7u32, TestError::NotInitialized)
        });
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn err_when_the_key_is_absent() {
        let env = Env::default();
        let result: Result<(), TestError> =
            with_contract(&env, || require_initialized(&env, &7u32, TestError::NotInitialized));
        assert_eq!(result, Err(TestError::NotInitialized));
    }

    #[test]
    fn ok_when_the_key_is_absent_for_initialization() {
        let env = Env::default();
        let result: Result<(), TestError> = with_contract(&env, || {
            require_not_initialized(&env, &7u32, TestError::AlreadyInitialized)
        });
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn err_when_the_key_is_present_for_initialization() {
        let env = Env::default();
        let result: Result<(), TestError> = with_contract(&env, || {
            env.storage().instance().set(&7u32, &true);
            require_not_initialized(&env, &7u32, TestError::AlreadyInitialized)
        });
        assert_eq!(result, Err(TestError::AlreadyInitialized));
    }
}
