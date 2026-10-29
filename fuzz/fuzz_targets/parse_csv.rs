//! Fuzzes CSV headers and rows directly, without starting the pipeline workers.
//!
//! Every successfully decoded transaction must survive a canonical CSV round trip.
#![no_main]

use payments_engine::transaction;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .flexible(true)
        .from_reader(data);
    let Ok(header) = reader.byte_headers().cloned() else {
        return;
    };
    let canonical_header = csv::ByteRecord::from(vec!["type", "client", "tx", "amount"]);
    let mut record = csv::ByteRecord::new();

    while let Ok(true) = reader.read_byte_record(&mut record) {
        if let Ok(parsed) = transaction::parse(&record, &header) {
            let mut canonical = csv::ByteRecord::new();
            canonical.push_field(parsed.kind().as_bytes());
            canonical.push_field(parsed.client().to_string().as_bytes());
            canonical.push_field(parsed.tx().to_string().as_bytes());
            match parsed.action() {
                transaction::Action::Deposit(amount) | transaction::Action::Withdrawal(amount) => {
                    canonical.push_field(amount.get().to_string().as_bytes());
                }
                transaction::Action::Dispute
                | transaction::Action::Resolve
                | transaction::Action::Chargeback => canonical.push_field(b""),
            }
            let reparsed = transaction::parse(&canonical, &canonical_header)
                .expect("canonical transaction should parse");
            assert_eq!(parsed, reparsed);
        }
    }
});
