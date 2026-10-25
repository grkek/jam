//! Small helpers for turning values into bytes and back, without panicking.

/// Reads `N` bytes starting at `offset`, or `None` when the input is too short.
pub(crate) fn read_array<const N: usize>(source_bytes: &[u8], offset: usize) -> Option<[u8; N]> {
    source_bytes.get(offset..offset + N)?.try_into().ok()
}

/// Collects bytes into a fixed array. Missing bytes stay zero, surplus bytes are ignored.
pub(crate) fn collect_array<const N: usize>(source_bytes: impl IntoIterator<Item = u8>) -> [u8; N] {
    let mut array = [0_u8; N];
    for (destination, source) in array.iter_mut().zip(source_bytes) {
        *destination = source;
    }
    array
}
