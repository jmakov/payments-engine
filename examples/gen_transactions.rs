//! Generates a large, reproducible transaction CSV for measuring the engine.
//!
//! The README's performance table is produced with this tool, so anyone can re-run the exact
//! measurement:
//!
//! ```sh
//! cargo run --release --example gen_transactions -- 1000000 42 > /tmp/1m.csv
//! cargo build --release
//! /usr/bin/time -v target/release/payments-engine /tmp/1m.csv > /dev/null
//! ```
//!
//! Arguments: number of rows, then the RNG seed. The same arguments always produce the same
//! file. The mix is [`CLIENTS`] clients with 60% deposits, 25% withdrawals and 15% dispute
//! actions against earlier deposits (mostly disputes and resolves, rarely chargebacks, so most
//! accounts stay unlocked and keep transacting).

use std::env;
use std::io;
use std::io::Write;

use eyre::WrapErr;

/// Distinct client ids used; enough that the account map is not trivially small.
const CLIENTS: u64 = 10_000;

/// Largest generated amount in ticks (0.0001 units): 100.0000.
const MAX_AMOUNT_TICKS: u64 = 1_000_000;

/// Ticks per whole unit, for formatting amounts with four decimal places.
const TICKS_PER_UNIT: u64 = 10_000;

/// Reads the arguments and writes the CSV to stdout.
fn main() -> eyre::Result<()> {
    let (rows, seed) = parse_args(env::args().skip(1))?;
    let stdout = io::stdout().lock();
    let mut output = io::BufWriter::new(stdout);
    write_transactions(&mut output, rows, seed).wrap_err("failed to write transactions")?;
    output.flush().wrap_err("failed to flush transactions")
}

/// Parses `<rows> <seed>`.
///
/// # Errors
///
/// Returns a usage message unless there are exactly two unsigned integers, and refuses more
/// rows than there are `u32` transaction ids.
fn parse_args(mut args: impl Iterator<Item = String>) -> eyre::Result<(u32, u64)> {
    let usage = "usage: gen_transactions <rows> <seed>";
    let (Some(rows), Some(seed), None) = (args.next(), args.next(), args.next()) else {
        eyre::bail!(usage);
    };
    let rows = rows
        .parse()
        .wrap_err_with(|| format!("rows must fit a u32 transaction id; {usage}"))?;
    let seed = seed.parse().wrap_err(usage)?;
    Ok((rows, seed))
}

/// Writes the header and `rows` transactions drawn from a generator seeded with `seed`.
///
/// # Errors
///
/// Returns any error from writing to `output`.
fn write_transactions(output: &mut impl Write, rows: u32, seed: u64) -> io::Result<()> {
    let mut rng = SplitMix64(seed);
    // (tx, client) of every deposit so far, so dispute actions reference real deposits.
    let mut deposits: Vec<(u32, u64)> = Vec::new();

    writeln!(output, "type,client,tx,amount")?;
    for tx in 1..=rows {
        let roll = rng.below(100);
        if roll >= 85 && !deposits.is_empty() {
            let count = u64::try_from(deposits.len()).expect("a Vec length fits u64");
            let index = usize::try_from(rng.below(count))
                .expect("index is below a Vec length, which fits usize");
            let (deposit_tx, client) = deposits[index];
            let kind = match rng.below(1_000) {
                0..5 => "chargeback",
                5..400 => "resolve",
                _ => "dispute",
            };
            writeln!(output, "{kind},{client},{deposit_tx},")?;
            continue;
        }
        let client = rng.below(CLIENTS);
        let ticks = 1 + rng.below(MAX_AMOUNT_TICKS);
        let (whole, fraction) = (ticks / TICKS_PER_UNIT, ticks % TICKS_PER_UNIT);
        let kind = if roll < 60 { "deposit" } else { "withdrawal" };
        if kind == "deposit" {
            deposits.push((tx, client));
        }
        writeln!(output, "{kind},{client},{tx},{whole}.{fraction:04}")?;
    }
    Ok(())
}

/// `SplitMix64`: a tiny, well-mixed, seedable generator, so the tool needs no `rand` dependency.
///
/// Not cryptographic; it only has to be fast and reproducible.
struct SplitMix64(u64);

impl SplitMix64 {
    /// Returns the next 64 random bits.
    fn next(&mut self) -> u64 {
        // Constants from the reference implementation (Steele, Lea and Flood, 2014).
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^ (mixed >> 31)
    }

    /// Returns a value in `0..bound`; the modulo bias is negligible for the small bounds used.
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

#[cfg(test)]
mod tests {
    fn generate(rows: u32, seed: u64) -> String {
        let mut output = Vec::new();
        super::write_transactions(&mut output, rows, seed).expect("writing to a Vec succeeds");
        String::from_utf8(output).expect("generator writes ASCII")
    }

    #[test]
    fn same_seed_gives_same_file() {
        assert_eq!(generate(1_000, 7), generate(1_000, 7));
        assert_ne!(generate(1_000, 7), generate(1_000, 8));
    }

    #[test]
    fn every_generated_row_is_well_formed() {
        let csv = generate(10_000, 42);
        let mut reader = csv::ReaderBuilder::new()
            .flexible(true)
            .from_reader(csv.as_bytes());
        let header = reader.byte_headers().expect("in-memory read").clone();

        let rows: Vec<_> = reader
            .byte_records()
            .map(|record| {
                payments_engine::transaction::parse(&record.expect("in-memory read"), &header)
            })
            .collect();

        assert_eq!(rows.len(), 10_000);
        assert!(
            rows.iter().all(Result::is_ok),
            "a generated row failed to parse"
        );
    }
}
