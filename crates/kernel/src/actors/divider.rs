//! The limit for this boot, derived once from the hardware and installed for the whole kernel.

use foundation::{configuration::Configuration, resource_budget::Limit};

use crate::synchronization::spin_lock::SpinLock;

static SYSTEM_LIMIT: SpinLock<Option<Limit>> = SpinLock::new(None);

/// Why the limit could not be installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallError {
    /// A limit was already installed for this boot.
    AlreadyInstalled,
}

/// Records the limit derived at boot, so every later spawn is checked against it.
///
/// Call once, before the first spawn.
///
/// # Errors
///
/// Returns [`InstallError::AlreadyInstalled`] when a limit is already installed.
pub fn install(limit: Limit) -> Result<(), InstallError> {
    let mut installed_limit = SYSTEM_LIMIT.lock();

    if installed_limit.is_some() {
        return Err(InstallError::AlreadyInstalled);
    }

    *installed_limit = Some(limit);
    Ok(())
}

/// The limit installed at boot.
///
/// # Panics
///
/// Panics when called before [`install`], which is a boot-order bug.
#[must_use]
pub fn system_limit() -> Limit {
    (*SYSTEM_LIMIT.lock()).expect("system limit read before installation")
}

/// The configuration an actor gets when its spawner has no special requirements.
#[must_use]
pub fn default_configuration() -> Configuration {
    Configuration::with_budget(system_limit().budget)
}
