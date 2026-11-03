//! Shared ASN.1/DER encoding and decoding primitives.
//!
//! These low-level helpers are used by both `spnego.rs` and `kerberos/messages.rs`
//! to build and parse DER-encoded structures. Only the core TLV operations live
//! here; type-specific helpers (INTEGER, GeneralString, etc.) stay in their
//! respective modules.

use crate::Error;

/// Encode a DER length field (X.690 § 8.1.3).
///
/// - Lengths < 128 are encoded as a single byte.
/// - Anything longer is `0x80 | n` followed by the length in `n` big-endian
///   bytes, as few as it takes. Every `usize` fits, so this can't fail: a
///   large-AD Kerberos token past 64 KiB gets `0x83`.
pub(crate) fn der_length(len: usize) -> Vec<u8> {
    if len < 128 {
        return vec![len as u8];
    }
    let bytes = len.to_be_bytes();
    let significant = &bytes[len.leading_zeros() as usize / 8..];
    let mut out = Vec::with_capacity(1 + significant.len());
    out.push(0x80 | significant.len() as u8);
    out.extend_from_slice(significant);
    out
}

/// Wrap data in a DER TLV (tag-length-value).
pub(crate) fn der_tlv(tag: u8, data: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend_from_slice(&der_length(data.len()));
    out.extend_from_slice(data);
    out
}

/// Parse a DER length field, returning `(length, bytes_consumed)`.
pub(crate) fn parse_der_length(data: &[u8]) -> Result<(usize, usize), Error> {
    if data.is_empty() {
        return Err(Error::invalid_data("DER: truncated length"));
    }
    let first = data[0];
    if first < 128 {
        Ok((first as usize, 1))
    } else if (0x81..=0x84).contains(&first) {
        // Long form: the low bits say how many length bytes follow. Four is
        // the most any token this crate handles could need (4 GiB).
        let n = (first & 0x7f) as usize;
        if data.len() < 1 + n {
            return Err(Error::invalid_data(format!(
                "DER: truncated length (0x{first:02x})"
            )));
        }
        let len = data[1..=n]
            .iter()
            .fold(0usize, |acc, &b| (acc << 8) | b as usize);
        Ok((len, 1 + n))
    } else {
        Err(Error::invalid_data(format!(
            "DER: unsupported length encoding: 0x{first:02x}"
        )))
    }
}

/// Parse a DER TLV, returning `(tag, value_slice, total_bytes_consumed)`.
pub(crate) fn parse_der_tlv(data: &[u8]) -> Result<(u8, &[u8], usize), Error> {
    if data.is_empty() {
        return Err(Error::invalid_data("DER: truncated TLV"));
    }
    let tag = data[0];
    let (len, len_bytes) = parse_der_length(&data[1..])?;
    let header_len = 1 + len_bytes;
    let total = header_len.saturating_add(len);
    if data.len() < total {
        return Err(Error::invalid_data(format!(
            "DER: TLV truncated: need {total} bytes, have {}",
            data.len()
        )));
    }
    Ok((tag, &data[header_len..total], total))
}

#[cfg(test)]
mod tests {
    use super::*;

    // =======================================================================
    // DER length encoding
    // =======================================================================

    #[test]
    fn length_single_byte() {
        assert_eq!(der_length(0), vec![0x00]);
        assert_eq!(der_length(1), vec![0x01]);
        assert_eq!(der_length(127), vec![0x7f]);
    }

    #[test]
    fn length_two_byte() {
        assert_eq!(der_length(128), vec![0x81, 0x80]);
        assert_eq!(der_length(255), vec![0x81, 0xff]);
    }

    #[test]
    fn length_three_byte() {
        assert_eq!(der_length(256), vec![0x82, 0x01, 0x00]);
        assert_eq!(der_length(65535), vec![0x82, 0xff, 0xff]);
        assert_eq!(der_length(1000), vec![0x82, 0x03, 0xe8]);
    }

    /// A Kerberos AP-REQ carries the service ticket and its PAC, which grows
    /// with group and claim count, so a large-AD token can pass 64 KiB. The
    /// encoder used to drop the high bits there and declare a much shorter
    /// value (#6: `der_length(70000)` came out as 4,464).
    #[test]
    fn length_past_64k_keeps_every_byte() {
        assert_eq!(der_length(65536), vec![0x83, 0x01, 0x00, 0x00]);
        assert_eq!(der_length(70000), vec![0x83, 0x01, 0x11, 0x70]);
        assert_eq!(der_length(0x0100_0000), vec![0x84, 0x01, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn every_length_round_trips_through_the_parser() {
        for len in [
            0,
            127,
            128,
            255,
            256,
            65535,
            65536,
            70000,
            0xFF_FFFF,
            0x0100_0000,
        ] {
            let encoded = der_length(len);
            assert_eq!(
                parse_der_length(&encoded).unwrap(),
                (len, encoded.len()),
                "length {len}"
            );
        }
    }

    // =======================================================================
    // DER TLV encoding
    // =======================================================================

    #[test]
    fn tlv_simple() {
        let result = der_tlv(0x04, &[0x01, 0x02]);
        assert_eq!(result, vec![0x04, 0x02, 0x01, 0x02]);
    }

    #[test]
    fn tlv_empty() {
        let result = der_tlv(0x30, &[]);
        assert_eq!(result, vec![0x30, 0x00]);
    }

    #[test]
    fn tlv_long_content() {
        let data = vec![0xaa; 200];
        let result = der_tlv(0x04, &data);
        assert_eq!(result[0], 0x04);
        assert_eq!(result[1], 0x81);
        assert_eq!(result[2], 200);
        assert_eq!(result.len(), 3 + 200);
    }

    // =======================================================================
    // DER length parsing
    // =======================================================================

    #[test]
    fn parse_length_single_byte() {
        let (len, consumed) = parse_der_length(&[0x05]).unwrap();
        assert_eq!(len, 5);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn parse_length_two_byte() {
        let (len, consumed) = parse_der_length(&[0x81, 0x80]).unwrap();
        assert_eq!(len, 128);
        assert_eq!(consumed, 2);
    }

    #[test]
    fn parse_length_three_byte() {
        let (len, consumed) = parse_der_length(&[0x82, 0x01, 0x00]).unwrap();
        assert_eq!(len, 256);
        assert_eq!(consumed, 3);
    }

    #[test]
    fn parse_length_four_byte() {
        let (len, consumed) = parse_der_length(&[0x83, 0x01, 0x00, 0x00]).unwrap();
        assert_eq!(len, 65536);
        assert_eq!(consumed, 4);
    }

    #[test]
    fn parse_length_truncated() {
        assert!(parse_der_length(&[]).is_err());
        assert!(parse_der_length(&[0x81]).is_err());
        assert!(parse_der_length(&[0x82, 0x01]).is_err());
        assert!(parse_der_length(&[0x83, 0x01, 0x00]).is_err());
    }

    // =======================================================================
    // DER TLV parsing
    // =======================================================================

    #[test]
    fn parse_tlv_roundtrip() {
        let original = der_tlv(0x04, &[0xde, 0xad, 0xbe, 0xef]);
        let (tag, value, total) = parse_der_tlv(&original).unwrap();
        assert_eq!(tag, 0x04);
        assert_eq!(value, &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(total, original.len());
    }

    #[test]
    fn parse_tlv_truncated() {
        assert!(parse_der_tlv(&[]).is_err());
        // Tag present, length says 10 bytes but only 2 available
        assert!(parse_der_tlv(&[0x04, 0x0a, 0x01, 0x02]).is_err());
    }
}
