//! Feeds arbitrary CSV bytes through the same two-worker pipeline used by the CLI.
//!
//! Whether invalid input returns an error or a partial report, every report row must satisfy
//! `total == available + held`; the parser and workers must never panic.
#![no_main]

use std::str::FromStr;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let mut report = Vec::new();
    let _result = payments_engine::run(data, &mut report);
    let report = String::from_utf8(report).expect("the report is UTF-8");
    let mut lines = report.lines();
    assert_eq!(lines.next(), Some("client,available,held,total,locked"));
    for line in lines {
        let fields: Vec<&str> = line.split(',').collect();
        let [_, available, held, total, _] = fields[..] else {
            panic!("report row should have 5 fields: {line:?}");
        };
        let decimal = |text: &str| {
            rust_decimal::Decimal::from_str(text).expect("report amounts are decimals")
        };
        assert_eq!(
            decimal(available).checked_add(decimal(held)),
            Some(decimal(total)),
            "{line:?}"
        );
    }
});
