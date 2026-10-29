//! Turns raw CSV rows into validated [`Transaction`] values.
//!
//! This module is the trust boundary of the crate. Rows arrive from a partner feed as loosely
//! typed text (`type, client, tx, amount`); everything after this module works with a closed
//! action enum whose variants carry exactly the data they need. For example, an [`Action::Deposit`]
//! cannot exist without a valid [`money::Amount`], and a [`Action::Dispute`] has no amount
//! field at all, so the [`crate::engine::Engine`] never has to handle "missing" or "unexpected"
//! values.
//!
//! [`parse`] uses Serde and the CSV header to deserialize each row. Amount bytes are borrowed
//! and parsed only for deposits and withdrawals; partner actions ignore that field.

use std::fmt;
use std::str;

use crate::money;

/// Identifies a client and their single asset account.
///
/// Wrapping the `u16` prevents mixing client ids with transaction ids at compile time.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct ClientId(u16);

impl ClientId {
    /// Wraps a raw client id. Every `u16` is a valid client id.
    #[must_use]
    pub const fn new(id: u16) -> Self {
        Self(id)
    }

    /// Returns the raw id.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Globally unique transaction id assigned by the partner.
///
/// Disputes, resolves and chargebacks use it to point back at the deposit they concern.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct TxId(u32);

impl TxId {
    /// Wraps a raw transaction id. Every `u32` is a valid transaction id.
    #[must_use]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Returns the raw id.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for TxId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// One validated instruction from the input feed.
///
/// Each transaction carries the client and transaction ID once. Its [`Action`] carries
/// a validated amount only for deposits and withdrawals.
///
/// # Examples
///
/// ```
/// use payments_engine::{money, transaction};
///
/// let deposit = transaction::Transaction::new(
///     transaction::ClientId::new(1),
///     transaction::TxId::new(7),
///     transaction::Action::Deposit(money::Amount::parse("2.5").expect("valid amount")),
/// );
/// assert_eq!(deposit.tx(), transaction::TxId::new(7));
/// ```
///
/// A deposit cannot carry an unvalidated balance:
///
/// ```compile_fail,E0308
/// use payments_engine::{money, transaction};
/// let action = transaction::Action::Deposit(money::Money::from_ticks(-10_000));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transaction {
    /// Client whose account the transaction affects.
    client: ClientId,
    /// Transaction ID or referenced deposit ID.
    tx: TxId,
    /// Validated operation to apply.
    action: Action,
}

/// Operation performed by a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Credits a validated amount to the account.
    Deposit(money::Amount),
    /// Debits a validated amount from the account.
    Withdrawal(money::Amount),
    /// Holds the referenced deposit pending a dispute.
    Dispute,
    /// Releases a disputed deposit.
    Resolve,
    /// Reverses a disputed deposit and locks its account.
    Chargeback,
}

impl Transaction {
    /// Creates an instruction with its client, ID, and validated action.
    ///
    /// # Arguments
    ///
    /// * `client` - account affected by the transaction.
    /// * `tx` - transaction ID or referenced deposit ID.
    /// * `action` - validated operation to apply.
    #[must_use]
    pub const fn new(client: ClientId, tx: TxId, action: Action) -> Self {
        Self { client, tx, action }
    }

    /// Returns the CSV transaction type for structured diagnostics.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self.action {
            Action::Deposit(_) => "deposit",
            Action::Withdrawal(_) => "withdrawal",
            Action::Dispute => "dispute",
            Action::Resolve => "resolve",
            Action::Chargeback => "chargeback",
        }
    }

    /// Returns the client this transaction applies to.
    #[must_use]
    pub const fn client(&self) -> ClientId {
        self.client
    }

    /// Returns the transaction ID or referenced deposit ID.
    #[must_use]
    pub const fn tx(&self) -> TxId {
        self.tx
    }

    /// Returns the validated action to apply.
    #[must_use]
    pub const fn action(&self) -> Action {
        self.action
    }
}

/// Deserializes a CSV record using its header and validates the required amount.
///
/// Configure the reader with `trim(csv::Trim::All)` and `flexible(true)`. Dispute actions
/// ignore the amount field, including invalid UTF-8 bytes.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use payments_engine::transaction;
/// let header = csv::ByteRecord::from(vec!["type", "client", "tx", "amount"]);
/// let row = csv::ByteRecord::from(vec!["deposit", "1", "2", "1.5"]);
/// let transaction = transaction::parse(&row, &header)?;
/// assert_eq!(transaction.client(), transaction::ClientId::new(1));
/// # Ok(())
/// # }
/// ```
///
/// # Arguments
///
/// * `record` - input row to deserialize.
/// * `header` - column names associated with the row.
///
/// # Errors
///
/// Returns an error if Serde cannot decode a required field or a money amount is invalid.
pub fn parse(
    record: &csv::ByteRecord,
    header: &csv::ByteRecord,
) -> Result<Transaction, ParseError> {
    // Serde's error text can contain the original field value. Retain its column index
    // without exposing untrusted input in diagnostics or the returned error.
    let row: CsvTransaction<'_> = record.deserialize(Some(header)).map_err(|error| {
        let column = if let csv::ErrorKind::Deserialize { err, .. } = error.kind() {
            err.field()
        } else {
            None
        };
        match column {
            Some(column) => ParseError::CsvColumn(column),
            None => ParseError::Csv,
        }
    })?;
    let amount = || match row.amount {
        Some(text) if !text.is_empty() => {
            let text = str::from_utf8(text).map_err(|_| money::AmountError::Malformed)?;
            Ok(money::Amount::parse(text)?)
        }
        _ => Err(ParseError::MissingAmount),
    };
    let action = match row.kind {
        Kind::Deposit => Action::Deposit(amount()?),
        Kind::Withdrawal => Action::Withdrawal(amount()?),
        Kind::Dispute => Action::Dispute,
        Kind::Resolve => Action::Resolve,
        Kind::Chargeback => Action::Chargeback,
    };
    Ok(Transaction::new(row.client, row.tx, action))
}

/// CSV fields before validating the amount required by deposits and withdrawals.
#[derive(Debug, serde::Deserialize)]
struct CsvTransaction<'row> {
    /// Input action selecting the domain transaction variant.
    #[serde(rename = "type")]
    kind: Kind,
    /// Client whose account the transaction affects.
    client: ClientId,
    /// Transaction or referenced deposit identifier.
    tx: TxId,
    /// Borrowed raw amount; optional and deliberately uninterpreted for partner actions.
    #[serde(default, borrow)]
    amount: Option<&'row [u8]>,
}

/// Transaction types accepted in CSV input.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    /// Credit an account.
    Deposit,
    /// Debit an account.
    Withdrawal,
    /// Place a deposit under dispute.
    Dispute,
    /// Release a disputed deposit.
    Resolve,
    /// Reverse a disputed deposit and lock its account.
    Chargeback,
}

/// Why a CSV row cannot be converted into a valid transaction.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// CSV fields do not match the expected transaction shape.
    #[error("invalid CSV transaction fields")]
    Csv,
    /// CSV deserialization failed at this zero-based column index.
    #[error("invalid CSV transaction field at zero-based column {0}")]
    CsvColumn(u64),
    /// A deposit or withdrawal row has an empty or missing amount.
    #[error("deposit or withdrawal without an amount")]
    MissingAmount,
    /// A deposit or withdrawal amount is invalid.
    #[error(transparent)]
    InvalidAmount(#[from] money::AmountError),
}

#[cfg(test)]
mod tests {
    /// Reads all input rows through the same Serde boundary as the worker.
    fn parse(input: impl AsRef<[u8]>) -> Vec<Result<super::Transaction, super::ParseError>> {
        let mut reader = csv::ReaderBuilder::new()
            .flexible(true)
            .trim(csv::Trim::All)
            .from_reader(input.as_ref());
        let header = reader.byte_headers().expect("fixture header").clone();
        reader
            .byte_records()
            .map(|row| super::parse(&row.expect("fixture row"), &header))
            .collect()
    }

    #[test]
    fn deserializes_every_transaction_type_and_reordered_columns() {
        let rows = parse(
            "amount, tx, type, client\n1.5, 2, deposit, 1\n0.0001, 3, withdrawal, 1\n,2,dispute,1\n,2,resolve,1\n,2,chargeback,1\n",
        );
        let rows: Vec<_> = rows
            .into_iter()
            .map(|row| row.expect("valid transaction"))
            .collect();
        assert!(matches!(rows[0].action(), super::Action::Deposit(_)));
        assert!(matches!(rows[1].action(), super::Action::Withdrawal(_)));
        assert!(matches!(rows[2].action(), super::Action::Dispute));
        assert!(matches!(rows[3].action(), super::Action::Resolve));
        assert!(matches!(rows[4].action(), super::Action::Chargeback));
        assert!(
            rows.iter()
                .all(|row| row.client() == super::ClientId::new(1))
        );
    }

    #[test]
    fn dispute_actions_ignore_missing_or_unusable_amounts() {
        let mut input =
            b"type,client,tx,amount\ndispute,1,2\nresolve,1,2,garbage\nchargeback,1,2,".to_vec();
        input.extend_from_slice(b"\xff\n");
        let rows = parse(input);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(Result::is_ok), "{rows:?}");
        let without_column = parse("type,client,tx\ndispute,1,2\n");
        assert!(without_column[0].is_ok());
    }

    #[test]
    fn deposits_require_positive_amounts() {
        let rows = parse(
            "type,client,tx,amount\ndeposit,1,2,\nwithdrawal,1,3\ndeposit,1,4,-5\nwithdrawal,1,5,invalid\n",
        );
        assert!(matches!(rows[0], Err(super::ParseError::MissingAmount)));
        assert!(matches!(rows[1], Err(super::ParseError::MissingAmount)));
        assert!(matches!(
            rows[2],
            Err(super::ParseError::InvalidAmount(
                super::money::AmountError::NotPositive
            ))
        ));
        assert!(matches!(
            rows[3],
            Err(super::ParseError::InvalidAmount(
                super::money::AmountError::Malformed
            ))
        ));
    }

    #[test]
    fn invalid_utf8_in_required_amount_is_malformed() {
        let rows = parse(b"type,client,tx,amount\ndeposit,1,2,1\xff\nwithdrawal,1,3,\xff\n");
        for row in rows {
            assert!(matches!(
                row,
                Err(super::ParseError::InvalidAmount(
                    super::money::AmountError::Malformed
                ))
            ));
        }
    }

    #[test]
    fn serde_rejects_invalid_or_ambiguous_required_fields() {
        for input in [
            "type,client,tx,amount\ntransfer,1,2,1\n",
            "type,client,tx,amount\ndeposit,65536,2,1\n",
            "type,client,tx,amount\ndeposit,1,4294967296,1\n",
            "type,client,amount\ndeposit,1,1\n",
            "type,client,tx,type\ndispute,1,2,deposit\n",
            "type,client,tx,amount,amount\ndispute,1,2,,\n",
        ] {
            assert!(
                matches!(
                    parse(input)[0],
                    Err(super::ParseError::Csv | super::ParseError::CsvColumn(_))
                ),
                "{input}"
            );
        }
    }

    #[test]
    fn csv_errors_do_not_expose_field_values() {
        let mut rows = parse("type,client,tx,amount\nprivate_value,1,2,1\n");
        let error = rows.remove(0).expect_err("invalid transaction type");
        assert!(!error.to_string().contains("private_value"));
        assert!(!format!("{error:?}").contains("private_value"));

        let mut rows = parse("type,client,tx,amount\ndeposit,private_value,2,1\n");
        let error = rows.remove(0).expect_err("invalid client");
        assert!(matches!(error, super::ParseError::CsvColumn(1)));
        assert!(error.to_string().contains("zero-based column 1"));
        assert!(!error.to_string().contains("private_value"));
    }

    #[test]
    fn ignores_extra_fields() {
        let rows = parse(b"type,client,tx,amount,notes\ndeposit,1,2,1.5,\xff\n");
        assert!(matches!(
            rows[0].as_ref().map(super::Transaction::action),
            Ok(super::Action::Deposit(_))
        ));
    }
}
