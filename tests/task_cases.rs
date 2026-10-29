//! Verifies every intermediate balance, including transient holds.

use payments_engine::{engine, transaction};

/// Each row's acceptance, available ticks, held ticks and locked flag, calculated by hand.
/// A final report alone would miss a dispute and resolve that both incorrectly did nothing.
const EXPECTED: [(bool, i64, i64, bool); 42] = [
    // Clients 1 and 2: the task's deposit/withdrawal example.
    (true, 20_000, 0, false),
    (true, 10_000, 0, false),
    (true, 30_000, 0, false),
    (true, 15_000, 0, false),
    (false, 20_000, 0, false),
    // Client 3: exact four-place arithmetic, an outstanding hold, insufficient available.
    (true, 101_234, 0, false),
    (true, 121_234, 0, false),
    (true, 120_000, 0, false),
    (true, 18_766, 101_234, false),
    (false, 18_766, 101_234, false),
    // Client 4: dispute and resolve preserve total and restore available funds.
    (true, 78_901, 0, false),
    (true, 0, 78_901, false),
    (true, 78_901, 0, false),
    (true, 78_900, 0, false),
    // Client 5: chargeback immediately locks an account that still holds other funds.
    (true, 82_500, 0, false),
    (true, 102_500, 0, false),
    (true, 20_000, 82_500, false),
    (true, 20_000, 0, true),
    (false, 20_000, 0, true),
    (false, 20_000, 0, true),
    // Client 6: withdrawing a deposit before reversing it leaves a locked debt.
    (true, 100_000, 0, false),
    (true, 0, 0, false),
    (true, -100_000, 100_000, false),
    (true, -100_000, 0, true),
    // Client 7: unknown references and undisputed resolutions/chargebacks change nothing.
    (true, 45_678, 0, false),
    (false, 45_678, 0, false),
    (false, 45_678, 0, false),
    (false, 45_678, 0, false),
    (false, 45_678, 0, false),
    (false, 45_678, 0, false),
    (true, 40_000, 0, false),
    // Client 8: resolved transactions are no longer disputed; a new dispute may charge back.
    (true, 50_000, 0, false),
    (true, 0, 50_000, false),
    (true, 50_000, 0, false),
    (false, 50_000, 0, false),
    (false, 50_000, 0, false),
    (true, 0, 50_000, false),
    (true, 0, 0, true),
    // Minimum/maximum client and transaction IDs, one tick, whitespace and unsorted IDs.
    (true, 1, 0, false),
    (true, 0, 0, false),
    (true, 1_234_567, 0, false),
    (true, 1_234_566, 0, false),
];

#[test]
fn task_cases_preserve_every_intermediate_balance() {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .flexible(true)
        .from_reader(include_bytes!("../examples/test_data.csv").as_slice());
    let header = reader.byte_headers().expect("fixture header").clone();
    let records: Vec<_> = reader
        .byte_records()
        .collect::<Result<_, _>>()
        .expect("fixture records");
    assert_eq!(records.len(), EXPECTED.len());
    let mut engine = engine::Engine::new();

    for (index, (record, &(accepted, available, held, locked))) in
        records.iter().zip(&EXPECTED).enumerate()
    {
        let transaction = transaction::parse(record, &header).expect("valid task transaction");

        let result = engine.apply(transaction);

        assert_eq!(result.is_ok(), accepted, "transaction {index}: {result:?}");
        let account = engine
            .account(transaction.client())
            .expect("client has a deposit");
        assert_eq!(
            (
                account.available().ticks(),
                account.held().ticks(),
                account.total().ticks(),
                account.is_locked()
            ),
            (available, held, available + held, locked),
            "transaction {index}: {transaction:?}",
        );
    }
    assert_eq!(engine.accounts().count(), 10);
}
