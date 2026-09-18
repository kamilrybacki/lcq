//! CRC-32 (IEEE 802.3), for detecting a record that was never finished.
//!
//! This guards against torn writes and bit rot, not against tampering. Anyone
//! who can rewrite the journal can recompute the checksum with it; what stops
//! them mattering is that the frames inside are signed and sealed, and that the
//! journal never leaves the node.

const POLYNOMIAL: u32 = 0xEDB8_8320;

/// The lookup table, built at compile time so nothing is computed at startup.
static TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut index = 0usize;
    while index < 256 {
        #[allow(clippy::cast_possible_truncation)]
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 {
                (value >> 1) ^ POLYNOMIAL
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
};

/// Checksum of a record's payload.
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in bytes {
        let index = ((crc ^ u32::from(*byte)) & 0xFF) as usize;
        crc = (crc >> 8) ^ TABLE[index];
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::crc32;

    #[test]
    fn matches_the_published_check_value() {
        // The standard check value for CRC-32/ISO-HDLC.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn the_empty_input_checksums_to_zero() {
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn a_single_flipped_bit_changes_the_checksum() {
        assert_ne!(crc32(b"lorai"), crc32(b"lorbi"));
    }
}
