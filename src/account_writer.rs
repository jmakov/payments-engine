//! Applies queued transactions and reports final balances after the producer finishes.
//!
//! Business rejections leave the engine unchanged and processing continues. Queue and output
//! failures stop the worker and are returned to the command-line caller.

use std::io;

use eyre::Context;

use crate::{engine, transaction};

/// Column names of the output CSV, in the order writes each row's values.
const OUTPUT_HEADER: [&str; 5] = ["client", "available", "held", "total", "locked"];
/// Number of items we want to get from the queue in one iteration.
const BATCH_SIZE: usize = 1024;
/// Empty queues are either temporarily idle or finished, depending on producer ownership.
const NO_TRANSACTIONS_AVAILABLE: usize = 0;

/// Drains transactions, skips rejected operations and writes the final account report.
///
/// # Errors
///
/// Returns queue read errors and failures writing or flushing the output. A refused transaction
/// is a normal business outcome and does not fail the stream.
pub(crate) fn run(
    queue_rx: &mut rtrb::Consumer<transaction::Transaction>,
    output: impl io::Write,
) -> eyre::Result<()> {
    tracing::debug!("transaction worker started");
    let mut engine = engine::Engine::new();
    let mut accepted = 0_u64;
    let mut rejected = 0_u64;

    loop {
        match queue_rx.slots().min(BATCH_SIZE) {
            // Producer finished and queue is empty.
            NO_TRANSACTIONS_AVAILABLE if queue_rx.is_abandoned() && queue_rx.is_empty() => break,
            // No transactions available yet.
            NO_TRANSACTIONS_AVAILABLE => {
                // Allow sibling logical CPU core to use more execution (the logical CPU core receives
                // PAUSE instruction and the CPU can use this info to reduce SMT contention),
                // don't yield to OS scheduler (avoid OS overhead).
                // N.B. this is only relevant if SMT (hyperthreading) is enabled in BIOS. For lowest
                // latency, we'd disable SMT, run on a physical core and let the OS scheduler find the
                // coolest core so it can turbo.
                std::hint::spin_loop();
            }
            // Process available transactions without allocating.
            available_transactions => {
                for tx in queue_rx.read_chunk(available_transactions)? {
                    if let Err(rejection) = engine.apply(tx) {
                        rejected = rejected.saturating_add(1);
                        tracing::debug!(
                            client = %tx.client(),
                            tx = %tx.tx(),
                            %rejection,
                            "transaction rejected"
                        );
                    } else {
                        accepted = accepted.saturating_add(1);
                        tracing::debug!(client = %tx.client(), tx = %tx.tx(), kind = tx.kind(), "transaction applied");
                    }
                }
            }
        }
    }

    tracing::info!(
        accepted,
        rejected,
        accounts = engine.accounts().count(),
        "transactions processed"
    );
    write_accounts(&engine, output).inspect_err(|error| {
        tracing::error!(%error, "account report failed");
    })?;
    Ok(())
}

/// Writes a deterministic account report; output errors propagate to the CLI.
fn write_accounts(engine: &engine::Engine, output: impl io::Write) -> eyre::Result<()> {
    let mut writer = csv::WriterBuilder::new()
        .has_headers(false)
        .from_writer(output);
    writer
        .write_record(OUTPUT_HEADER)
        .wrap_err("Failed to write output header")?;

    let mut accounts: Vec<_> = engine.accounts().collect();
    tracing::debug!(accounts = accounts.len(), "writing account report");
    accounts.sort_unstable_by_key(|(client, _)| *client);

    for (client, account) in accounts {
        let row = (
            client,
            account.available(),
            account.held(),
            account.total(),
            account.is_locked(),
        );
        writer
            .serialize(row)
            .wrap_err_with(|| format!("Failed to write account {client}"))?;
    }
    writer.flush().wrap_err("Failed to flush output")?;
    tracing::debug!("account report written");

    Ok(())
}
