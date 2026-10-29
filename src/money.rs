//! Exact fixed-point money: every balance and amount is a whole number of ten-thousandths.
//!
//! The input format allows at most [`DECIMAL_PLACES`] digits after the decimal point, so every
//! value the engine can ever see is an integer multiple of 0.0001 (one *tick*). Storing ticks in
//! an `i64` makes all arithmetic exact: an addition either produces the precise result or, via
//! `checked_add`, reports overflow.
//!
//! Text is converted only at the edges: [`Amount::parse`] turns input text into
//! ticks, and [`Money`]'s `Display` turns ticks back into text for the report.

use std::fmt;

/// Digits after the decimal point that every money value carries.
///
/// Fixed by the input specification. Changing it changes the unit of [`Money`], so stored
/// tick counts would no longer mean the same amount.
pub const DECIMAL_PLACES: u32 = 4;

/// Ticks in one whole currency unit: `10^DECIMAL_PLACES`.
pub const TICKS_PER_UNIT: i64 = 10_i64.pow(DECIMAL_PLACES);

/// A signed amount of money, stored as an exact count of ticks (0.0001 units).
///
/// Balances are `Money` and may be negative: disputing a deposit after its funds were withdrawn
/// puts the deposit on hold and makes available funds negative. [`Amount`] is a separate,
/// strictly positive type for transaction quantities; accepting arbitrary `Money` there would
/// allow a negative withdrawal to credit the account. Every operation that would leave the INT64
/// range is refused rather than wrapped or rounded.
///
/// # Examples
///
/// ```
/// use payments_engine::money;
///
/// let one_and_a_half = money::Money::from_ticks(15_000);
/// let sum = one_and_a_half.checked_add(money::Money::from_ticks(1));
///
/// assert_eq!(sum.map(|money| money.to_string()), Some("1.5001".to_owned()));
/// ```
///
/// Not recommended: plain `+` does not exist, because it could overflow silently in release
/// builds. Use [`Money::checked_add`] and handle `None`.
///
/// ```compile_fail,E0369
/// use payments_engine::money;
///
/// let sum = money::Money::MAX + money::Money::from_ticks(1);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Money(i64);

impl Money {
    /// No money.
    pub const ZERO: Self = Self(0);

    /// The largest representable amount.
    pub const MAX: Self = Self(i64::MAX);

    /// Wraps a raw tick count, where one tick is 0.0001 units.
    #[must_use]
    pub const fn from_ticks(ticks: i64) -> Self {
        Self(ticks)
    }

    /// Returns the raw tick count.
    #[must_use]
    pub const fn ticks(self) -> i64 {
        self.0
    }

    /// Adds `other`, or returns `None` if the result would leave the representable range.
    #[must_use]
    pub const fn checked_add(self, other: Self) -> Option<Self> {
        match self.0.checked_add(other.0) {
            Some(ticks) => Some(Self(ticks)),
            None => None,
        }
    }

    /// Subtracts `other`, or returns `None` if the result would leave the representable range.
    #[must_use]
    pub const fn checked_sub(self, other: Self) -> Option<Self> {
        match self.0.checked_sub(other.0) {
            Some(ticks) => Some(Self(ticks)),
            None => None,
        }
    }
}

/// Prints the shortest exact decimal form: `1.5`, `-10`, `0`, `0.0001`.
impl fmt::Display for Money {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = rust_decimal::Decimal::new(self.0, DECIMAL_PLACES).normalize();
        write!(formatter, "{value}")
    }
}

/// Serializes as the `Display` text, so CSV output shows `1.5` rather than a tick count.
impl serde::Serialize for Money {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn displays_shortest_exact_form() {
        let cases = [
            (0, "0"),
            (1, "0.0001"),
            (15_000, "1.5"),
            (-100_000, "-10"),
            (i64::MAX, "922337203685477.5807"),
            (i64::MIN, "-922337203685477.5808"),
        ];

        for (ticks, text) in cases {
            assert_eq!(super::Money::from_ticks(ticks).to_string(), text);
        }
    }

    #[test]
    fn checked_arithmetic_refuses_to_leave_the_range() {
        let one_tick = super::Money::from_ticks(1);

        assert_eq!(super::Money::MAX.checked_add(one_tick), None);
        assert_eq!(
            super::Money::from_ticks(i64::MIN).checked_sub(one_tick),
            None
        );
        assert_eq!(
            super::Money::MAX.checked_sub(one_tick),
            Some(super::Money::from_ticks(i64::MAX - 1))
        );
    }

    #[test]
    fn ticks_per_unit_matches_decimal_places() {
        assert_eq!(
            super::Money::from_ticks(super::TICKS_PER_UNIT).to_string(),
            "1"
        );
    }
}

/// A strictly positive amount of [`Money`], converted exactly from its decimal text.
///
/// Deposits and withdrawals always move a positive quantity; the direction comes from the
/// transaction type, never from the sign. Rejecting zero, negative and over-precise values at
/// the edge means a malicious or buggy row such as `withdrawal, 1, 7, -100` can never turn into
/// a credit.
///
/// The inner value is private, so the only way to obtain an `Amount` is through
/// [`Amount::parse`], which enforces the invariant. Transaction variants and account operations
/// require this type rather than an `i64` or arbitrary [`Money`], so callers cannot bypass the
/// positive-quantity check. [`Money`] remains signed because account balances may be negative.
///
/// # Examples
///
/// ```
/// # fn main() -> eyre::Result<()> {
/// use payments_engine::money;
///
/// let amount = money::Amount::parse("1.5")?;
/// assert_eq!(amount.get(), money::Money::from_ticks(15_000));
/// # Ok(()) }
/// ```
///
/// Not recommended: building the newtype directly to skip validation does not compile,
/// because the field is private.
///
/// ```compile_fail,E0603
/// use payments_engine::money;
///
/// let negative = money::Amount(money::Money::from_ticks(-10_000));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Amount(Money);

impl Amount {
    /// Parses decimal text such as `1.5`, `+2`, `.25` or `3.` into an exact amount.
    ///
    /// [`rust_decimal::Decimal::from_str_exact`] parses the text; the normalized decimal is
    /// checked for positivity and precision, then converted to `i64` ticks without rounding.
    /// Input follows the task's contract: decimal numbers with at most four fractional places.
    ///
    /// # Arguments
    ///
    /// * `text` - the trimmed `amount` field as UTF-8 text.
    ///
    /// # Errors
    ///
    /// Returns [`AmountError::Malformed`] if the decimal parser rejects the input,
    /// [`AmountError::NotPositive`] for zero or negative values, [`AmountError::TooPrecise`]
    /// for more than [`DECIMAL_PLACES`] significant decimal places, and
    /// [`AmountError::TooLarge`] for values above [`Money::MAX`].
    ///
    /// # Examples
    ///
    /// ```
    /// use payments_engine::money;
    ///
    /// assert_eq!(money::Amount::parse("0.00001"), Err(money::AmountError::TooPrecise));
    /// assert_eq!(money::Amount::parse("-5"), Err(money::AmountError::NotPositive));
    /// ```
    pub fn parse(text: &str) -> Result<Self, AmountError> {
        let mut value = rust_decimal::Decimal::from_str_exact(text)
            .map_err(|_| AmountError::Malformed)?
            .normalize();
        if value <= rust_decimal::Decimal::ZERO {
            return Err(AmountError::NotPositive);
        }
        if value.scale() > DECIMAL_PLACES {
            return Err(AmountError::TooPrecise);
        }
        if value > rust_decimal::Decimal::new(i64::MAX, DECIMAL_PLACES) {
            return Err(AmountError::TooLarge);
        }
        // With at most four fractional places and an i64-sized result, rescaling is exact.
        value.rescale(DECIMAL_PLACES);
        let ticks = i64::try_from(value.mantissa()).map_err(|_| AmountError::TooLarge)?;
        tracing::trace!("amount validated");
        Ok(Self(Money::from_ticks(ticks)))
    }

    /// Returns the validated amount for balance arithmetic.
    #[must_use]
    pub fn get(self) -> Money {
        self.0
    }
}

/// Why amount text is not a valid [`Amount`].
///
/// Surfaces in logs when a malformed input row stops processing; see
/// [`crate::transaction::ParseError`]. Variants describe the error without repeating the value.
// `thiserror::Error` generates the `Display` impl from the `#[error(...)]` messages and
// implements `std::error::Error`, so this type converts into `eyre::Report` with `?`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AmountError {
    /// The text is not valid UTF-8 or is rejected by the decimal parser.
    #[error("amount is not a valid decimal number")]
    Malformed,
    /// The value is zero or negative; the transaction type already encodes the direction.
    #[error("amount must be positive")]
    NotPositive,
    /// The value has more significant decimal places than [`DECIMAL_PLACES`].
    #[error("amount has more than {max} decimal places", max = DECIMAL_PLACES)]
    TooPrecise,
    /// The value exceeds [`Money::MAX`], so no balance could hold it.
    #[error("amount exceeds the maximum of {max}", max = Money::MAX)]
    TooLarge,
}

#[cfg(test)]
mod amount_tests {

    fn parse(text: &str) -> Result<i64, super::AmountError> {
        super::Amount::parse(text).map(|amount| amount.get().ticks())
    }

    #[test]
    fn converts_plain_decimals_to_exact_ticks() {
        let cases = [
            ("1", 10_000),
            ("0.0001", 1),
            ("12345.6789", 123_456_789),
            ("1.50000", 15_000),
            ("+1.5", 15_000),
            (".5", 5_000),
            ("2.", 20_000),
            ("007", 70_000),
            ("1000", 10_000_000),
            // 17 significant digits: more than an f64 holds, so a float round-trip alters it.
            ("1234567890123.0001", 12_345_678_901_230_001),
            ("922337203685477.5807", i64::MAX),
        ];

        for (text, ticks) in cases {
            assert_eq!(parse(text), Ok(ticks), "{text}");
        }
    }

    #[test]
    fn rejects_unusable_text_with_the_reason() {
        let cases = [
            ("", super::AmountError::Malformed),
            ("abc", super::AmountError::Malformed),
            ("0", super::AmountError::NotPositive),
            ("0.0000", super::AmountError::NotPositive),
            ("-0", super::AmountError::NotPositive),
            ("-1", super::AmountError::NotPositive),
            ("-0.0001", super::AmountError::NotPositive),
            ("1.00001", super::AmountError::TooPrecise),
            ("922337203685477.5808", super::AmountError::TooLarge),
            (
                "79228162514264337593543950335",
                super::AmountError::TooLarge,
            ),
            (
                "79228162514264337593543950336",
                super::AmountError::Malformed,
            ),
        ];

        for (text, error) in cases {
            assert_eq!(parse(text), Err(error), "{text:?}");
        }
    }
}
