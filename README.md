# payments-engine

A toy payments engine. It streams a CSV of deposits, withdrawals, disputes, resolves and
chargebacks, applies them in file order, and prints every client's final balances as CSV.

## Dependencies

- Rust 1.99.1 or newer, Cargo and a native linker. Install Rust using [rustup](https://rustup.rs/);
- For the test and lint commands below, install [cargo-nextest](https://nexte.st/docs/installation/),
  Clippy and rustfmt:

  ```sh
  rustup toolchain install stable
  rustup component add clippy rustfmt
  cargo +stable install cargo-nextest --locked
  ```
  
- Fuzzing additionally requires nightly Rust, a C++11 compiler and `cargo-fuzz`; see
  [Fuzzing](#fuzzing).

## Usage

```sh
cargo run -- examples/test_data.csv > accounts.csv
```

- Input: one CSV file with the header `type, client, tx, amount`. Whitespace around fields is
  ignored; dispute, resolve and chargeback rows may leave `amount` empty or out.
- Output on stdout: `client,available,held,total,locked`, one row per client, sorted by client
  id, trailing zeros dropped (`1.5`, `2`, `0`).
- Structured diagnostics go to stderr through `tracing`. The default `info` level reports
  processing progress and accepted/rejected counts. `RUST_LOG=debug` adds transaction
  outcomes; `RUST_LOG=trace` adds parsing, batches and state transitions. Standard filter
  directives work too, for example `RUST_LOG=warn,payments_engine::account_writer=debug`.
  `RUST_LOG=off` disables logs. Diagnostics can include parsed client and transaction IDs,
  but not raw field text or monetary amounts.
- Invalid arguments, malformed rows and I/O failures return a non-zero exit status; expected
  business rejections are logged at debug level and processing continues.

## Design
```
CSV rows ──> csv_reader ──> bounded queue ──> engine ──> account_writer ──> CSV report
             thread_1 (parse/validate)                  thread_2 (TX processing state machine)
```
- Uses type system to model valid states and state machine to allow only valid account states.
- Can process input files larger than memory
- CSV reader and TX processing engine running in own threads

### Scaling strategy

- Vertically we can scale by having multiple producers parsing the stream and forwarding parsed TX via MPSC queue to
  the TX processing engine.
- For horizontal scaling one could shard by client ID and require that a client ID is tied to a dedicated stream (as it
  is the case with e.g. connections to financial exchanges). By doing this, we can run multiple TX processing engines. 

### Guarantees

- Money is exact. Amounts have at most 4 decimal places, so all money is an `i64` count of
  ticks, each worth 0.0001. Balances change only through checked integer arithmetic. Every result is exact, the input 
  fails validation, or the transaction is refused; nothing is ever rounded.
- The library denies `clippy::arithmetic_side_effects` and `clippy::float_arithmetic` (clippy
  exempts test code).
- Transaction shapes are type-checked. Each `Action` variant carries exactly its data:
  a deposit always holds a validated `Amount` (private field; a `compile_fail` doctest checks
  it), a dispute holds none. `ClientId(u16)` and `TxId(u32)` are distinct newtypes. Whether a
  transaction is allowed for the current account and deposit state is checked at runtime.
- `total == available + held` always: `total` is computed, never stored.
- A refused transaction changes nothing, not even by creating an empty account.
- Operations require typed state permissions. The engine first validates the runtime
  state and borrows an `OpenAccount`, `Settled` deposit or `Disputed` deposit. Only
  `OpenAccount` has deposit/withdrawal methods, only `Settled` has `dispute`, and only
  `Disputed` has resolve/chargeback methods. Operations consume these exclusive borrows;
  they cannot be copied, reused or coexist with another mutation of the same record.
  Private fields and balance methods prevent the engine from bypassing those transitions.
- The dispute lifecycle is explicit:

```
          dispute              chargeback
Settled ----------> Disputed -------------> ChargedBack (final, account locked)
   ^                   |
   +----- resolve -----+
```

Account state is `Open -> Locked` on chargeback, with no unlock transition. Locked accounts
still accept partner dispute actions on earlier deposits. Runtime checks remain necessary
for CSV-selected ids, ownership, current states, sufficient funds and integer overflow.
Types constrain legal operations after those checks; they do not statically prove balances
across the two maps. Checked arithmetic, exclusive access and an infallible final commit
preserve that relationship, covered by transition regressions and the reference-model tests.

### Assumptions

Where the spec is silent, these bank-like rules apply:

1. Only deposits can be disputed. A dispute moves funds from available to held, which only
   reverses a credit; the spec describes no flow for withdrawals. Withdrawals are not stored,
   and a dispute naming one is ignored like an unknown id.
2. Dispute actions must name the deposit's owner; otherwise one client could freeze
   another client's funds.
3. A dispute applies even if it drives `available` negative. That is the fraud the spec
   describes (deposit, withdraw, reverse the deposit); after the chargeback the account shows
   what the client owes and is locked.
4. A locked account refuses deposits and withdrawals but still processes disputes,
   resolves and chargebacks. Those come from the partner, not the client, and must still
   settle earlier deposits: a fraudster with two charged-back deposits owes both. In
   production the refused rows would go to manual review.
5. A resolved deposit can be disputed again; a chargeback is final.
6. Duplicate dispute actions are ignored: disputing a disputed deposit, or resolving or
   charging back one that is not disputed.
7. Deposit and withdrawal amounts must be positive, with at most 4 significant decimal
   places, and at most `922337203685477.5807`, the largest `i64` of ticks; otherwise the row
   is malformed. The type gives the direction, so a negative amount never reverses it. A
   deposit that would push a balance past the range is refused as an overflow.
8. Amounts on dispute actions are ignored, even when they are not numbers: junk in an
   unused column must not drop a chargeback.
9. A deposit that reuses an earlier deposit's tx id is rejected. Withdrawal ids are not
   stored, so reusing one of those goes undetected; the spec guarantees unique ids anyway.
10. A withdrawal larger than the available funds is rejected; held funds cannot be
    withdrawn.
11. An account is created by its client's first accepted transaction. The spec's "create
    the client" is read that way: a rejected or malformed row changes nothing, so it adds no
    `0,0,0,false` row for a client who never held money.
12. Malformed rows stop processing and are logged with their line number and reason.
    Expected business rejections are ignored after a debug event; later rows still run.
13. Well-formed means: Serde matches columns by header name in any order; every row needs
    `type`, `client` and `tx`, and deposits and withdrawals need `amount`. Duplicate known
    columns are rejected. The CSV reader trims ASCII whitespace; `type` is lowercase and IDs
    are unsigned integers in range. Amounts are valid decimal text with up to four fractional
    places, as guaranteed by the task. `rust_decimal` parses amounts; positivity, precision and
    range are checked separately. Dispute actions ignore their amount, even when it is missing
    or contains non-UTF-8 bytes. Extra columns are ignored.

## Testing

`examples/test_data.csv` covers the transaction and edge cases. The independently calculated final balances
are in `examples/test_data.expected.csv`; [the coverage notes](docs/test_data.md) explain each
case. Verify with:

```sh
cargo nextest run          # unit + integration tests (or: cargo test --all-targets)
cargo test --doc           # doctests incl. compile_fail checks; nextest skips these
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

### Fuzzing

The separate `fuzz/` package uses `cargo-fuzz` and libFuzzer. Install a C++11 compiler and
the system build tools for your distribution:

```sh
# Ubuntu / Debian
sudo apt-get update
sudo apt-get install -y build-essential

# Fedora
sudo dnf install -y gcc-c++

# Alpine
# Alpine's default static musl setup conflicts with AddressSanitizer. Disabling the sanitizer
# still exercises the targets but does not detect the memory errors that AddressSanitizer would;
# Use a GNU/Linux environment for the default sanitizer-enabled runs.
apk add --no-cache build-base curl ca-certificates git
```

Then install nightly Rust and `cargo-fuzz`:

```sh
rustup toolchain install nightly
cargo +stable install cargo-fuzz --locked
```

Optionally seed the CSV targets with the checked-in example:

```sh
mkdir -p fuzz/corpus/run_csv fuzz/corpus/parse_csv
cp examples/test_data.csv fuzz/corpus/run_csv/test_data.csv
cp examples/test_data.csv fuzz/corpus/parse_csv/test_data.csv
```

Run the fuzz targets with:

```sh
cargo +nightly fuzz run run_csv -- -max_total_time=60
cargo +nightly fuzz run parse_csv -- -max_total_time=60
cargo +nightly fuzz run amount_text -- -max_total_time=60
cargo +nightly fuzz run transaction_sequence -- -max_total_time=60
```
`run_csv` checks the full pipeline and its balance
invariant;

`parse_csv` exercises parsing directly and checks transaction round trips.

`amount_text` checks positivity and compares valid amounts with an independent integer oracle.

`transaction_sequence` uses `arbitrary` to generate typed operations with small, full-width and
maximum IDs, and checks each step against the same independent model as the property tests.

If a target finds a failure, reproduce it with `cargo +nightly fuzz run <target> <artifact-path>`,
then add a deterministic regression test.
On Alpine, add `--sanitizer none` to each `cargo fuzz run` command, for example:

```sh
cargo +nightly fuzz run --sanitizer none parse_csv -- -max_total_time=60
```


