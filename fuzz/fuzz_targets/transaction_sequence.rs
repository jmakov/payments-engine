//! Compares structured transaction sequences with the shared independent engine model.
//!
//! Small and boundary ID sets encourage collisions alongside full-width IDs.
#![no_main]

use payments_engine::{engine, money, transaction};

#[path = "../../tests/support/engine_model.rs"]
mod engine_model;

/// A small or boundary-sized positive amount in 0.0001-unit ticks.
#[derive(arbitrary::Arbitrary, Debug)]
enum Amount {
    /// One to 65,536 ticks.
    Small(u16),
    /// An amount close to the largest representable balance.
    NearMaximum(u16),
}

impl Amount {
    /// Converts the generated value to a validated transaction amount.
    fn validated(&self) -> money::Amount {
        let ticks = match self {
            Self::Small(value) => i64::from(*value) + 1,
            Self::NearMaximum(value) => i64::MAX - i64::from(*value),
        };
        let text = money::Money::from_ticks(ticks).to_string();
        money::Amount::parse(&text).expect("generated positive ticks are valid amounts")
    }
}

/// Action and its required amount, matching the domain transaction shape.
#[derive(arbitrary::Arbitrary, Debug)]
enum Action {
    /// Credit an account.
    Deposit(Amount),
    /// Debit an account.
    Withdrawal(Amount),
    /// Hold a previously credited amount.
    Dispute,
    /// Release a hold.
    Resolve,
    /// Reverse a disputed deposit and lock its account.
    Chargeback,
}

/// Selects a client ID while retaining frequent collisions and full-width coverage.
#[derive(arbitrary::Arbitrary, Debug)]
enum ClientId {
    /// One of four frequently reused IDs, including zero.
    Small(u8),
    /// Any client ID.
    Full(u16),
    /// The largest client ID.
    Maximum,
}

impl ClientId {
    /// Returns the selected client ID.
    fn value(&self) -> u16 {
        match self {
            Self::Small(value) => u16::from(*value % 4),
            Self::Full(value) => *value,
            Self::Maximum => u16::MAX,
        }
    }
}

/// Selects a transaction ID while retaining frequent collisions and full-width coverage.
#[derive(arbitrary::Arbitrary, Debug)]
enum TxId {
    /// One of twelve frequently reused IDs, including zero.
    Small(u8),
    /// Any transaction ID.
    Full(u32),
    /// The largest transaction ID.
    Maximum,
}

impl TxId {
    /// Returns the selected transaction ID.
    fn value(&self) -> u32 {
        match self {
            Self::Small(value) => u32::from(*value % 12),
            Self::Full(value) => *value,
            Self::Maximum => u32::MAX,
        }
    }
}

/// Input for one transaction; repeated IDs encourage interactions.
#[derive(arbitrary::Arbitrary, Debug)]
struct Operation {
    /// Selects a client.
    client: ClientId,
    /// Selects a deposit or withdrawal ID.
    tx: TxId,
    /// Requested transaction action.
    action: Action,
}

impl Operation {
    /// Builds a valid domain transaction, without asserting its business acceptance.
    fn transaction(&self) -> transaction::Transaction {
        let action = match &self.action {
            Action::Deposit(amount) => transaction::Action::Deposit(amount.validated()),
            Action::Withdrawal(amount) => transaction::Action::Withdrawal(amount.validated()),
            Action::Dispute => transaction::Action::Dispute,
            Action::Resolve => transaction::Action::Resolve,
            Action::Chargeback => transaction::Action::Chargeback,
        };
        transaction::Transaction::new(
            transaction::ClientId::new(self.client.value()),
            transaction::TxId::new(self.tx.value()),
            action,
        )
    }
}

libfuzzer_sys::fuzz_target!(|operations: Vec<Operation>| {
    let mut engine = engine::Engine::new();
    let mut model = engine_model::Model::default();
    // Bounded sequences keep each fuzz iteration fast and match the property test's scale.
    for operation in operations.iter().take(64) {
        let transaction = operation.transaction();
        assert_eq!(engine.apply(transaction).is_ok(), model.apply(transaction));
        assert_eq!(engine_model::engine_accounts(&engine), model.accounts);
    }
});
