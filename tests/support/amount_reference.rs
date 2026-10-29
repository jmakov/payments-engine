//! Independent amount oracle for property tests and fuzzing, using standard integer parsers.

/// Returns positive ten-thousandths for plain decimal text within the input contract.
///
/// Whole units and fractional ticks are parsed separately with the standard library. This
/// deliberately avoids the production decimal library and its mantissa/scale conversion.
pub fn ticks(text: &[u8]) -> Option<i64> {
    if text.len() > 64 {
        return None;
    }
    let text = std::str::from_utf8(text).ok()?;
    let unsigned = text.strip_prefix('+').unwrap_or(text);
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if (whole.is_empty() && fraction.is_empty())
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let fraction = fraction.trim_end_matches('0');
    if fraction.len() > 4 {
        return None;
    }
    let units = if whole.is_empty() {
        0
    } else {
        whole.parse::<i128>().ok()?
    };
    let fractional_ticks = format!("{fraction:0<4}").parse::<i128>().ok()?;
    let ticks = units.checked_mul(10_000)?.checked_add(fractional_ticks)?;
    i64::try_from(ticks).ok().filter(|&ticks| ticks > 0)
}
