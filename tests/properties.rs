//! Property tests: random inputs checked against simple, obviously correct reference code.
//!
//! - Money and amounts: [`money::Money`] prints exact ticks and its positive values parse back;
//!   [`money::Amount::parse`] agrees with an independent reference using integer parsers.
//! - Engine: random transaction sequences are applied both to [`engine::Engine`] and to
//!   [`Model`], a naive re-implementation of the README's rules on `i128` balances. After
//!   every step both must agree on acceptance and on every account.
//!
//! Inputs come from a fixed seed, so every run explores exactly the same cases.

use payments_engine::{engine, money, transaction};
// Traits must be in scope for their methods (`prop_map`) to be callable.
use proptest::strategy::Strategy;

#[path = "support/amount_reference.rs"]
mod amount_reference;

#[path = "support/engine_model.rs"]
mod engine_model;

/// Fixed seed and no failure files: the tests are deterministic and write nothing.
fn config() -> proptest::test_runner::Config {
    proptest::test_runner::Config {
        cases: 512,
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x5EED_CAFE),
        failure_persistence: None,
        ..proptest::test_runner::Config::default()
    }
}

/// Builds the validated amount for `ticks` (must be positive) from its text form.
fn amount_from_ticks(ticks: i64) -> money::Amount {
    let text = money::Money::from_ticks(ticks).to_string();
    money::Amount::parse(&text).expect("positive tick counts are valid amounts")
}

/// Random transactions over few clients and ids, so collisions, disputes of real deposits and
/// cross-client references are common. One amount in ten is near `i64::MAX` to hit overflow.
fn transactions() -> impl Strategy<Value = Vec<transaction::Transaction>> {
    let ticks = proptest::prop_oneof![
        9 => 1..=50_000_i64,
        1 => (i64::MAX - 50_000)..=i64::MAX,
    ];
    let transaction =
        (0..5_u8, 0..4_u16, 0..12_u32, ticks).prop_map(|(kind, client, tx, ticks)| {
            let client = transaction::ClientId::new(client);
            let tx = transaction::TxId::new(tx);
            let action = match kind {
                0 => transaction::Action::Deposit(amount_from_ticks(ticks)),
                1 => transaction::Action::Withdrawal(amount_from_ticks(ticks)),
                2 => transaction::Action::Dispute,
                3 => transaction::Action::Resolve,
                _ => transaction::Action::Chargeback,
            };
            transaction::Transaction::new(client, tx, action)
        });
    proptest::collection::vec(transaction, 1..60)
}

proptest::proptest! {
    #![proptest_config(config())]

    #[test]
    fn engine_matches_reference_model(transactions in transactions()) {
        let mut engine = engine::Engine::new();
        let mut model = engine_model::Model::default();

        for transaction in transactions {
            let accepted = engine.apply(transaction).is_ok();

            proptest::prop_assert_eq!(accepted, model.apply(transaction), "{:?}", transaction);
            proptest::prop_assert_eq!(&engine_model::engine_accounts(&engine), &model.accounts);
        }
    }

    #[test]
    fn dispute_lifecycle_preserves_balances(
        client in proptest::prop_oneof![
            proptest::strategy::Just(0_u16),
            proptest::strategy::Just(u16::MAX),
            proptest::num::u16::ANY,
        ],
        deposit_id in proptest::prop_oneof![
            proptest::strategy::Just(0_u32),
            proptest::strategy::Just(u32::MAX),
            proptest::num::u32::ANY,
        ],
        deposit in proptest::prop_oneof![
            proptest::strategy::Just(1_i64),
            proptest::strategy::Just(i64::MAX),
            1_i64..1_000_000_000,
        ],
        withdrawal_seed in 0_i64..=i64::MAX,
        chargeback in proptest::bool::ANY,
    ) {
        let client = transaction::ClientId::new(client);
        let deposit_id = transaction::TxId::new(deposit_id);
        let withdrawal_id = transaction::TxId::new(deposit_id.get().wrapping_add(1));
        let withdrawal = 1 + withdrawal_seed % deposit;
        let remaining = deposit - withdrawal;
        let terminal = if chargeback {
            transaction::Action::Chargeback
        } else {
            transaction::Action::Resolve
        };
        let after_terminal = if chargeback { -withdrawal } else { remaining };
        let steps = [
            (deposit_id, transaction::Action::Deposit(amount_from_ticks(deposit)),
                true, deposit, 0, false),
            (withdrawal_id, transaction::Action::Withdrawal(amount_from_ticks(withdrawal)),
                true, remaining, 0, false),
            (deposit_id, transaction::Action::Dispute, true, -withdrawal, deposit, false),
            (deposit_id, transaction::Action::Dispute, false, -withdrawal, deposit, false),
            (deposit_id, terminal, true, after_terminal, 0, chargeback),
            (deposit_id, terminal, false, after_terminal, 0, chargeback),
        ];
        let mut engine = engine::Engine::new();

        for (tx, action, accepted, available, held, locked) in steps {
            let result = engine.apply(transaction::Transaction::new(client, tx, action));
            proptest::prop_assert_eq!(result.is_ok(), accepted, "{:?}", action);
            let account = engine.account(client).expect("the initial deposit created the account");
            proptest::prop_assert_eq!(
                (account.available().ticks(), account.held().ticks(),
                    account.total().ticks(), account.is_locked()),
                (available, held, available + held, locked),
                "{:?}", action,
            );
        }
    }

    #[test]
    fn money_prints_exact_ticks(ticks in proptest::num::i64::ANY) {
        let money = money::Money::from_ticks(ticks);

        let text = money.to_string();

        if ticks == 0 {
            proptest::prop_assert_eq!(&text, "0");
        } else if ticks == i64::MIN {
            proptest::prop_assert_eq!(&text, "-922337203685477.5808");
        } else {
            let magnitude = text.strip_prefix('-').unwrap_or(&text);
            proptest::prop_assert_eq!(text.starts_with('-'), ticks < 0);
            proptest::prop_assert_eq!(amount_reference::ticks(magnitude.as_bytes()), Some(ticks.abs()));
            if magnitude.contains('.') {
                proptest::prop_assert!(!magnitude.ends_with('0'));
            }
        }
        if ticks > 0 {
            let parsed = money::Amount::parse(&text).map(money::Amount::get);
            proptest::prop_assert_eq!(parsed, Ok(money));
        }
    }

    #[test]
    fn amounts_match_the_reference_parser(
        text in "[+]?[0-9]{1,15}(\\.[0-9]{0,4})?"
    ) {
        let parsed = money::Amount::parse(&text).map(|amount| amount.get().ticks());

        proptest::prop_assert_eq!(parsed.ok(), amount_reference::ticks(text.as_bytes()), "{:?}", text);
    }

    #[test]
    fn amount_padding_preserves_every_positive_tick_count(
        ticks in 1_i64..=i64::MAX,
        leading_zeros in 0_usize..8,
        trailing_zeros in 0_usize..8,
        plus in proptest::bool::ANY,
    ) {
        let mut text = money::Money::from_ticks(ticks).to_string();
        if !text.contains('.') {
            text.push('.');
        }
        let sign = if plus { "+" } else { "" };
        let text = format!("{sign}{}{text}{}", "0".repeat(leading_zeros), "0".repeat(trailing_zeros));

        let parsed = money::Amount::parse(&text).map(|amount| amount.get().ticks());

        proptest::prop_assert_eq!(parsed, Ok(ticks), "{:?}", text);
    }
}
