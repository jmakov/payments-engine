//! Fuzz amount parsing and compare task-valid inputs with an independent integer oracle.
//!
//! Arbitrary input must never produce a non-positive amount. Inputs within the task's decimal
//! precision and range must agree with the oracle; other syntax is left to the decimal library.

#![no_main]

#[path = "../../tests/support/amount_reference.rs"]
mod amount_reference;

libfuzzer_sys::fuzz_target!(|text: &[u8]| {
    let parsed = std::str::from_utf8(text)
        .ok()
        .and_then(|text| payments_engine::money::Amount::parse(text).ok())
        .map(|amount| amount.get().ticks());
    if let Some(ticks) = parsed {
        assert!(ticks > 0);
    }
    if text.len() > 20
        || text
            .split(|&byte| byte == b'.')
            .nth(1)
            .is_some_and(|f| f.len() > 4)
    {
        return;
    }
    if let Some(expected) = amount_reference::ticks(text) {
        assert_eq!(
            parsed,
            Some(expected),
            "{:?}",
            String::from_utf8_lossy(text)
        );
    }
});
