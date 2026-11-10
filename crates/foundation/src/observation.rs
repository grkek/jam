//! Pointer-free observation records encoded as little-endian 64-bit words.
//!
//! `ReadCensus`: x0 is the slot index, x1 the output pointer, x2 the byte capacity;
//! x3..x5 must be zero. Success returns the slot capacity in x1. Free slots are
//! represented explicitly. Each record is consistent under its actor lock;
//! successive records need not describe the same instant.
//!
//! `ReadCoreCensus`: x0 is a core index, x1 an output pointer, x2 the byte
//! capacity; x3..x5 must be zero. Success returns the supported core slots in x1.
//! Counters are sampled independently and may differ during concurrent updates.
//!
//! `ReadStatistics`: x0 is the output pointer, x1 the byte capacity; x2..x5
//! must be zero. Success returns the record byte count in x1. Its counters are
//! sampled independently and are diagnostic, not an atomic resource ledger.
//! Both calls report required bytes in x1 on `BufferTooSmall` and validate the
//! entire writable user range before copying. Neither record exposes pointers.

/// Encoding version shared by both records.
pub const RECORD_VERSION: u64 = 1;
/// Words in one actor census record.
pub const ACTOR_RECORD_WORD_COUNT: usize = 8;
/// Words in the node statistics record.
pub const STATISTICS_RECORD_WORD_COUNT: usize = 10;
/// Words in the per-core scheduler census record.
pub const CORE_RECORD_WORD_COUNT: usize = 10;
/// Bytes in a core scheduler census record.
pub const CORE_RECORD_BYTE_COUNT: usize = CORE_RECORD_WORD_COUNT * 8;
/// Bytes in one actor census record.
pub const ACTOR_RECORD_BYTE_COUNT: usize = ACTOR_RECORD_WORD_COUNT * 8;
/// Bytes in the node statistics record.
pub const STATISTICS_RECORD_BYTE_COUNT: usize = STATISTICS_RECORD_WORD_COUNT * 8;
/// Maximum actor records returned by one census call.
pub const MAXIMUM_CENSUS_RECORDS_PER_CALL: usize = 8;
/// Maximum bytes written by one batched census call.
pub const CENSUS_BATCH_BYTE_COUNT: usize =
    MAXIMUM_CENSUS_RECORDS_PER_CALL * ACTOR_RECORD_BYTE_COUNT;

/// Actor record word offsets: version, packed address, state, parent-present,
/// packed parent, inbox messages, inbox charged bytes, and inbox byte budget.
pub type ActorRecord = [u64; ACTOR_RECORD_WORD_COUNT];
/// Core record word offsets: version, index, online, queued actors, timer ticks,
/// actor dispatches, steal scans, actors stolen, migrations, idle dispatches.
pub type CoreRecord = [u64; CORE_RECORD_WORD_COUNT];
/// Statistics word offsets: version, total frames, free frames, pool capacity,
/// allocated messages, charged message bytes, online cores, actor capacity,
/// occupied actors, and waiting actors. Physical frames are 4096 bytes.
pub type StatisticsRecord = [u64; STATISTICS_RECORD_WORD_COUNT];

/// Stable state numbers used in actor records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum State {
    /// Unoccupied actor slot.
    Free = 0,
    /// Eligible for scheduling.
    Runnable = 1,
    /// Executing on a core.
    Running = 2,
    /// Waiting for inbox progress.
    Waiting = 3,
    /// Stopped and awaiting reclamation.
    Exiting = 4,
}

/// Encodes words without relying on native alignment, padding, or endianness.
/// Returns false without modifying output when its length is incorrect.
pub fn encode(words: &[u64], output: &mut [u8]) -> bool {
    if words.len().checked_mul(8) != Some(output.len()) {
        return false;
    }
    for (word, bytes) in words.iter().zip(output.chunks_exact_mut(8)) {
        bytes.copy_from_slice(&word.to_le_bytes());
    }
    true
}

/// Decodes a complete, versioned observation record.
#[must_use]
pub fn decode<const WORD_COUNT: usize>(bytes: &[u8]) -> Option<[u64; WORD_COUNT]> {
    if WORD_COUNT.checked_mul(8)? != bytes.len() {
        return None;
    }
    let mut words = [0; WORD_COUNT];
    for (word, bytes) in words.iter_mut().zip(bytes.chunks_exact(8)) {
        *word = u64::from_le_bytes(bytes.try_into().ok()?);
    }
    (words.first().copied() == Some(RECORD_VERSION)).then_some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_little_endian_and_versioned() {
        let words = [RECORD_VERSION, 0x0102_0304_0506_0708];
        let mut bytes = [0; 16];
        assert!(encode(&words, &mut bytes));
        assert_eq!(bytes.get(8..), Some(&[8, 7, 6, 5, 4, 3, 2, 1][..]));
        assert_eq!(decode::<2>(&bytes), Some(words));
        bytes[0] = 2;
        assert_eq!(decode::<2>(&bytes), None);
    }

    #[test]
    fn malformed_lengths_do_not_partially_write() {
        let mut bytes = [0x55; 7];
        assert!(!encode(&[RECORD_VERSION], &mut bytes));
        assert_eq!(bytes, [0x55; 7]);
        assert_eq!(decode::<1>(&bytes), None);
        assert_eq!(decode::<0>(&[]), None);
    }
}
