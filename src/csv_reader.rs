//! Streams CSV records into validated transactions for the engine worker.

use std::io;

use eyre::Context;

use crate::transaction;

/// CSV reader buffer, set to 1MiB
const READER_BUFFER_SIZE: usize = 1024 * 1024;
/// Number of items we want to send in one iteration to the queue consumer.
const BATCH_SIZE: usize = 1024;

/// Reads a file in order and sends complete transaction batches to the consumer.
///
/// # Errors
///
/// Returns input, CSV or transaction parsing errors, an undersized queue, or a stopped consumer.
pub(crate) fn run(
    input: impl io::Read,
    queue_tx: &mut rtrb::Producer<transaction::Transaction>,
) -> eyre::Result<()> {
    tracing::debug!("CSV reader started");

    // A full batch always fits, so send_batch() can't wait forever, and there is room for
    // a second batch, so the two threads overlap instead of taking turns.
    eyre::ensure!(
        BATCH_SIZE * 2 <= queue_tx.buffer().capacity(),
        "Queue capacity ({}) must be at least twice the batch size ({BATCH_SIZE})",
        queue_tx.buffer().capacity()
    );

    let mut reader = csv::ReaderBuilder::new()
        .buffer_capacity(READER_BUFFER_SIZE)
        .trim(csv::Trim::All)
        // Partner actions may omit amount entirely, producing three fields instead of four.
        .flexible(true)
        .from_reader(input);
    let header = reader
        .byte_headers()
        .wrap_err("Failed to read the header row")?
        .clone();
    let mut record = csv::ByteRecord::new();
    let mut batch = Vec::with_capacity(BATCH_SIZE);

    while reader
        .read_byte_record(&mut record)
        .wrap_err("Failed to read transactions")?
    {
        let tx = transaction::parse(&record, &header).inspect_err(|error| {
            let line = record.position().map(csv::Position::line);
            tracing::error!(?line, %error, "invalid transaction row");
        })?;
        tracing::trace!(client = %tx.client(), tx = %tx.tx(), "transaction parsed");
        batch.push(tx);

        if batch.len() == BATCH_SIZE {
            send_batch(queue_tx, &mut batch)?;
        }
    }

    // Send what's left.
    if !batch.is_empty() {
        send_batch(queue_tx, &mut batch)?;
    }

    tracing::debug!("CSV reader finished");
    Ok(())
}

/// Moves the whole batch into the queue with a single publish, waiting for room if needed.
/// `batch.len()` is at most `BATCH_SIZE`, which `run` checked fits the queue.
fn send_batch(
    queue_tx: &mut rtrb::Producer<transaction::Transaction>,
    batch: &mut Vec<transaction::Transaction>,
) -> eyre::Result<()> {
    loop {
        if queue_tx.is_abandoned() {
            eyre::bail!("Queue consumer stopped, exiting.");
        }

        match queue_tx.write_chunk_uninit(batch.len()) {
            Ok(chunk) => {
                tracing::trace!(count = batch.len(), "sending transaction batch");
                chunk.fill_from_iter(batch.drain(..));
                return Ok(());
            }
            // Busy-wait without yielding to the OS scheduler.
            Err(_) => std::hint::spin_loop(),
        }
    }
}
