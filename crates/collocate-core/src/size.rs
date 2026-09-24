use crate::{Error, Result};

pub fn parse_size(input: &str) -> Result<u64> {
    let bad = || Error::InvalidSize(input.to_string());
    let s = input.trim();
    if s.is_empty() {
        return Err(bad());
    }
    let bytes = s.as_bytes();
    let mut end = bytes.len();
    let mut shift = 0u32;
    if end > 0 && matches!(bytes[end - 1], b'B' | b'b') {
        end -= 1;
    }
    if end > 1 && matches!(bytes[end - 1], b'i' | b'I') && matches!(bytes[end - 2], b'k' | b'K' | b'm' | b'M' | b'g' | b'G' | b't' | b'T') {
        end -= 1;
    }
    if end > 0 && bytes[end - 1].is_ascii_alphabetic() {
        shift = match bytes[end - 1] {
            b'k' | b'K' => 10,
            b'm' | b'M' => 20,
            b'g' | b'G' => 30,
            b't' | b'T' => 40,
            _ => return Err(bad()),
        };
        end -= 1;
    }
    if end == 0 || !s[..end].bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: u64 = s[..end].parse().map_err(|_| bad())?;
    n.checked_mul(1u64 << shift).ok_or_else(bad)
}

pub fn parse_duration_secs(input: &str) -> Result<u64> {
    let bad = || Error::Invalid(format!("invalid duration {input:?}"));
    let s = input.trim();
    let (digits, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        Some('d') => (&s[..s.len() - 1], 86400),
        _ => (s, 1),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    digits.parse::<u64>().map_err(|_| bad())?.checked_mul(mult).ok_or_else(bad)
}
