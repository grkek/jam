//! Scenario catalogue and bootstrap wire format shared by the kernel's boot-time
//! stress plan and the EL0 stress program.

/// Message tag type used by the stress program. Change here if the runtime differs.
pub type Tag = u64;

/// Tag of the bootstrap message every stress actor waits for first.
pub const BOOTSTRAP_TAG: Tag = 0;
/// Tag of the junk the kernel sends to idle or dead actors.
pub const HAMMER_TAG: Tag = 3;
/// Little-endian `u64` words in a bootstrap message.
pub const BOOTSTRAP_WORDS: usize = 5;
/// Encoded size of a bootstrap message.
pub const BOOTSTRAP_BYTES: usize = BOOTSTRAP_WORDS * core::mem::size_of::<u64>();
/// Exchanges performed by [`Scenario::PingPong`].
pub const PING_PONG_EXCHANGES: u64 = 128;

/// What a stress actor does. The numbers are the wire format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum Scenario {
    /// Copy validation, then selective ping-pong. `param` = sequence at which both die (0 = never).
    PingPong = 0,
    /// Floods a collector. `role` = sender index, `param` = message count.
    CollectorSender = 1,
    /// Checks per-sender FIFO order. `role` = sender count, `param` = messages per sender.
    Collector = 2,
    /// Fills and drains its own inbox. `param != 0` demands identical capacity every cycle.
    SelfCycle = 3,
    /// Payload sizes and page-crossing offsets.
    SizeProbe = 4,
    /// Hostile `SystemCall` arguments.
    SystemCallFuzz = 5,
    /// Replies to requests. `param` = total requests to serve.
    Server = 6,
    /// Request/reply client. `role` = index, `param` = request count.
    Client = 7,
    /// Sends one request, then dies before the reply.
    ClientCrash = 8,
    /// Exits immediately (spawn/exit churn).
    QuickExit = 9,
    /// Burns CPU to prove preemption.
    SpinHog = 10,
    /// Spams an address that is dead or dying.
    DeadLetter = 11,
    /// Fills a peer's inbox until the kernel refuses.
    FillVictim = 12,
    /// Waits for a message nobody sends.
    BlockForever = 13,
    /// Deliberately triggers a breakpoint exception.
    CrashBreakpoint = 20,
    /// Deliberately triggers a null-pointer write exception.
    CrashNullWrite = 21,
    /// Deliberately triggers an execute-data exception.
    CrashExecuteData = 22,
    /// Deliberately triggers an undefined-instruction exception.
    CrashUndefined = 23,
    /// Deliberately triggers a panic exception.
    CrashPanic = 24,
    /// Deliberately triggers a stack-overflow exception.
    CrashStackOverflow = 25,
    /// Deliberately triggers a write-code exception.
    CrashWriteCode = 26,
}

impl Scenario {
    /// Every scenario; the single source of truth for [`Scenario::from_number`].
    pub const ALL: [Self; 21] = [
        Self::PingPong,
        Self::CollectorSender,
        Self::Collector,
        Self::SelfCycle,
        Self::SizeProbe,
        Self::SystemCallFuzz,
        Self::Server,
        Self::Client,
        Self::ClientCrash,
        Self::QuickExit,
        Self::SpinHog,
        Self::DeadLetter,
        Self::FillVictim,
        Self::BlockForever,
        Self::CrashBreakpoint,
        Self::CrashNullWrite,
        Self::CrashExecuteData,
        Self::CrashUndefined,
        Self::CrashPanic,
        Self::CrashStackOverflow,
        Self::CrashWriteCode,
    ];

    /// Returns the number used in the wire format.
    #[must_use]
    pub const fn number(self) -> u64 {
        self as u64
    }

    /// Returns the scenario for a number, or `None` if unknown.
    #[must_use]
    pub const fn from_number(number: u64) -> Option<Self> {
        match number {
            0 => Some(Self::PingPong),
            1 => Some(Self::CollectorSender),
            2 => Some(Self::Collector),
            3 => Some(Self::SelfCycle),
            4 => Some(Self::SizeProbe),
            5 => Some(Self::SystemCallFuzz),
            6 => Some(Self::Server),
            7 => Some(Self::Client),
            8 => Some(Self::ClientCrash),
            9 => Some(Self::QuickExit),
            10 => Some(Self::SpinHog),
            11 => Some(Self::DeadLetter),
            12 => Some(Self::FillVictim),
            13 => Some(Self::BlockForever),
            20 => Some(Self::CrashBreakpoint),
            21 => Some(Self::CrashNullWrite),
            22 => Some(Self::CrashExecuteData),
            23 => Some(Self::CrashUndefined),
            24 => Some(Self::CrashPanic),
            25 => Some(Self::CrashStackOverflow),
            26 => Some(Self::CrashWriteCode),
            _ => None,
        }
    }

    /// Scenarios whose only purpose is to die.
    #[must_use]
    pub const fn is_fault(self) -> bool {
        matches!(
            self,
            Self::CrashBreakpoint
                | Self::CrashNullWrite
                | Self::CrashExecuteData
                | Self::CrashUndefined
                | Self::CrashPanic
                | Self::CrashStackOverflow
                | Self::CrashWriteCode
        )
    }
}

/// The five-word message the kernel sends each stress actor first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bootstrap {
    /// Packed peer address (see the kernel's `pack_local_address`).
    pub peer: u64,

    /// The role of this actor in the scenario. For example, a collector's senders are numbered 0..`role`-1.
    pub role: u64,

    /// The scenario to run.
    pub scenario: Scenario,

    /// A seed for pseudo-randomness. The kernel and the actor must use the same seed.
    pub seed: u64,

    /// A scenario-specific parameter. For example, a collector's senders each send
    /// `param` messages.
    pub param: u64,
}

impl Bootstrap {
    /// Encodes the bootstrap message into a fixed-size byte array. The kernel and the actor must use the same encoding.
    #[must_use]
    pub fn encode(&self) -> [u8; BOOTSTRAP_BYTES] {
        let words = [
            self.peer,
            self.role,
            self.scenario.number(),
            self.seed,
            self.param,
        ];
        let mut bytes = [0; BOOTSTRAP_BYTES];
        for (chunk, word) in bytes.chunks_exact_mut(8).zip(words) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    /// `None` for a wrong length or an unknown scenario.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != BOOTSTRAP_BYTES {
            return None;
        }
        let word = |index: usize| -> Option<u64> {
            let start = index * 8;
            Some(u64::from_le_bytes(
                bytes.get(start..start + 8)?.try_into().ok()?,
            ))
        };
        Some(Self {
            peer: word(0)?,
            role: word(1)?,
            scenario: Scenario::from_number(word(2)?)?,
            seed: word(3)?,
            param: word(4)?,
        })
    }

    /// Faults this actor is designed to take. The kernel sums these so the CI log
    /// check can compare against the faults actually reported.
    #[must_use]
    pub const fn expected_faults(&self) -> usize {
        let dies = self.scenario.is_fault()
            || matches!(self.scenario, Scenario::ClientCrash)
            || (matches!(self.scenario, Scenario::PingPong)
                && self.param != 0
                && self.param < PING_PONG_EXCHANGES);
        dies as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_unique_and_round_trip() {
        for (index, scenario) in Scenario::ALL.iter().enumerate() {
            assert_eq!(Scenario::from_number(scenario.number()), Some(*scenario));
            for other in &Scenario::ALL[index + 1..] {
                assert_ne!(scenario.number(), other.number());
            }
        }
    }

    #[test]
    fn unknown_scenario_is_rejected() {
        assert_eq!(Scenario::from_number(u64::MAX), None);
        assert_eq!(Scenario::from_number(14), None);
    }

    #[test]
    fn bootstrap_round_trips() {
        let bootstrap = Bootstrap {
            peer: 0x0000_0007_0000_0003,
            role: 1,
            scenario: Scenario::Collector,
            seed: 0xdead_beef,
            param: 400,
        };
        assert_eq!(Bootstrap::decode(&bootstrap.encode()), Some(bootstrap));
    }

    #[test]
    fn decode_rejects_bad_input() {
        let mut bytes = Bootstrap {
            peer: 0,
            role: 0,
            scenario: Scenario::QuickExit,
            seed: 0,
            param: 0,
        }
        .encode();
        assert_eq!(Bootstrap::decode(&bytes[..BOOTSTRAP_BYTES - 1]), None);
        bytes[16..24].copy_from_slice(&999_u64.to_le_bytes());
        assert_eq!(Bootstrap::decode(&bytes), None);
    }

    #[test]
    fn expected_faults() {
        let faults = |scenario, param| {
            Bootstrap {
                peer: 0,
                role: 0,
                scenario,
                seed: 0,
                param,
            }
            .expected_faults()
        };
        assert_eq!(faults(Scenario::CrashPanic, 0), 1);
        assert_eq!(faults(Scenario::ClientCrash, 0), 1);
        assert_eq!(faults(Scenario::PingPong, 0), 0);
        assert_eq!(faults(Scenario::PingPong, 40), 1);
        assert_eq!(faults(Scenario::PingPong, PING_PONG_EXCHANGES), 0);
        assert_eq!(faults(Scenario::QuickExit, 0), 0);
    }
}
