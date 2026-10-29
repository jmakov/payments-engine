//! Streaming payments engine with a CSV reader and transaction worker.
//!
//! [`run`] connects the two workers with a bounded queue. Transactions stay in input order;
//! the engine retains account balances and deposit states needed for later disputes. Money
//! uses exact fixed-point ticks, while typed permissions constrain state transitions.

#![deny(clippy::arithmetic_side_effects, clippy::float_arithmetic)]

pub mod account;
mod account_writer;
mod csv_reader;
pub mod engine;
pub mod money;
pub mod rejection;
pub mod transaction;

use std::io;
use std::thread;

/// Maximum number of transactions buffered between the reader and engine workers.
const QUEUE_SIZE: usize = 1024 * 1024;

/// Streams a transaction CSV through two workers and writes the final account report.
///
/// Input errors stop the reader; accepted business rejections are logged and skipped.
/// The consumer may write a partial report after an input error. Workers are joined before
/// returning so borrowed readers and writers may be used by callers.
///
/// # Examples
///
/// ```
/// # fn main() -> eyre::Result<()> {
/// let input = b"type,client,tx,amount\ndeposit,1,1,1.5\n";
/// let mut output = Vec::new();
/// payments_engine::run(input.as_slice(), &mut output)?;
/// assert_eq!(output, b"client,available,held,total,locked\n1,1.5,0,1.5,false\n");
/// # Ok(()) }
/// ```
///
/// # Errors
///
/// Returns CSV, transaction, thread, or output errors.
pub fn run(input: impl io::Read + Send, output: impl io::Write + Send) -> eyre::Result<()> {
    tracing::info!("processing transactions");

    let (mut queue_tx, mut queue_rx) = rtrb::RingBuffer::new(QUEUE_SIZE);

    // start producer and consumer on own threads
    thread::scope(|scope| {
        let producer = thread::Builder::new()
            .name("csv_reader".into())
            .spawn_scoped(scope, move || csv_reader::run(input, &mut queue_tx))?;
        let consumer = thread::Builder::new()
            .name("tx_engine".into())
            .spawn_scoped(scope, move || account_writer::run(&mut queue_rx, output))?;

        let producer_result = producer.join();
        let consumer_result = consumer.join();

        producer_result.unwrap_or_else(|_| Err(eyre::eyre!("CSV reader thread panicked")))?;
        consumer_result.unwrap_or_else(|_| Err(eyre::eyre!("TX engine thread panicked")))?;

        Ok::<_, eyre::Report>(())
    })?;

    tracing::info!("processing complete");
    Ok(())
}
