//! Decoding of the bootstrap message.

use foundation::{
    address::LocalAddress,
    stress::{BOOTSTRAP_TAG, Bootstrap, Scenario},
};

use crate::support::{OrFail, fail_at, fail_with_status, tagged};
use runtime::inbox;

/// What this actor was told to do.
#[derive(Clone, Copy)]
pub struct Assignment {
    /// The scenario to run.
    pub scenario: Scenario,
    /// The address of the peer actor that sent the bootstrap message.
    pub peer: LocalAddress,
    /// The role of this actor in the scenario. For example, a collector's senders are numbered 0..`role`-1.
    pub role: u64,
    /// A seed for pseudo-randomness. The kernel and the actor must use the same seed.
    pub seed: u64,
    /// An optional parameter for the scenario.
    pub param: u64,
}

impl Assignment {
    /// Blocks until the bootstrap message arrives. Any malformed message is fatal.
    pub fn receive(record: &mut [u8]) -> Self {
        let message = match inbox::await_matching(tagged(BOOTSTRAP_TAG), record) {
            Ok(message) => message,
            Err(error) => fail_with_status!(0x120, error.status.number()),
        };

        let Some(bootstrap) = Bootstrap::decode(message.payload) else {
            fail_at::<0x202>() // malformed or unknown scenario
        };

        let low = u32::try_from(bootstrap.peer & u64::from(u32::MAX)).or_fail();
        let high = u32::try_from(bootstrap.peer >> 32).or_fail();

        Self {
            scenario: bootstrap.scenario,
            peer: LocalAddress::new(low, high),
            role: bootstrap.role,
            seed: bootstrap.seed,
            param: bootstrap.param,
        }
    }
}
