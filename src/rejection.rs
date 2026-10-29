//! Reasons the engine refuses a valid transaction.
//!
//! A [`Rejection`] is an expected business outcome, not a failure of the program: the spec
//! tells us to ignore, for example, a dispute that references an unknown transaction. The
//! [`crate::engine::Engine`] returns it so callers can log or count refusals, and the account
//! state is guaranteed to be unchanged whenever one is returned.

use crate::transaction;

/// Why [`crate::engine::Engine::apply`] left all balances untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Rejection {
    /// A deposit or withdrawal named an account frozen by an earlier chargeback.
    #[error("account {0} is locked")]
    AccountLocked(transaction::ClientId),
    /// A withdrawal asked for more than the client's available funds.
    #[error("insufficient available funds")]
    InsufficientFunds,
    /// A deposit reused the id of an earlier deposit.
    #[error("tx {0} was already processed")]
    DuplicateTransaction(transaction::TxId),
    /// A dispute, resolve or chargeback referenced a deposit that does not exist.
    #[error("tx {0} is not a known deposit")]
    UnknownTransaction(transaction::TxId),
    /// A dispute, resolve or chargeback named a client that does not own the deposit.
    #[error("tx {0} belongs to another client")]
    ClientMismatch(transaction::TxId),
    /// A dispute referenced a deposit that is already under dispute.
    #[error("tx {0} is already disputed")]
    AlreadyDisputed(transaction::TxId),
    /// A dispute referenced a deposit that has already been charged back.
    #[error("tx {0} was charged back")]
    AlreadyChargedBack(transaction::TxId),
    /// A resolve or chargeback referenced a deposit that is not under dispute.
    #[error("tx {0} is not under dispute")]
    NotDisputed(transaction::TxId),
    /// Applying the transaction would push a balance outside [`crate::money::Money`]'s range.
    #[error(
        "balance would exceed the supported range of +/-{}",
        crate::money::Money::MAX
    )]
    Overflow,
}
