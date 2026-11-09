//! 丸めを行わない価格・数量・率・残高の固定小数点整数型。
use std::{collections::BTreeMap, fmt, str::FromStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NumericError {
    Invalid,
    Precision,
    Overflow,
    DivisionByZero,
}
impl fmt::Display for NumericError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for NumericError {}

fn parse_raw(text: &str, scale: u32) -> Result<i128, NumericError> {
    let invalid = NumericError::Invalid;
    let (negative, text) = match text.as_bytes().first().ok_or(invalid)? {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    let (text, exponent) = if let Some(at) = text.find(['e', 'E']) {
        (
            &text[..at],
            text[at + 1..].parse::<i32>().map_err(|_| invalid)?,
        )
    } else {
        (text, 0)
    };
    let mut fractional = None;
    let mut digits = 0_i32;
    let mut zeroes = 0_i32;
    for ch in text.bytes() {
        match ch {
            b'.' if fractional.is_none() => fractional = Some(0_i32),
            b'0'..=b'9' => {
                digits = digits.checked_add(1).ok_or(NumericError::Overflow)?;
                if ch == b'0' {
                    zeroes += 1
                } else {
                    zeroes = 0
                };
                if let Some(n) = &mut fractional {
                    *n += 1
                }
            }
            _ => return Err(invalid),
        }
    }
    if digits == 0 {
        return Err(invalid);
    }
    if digits == zeroes {
        return Ok(0);
    }
    let power = (scale as i32)
        .checked_add(exponent)
        .and_then(|n| n.checked_sub(fractional.unwrap_or(0)))
        .and_then(|n| n.checked_add(zeroes))
        .ok_or(NumericError::Overflow)?;
    if power < 0 {
        return Err(NumericError::Precision);
    }
    let mut raw = 0_u128;
    for ch in text
        .bytes()
        .filter(|b| *b != b'.')
        .take((digits - zeroes) as usize)
    {
        raw = raw
            .checked_mul(10)
            .and_then(|n| n.checked_add((ch - b'0') as u128))
            .ok_or(NumericError::Overflow)?;
    }
    raw = raw
        .checked_mul(
            10_u128
                .checked_pow(power as u32)
                .ok_or(NumericError::Overflow)?,
        )
        .ok_or(NumericError::Overflow)?;
    if negative && raw == 1_u128 << 127 {
        Ok(i128::MIN)
    } else {
        let n = i128::try_from(raw).map_err(|_| NumericError::Overflow)?;
        Ok(if negative { -n } else { n })
    }
}
fn display_raw(raw: i128, scale: u32, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let value = raw.unsigned_abs();
    let factor = 10_u128.pow(scale);
    if raw < 0 {
        f.write_str("-")?
    }
    write!(f, "{}", value / factor)?;
    let remainder = value % factor;
    if remainder != 0 {
        let fraction = format!("{:0width$}", remainder, width = scale as usize);
        write!(f, ".{}", fraction.trim_end_matches('0'))?
    }
    Ok(())
}
macro_rules! fixed {
    ($name:ident,$raw:ty,$scale:literal) => {
        #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name($raw);
        impl $name {
            pub const ZERO: Self = Self(0);
            pub const ONE: Self = Self((10 as $raw).pow($scale));
            pub const MIN: Self = Self(<$raw>::MIN);
            pub const MAX: Self = Self(<$raw>::MAX);
            pub const fn from_units(raw: $raw) -> Self {
                Self(raw)
            }
            pub const fn units(self) -> $raw {
                self.0
            }
            pub fn is_zero(self) -> bool {
                self.0 == 0
            }
            pub fn checked_add(self, other: Self) -> Result<Self, NumericError> {
                self.0
                    .checked_add(other.0)
                    .map(Self)
                    .ok_or(NumericError::Overflow)
            }
            pub fn checked_sub(self, other: Self) -> Result<Self, NumericError> {
                self.0
                    .checked_sub(other.0)
                    .map(Self)
                    .ok_or(NumericError::Overflow)
            }
            pub fn checked_neg(self) -> Result<Self, NumericError> {
                self.0.checked_neg().map(Self).ok_or(NumericError::Overflow)
            }
            pub fn to_f64(self) -> f64 {
                self.0 as f64 / 10_f64.powi($scale)
            }
        }
        impl From<i32> for $name {
            fn from(n: i32) -> Self {
                Self((n as $raw) * (10 as $raw).pow($scale))
            }
        }
        impl FromStr for $name {
            type Err = NumericError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let n = parse_raw(s, $scale)?;
                Ok(Self(
                    <$raw>::try_from(n).map_err(|_| NumericError::Overflow)?,
                ))
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                display_raw(self.0 as i128, $scale, f)
            }
        }
        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(self)
            }
        }
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let value = serde_json::Value::deserialize(d)?;
                let text = match value {
                    serde_json::Value::String(s) => s,
                    serde_json::Value::Number(n) => n.to_string(),
                    _ => {
                        return Err(serde::de::Error::custom(
                            "expected exact numeric string or number",
                        ));
                    }
                };
                text.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}
fixed!(Price, i128, 8);
fixed!(Quantity, i64, 8);
fixed!(Rate, i64, 8);
fixed!(Amount, i128, 24);
impl Price {
    pub fn notional(self, quantity: Quantity) -> Result<Amount, NumericError> {
        self.0
            .checked_mul(quantity.0 as i128)
            .and_then(|n| n.checked_mul(100_000_000))
            .map(Amount)
            .ok_or(NumericError::Overflow)
    }
    pub fn fee(self, quantity: Quantity, rate: Rate) -> Result<Amount, NumericError> {
        self.0
            .checked_mul(quantity.0 as i128)
            .and_then(|n| n.checked_mul(rate.0 as i128))
            .map(Amount)
            .ok_or(NumericError::Overflow)
    }
}
impl Quantity {
    pub fn to_amount(self) -> Amount {
        Amount(self.0 as i128 * 10_i128.pow(16))
    }
    pub fn checked_remainder(self, lot: Self) -> Result<Self, NumericError> {
        if lot.0 <= 0 {
            return Err(NumericError::DivisionByZero);
        }
        Ok(Self(self.0.rem_euclid(lot.0)))
    }
}
impl Amount {
    pub fn to_quantity(self) -> Result<Quantity, NumericError> {
        let factor = 10_i128.pow(16);
        if self.0 % factor != 0 {
            return Err(NumericError::Precision);
        }
        i64::try_from(self.0 / factor)
            .map(Quantity)
            .map_err(|_| NumericError::Overflow)
    }
}
pub type Levels = BTreeMap<Price, Quantity>;

/// ソース中の検証済み数値リテラル用。実行時入力にはFromStrを使う。
pub fn literal<T: FromStr>(s: &str) -> T
where
    T::Err: fmt::Debug,
{
    s.parse().expect("valid fixed-point literal")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parsing_is_exact_and_checked() {
        for (s, expected) in [
            ("0", 0),
            ("-0", 0),
            ("+1.230000000", 123000000),
            ("1e-8", 1),
            ("1.234567891e1", 1234567891),
            ("90000000000.00000001", 9_000_000_000_000_000_001),
        ] {
            assert_eq!(s.parse::<Quantity>().unwrap().units(), expected);
        }
        assert_eq!(
            "92233720368.54775807".parse::<Quantity>().unwrap(),
            Quantity::MAX
        );
        assert_eq!(
            "-92233720368.54775808".parse::<Quantity>().unwrap(),
            Quantity::MIN
        );
        assert_eq!(
            "92233720368.54775808".parse::<Quantity>(),
            Err(NumericError::Overflow)
        );
        for s in ["1e-9", "1.00000000000000000000000000001"] {
            assert_eq!(s.parse::<Price>(), Err(NumericError::Precision));
        }
        for s in ["", ".", "NaN", "inf", "1..2", "1e", "--1", " 1"] {
            assert!(s.parse::<Price>().is_err(), "{s}");
        }
        for value in [
            Amount::MIN,
            Amount::MAX,
            Amount::ZERO,
            Amount::from_units(1),
        ] {
            assert_eq!(value.to_string().parse::<Amount>().unwrap(), value);
        }
    }
    #[test]
    fn wide_prices_preserve_exact_boundaries_and_json() {
        for (text, expected) in [
            (
                "92233720368.54775808",
                Price::from_units(9_223_372_036_854_775_808),
            ),
            (
                "111111111111.00000001",
                Price::from_units(11_111_111_111_100_000_001),
            ),
            (
                "123456789012",
                Price::from_units(12_345_678_901_200_000_000),
            ),
            ("1701411834604692317316873037158.84105727", Price::MAX),
            ("-1701411834604692317316873037158.84105728", Price::MIN),
        ] {
            assert_eq!(text.parse::<Price>().unwrap(), expected);
            assert_eq!(expected.to_string(), text);
            for json in [text.to_owned(), format!("\"{text}\"")] {
                assert_eq!(serde_json::from_str::<Price>(&json).unwrap(), expected);
            }
            assert_eq!(
                serde_json::to_string(&expected).unwrap(),
                format!("\"{text}\"")
            );
        }
        for text in [
            "1701411834604692317316873037158.84105728",
            "-1701411834604692317316873037158.84105729",
        ] {
            assert_eq!(text.parse::<Price>(), Err(NumericError::Overflow));
        }
        let price: Price = literal("123456789012");
        let quantity: Quantity = literal("0.0002");
        assert_eq!(price.notional(quantity).unwrap(), literal("24691357.8024"));
        assert_eq!(
            price.fee(quantity, literal("-0.0002")).unwrap(),
            literal("-4938.27156048")
        );
        assert_eq!(
            Price::MAX.checked_add(Price::from_units(1)),
            Err(NumericError::Overflow)
        );
        assert_eq!(Price::MIN.checked_neg(), Err(NumericError::Overflow));
        assert_eq!(
            Price::MAX.notional(Quantity::from_units(1)),
            Err(NumericError::Overflow)
        );
        assert_eq!(
            Price::MAX.fee(Quantity::ONE, Rate::ONE),
            Err(NumericError::Overflow)
        );
    }
    #[test]
    fn all_24_digits_and_signed_fees_are_preserved() {
        let p: Price = literal("0.00000001");
        let q: Quantity = literal("0.00000001");
        let r: Rate = literal("0.00000001");
        assert_eq!(p.fee(q, r).unwrap(), Amount::from_units(1));
        assert_eq!(p.notional(q).unwrap(), Amount::from_units(100_000_000));
        assert_eq!(
            p.fee(q, r.checked_neg().unwrap()).unwrap(),
            Amount::from_units(-1)
        );
        let p: Price = literal("200.299");
        let q: Quantity = literal("10");
        assert_eq!(p.notional(q).unwrap(), literal("2002.99"));
        assert_eq!(p.fee(q, literal("-0.0002")).unwrap(), literal("-0.400598"));
    }
    #[test]
    fn overflow_and_precision_reduction_never_round() {
        assert_eq!(
            Amount::MAX.checked_add(Amount::from_units(1)),
            Err(NumericError::Overflow)
        );
        assert_eq!(Amount::MIN.checked_neg(), Err(NumericError::Overflow));
        assert_eq!(
            Price::MAX.notional(Quantity::MAX),
            Err(NumericError::Overflow)
        );
        assert_eq!(
            Price::MAX.fee(Quantity::MAX, Rate::MAX),
            Err(NumericError::Overflow)
        );
        assert_eq!(
            Amount::from_units(1).to_quantity(),
            Err(NumericError::Precision)
        );
        assert_eq!(Amount::MAX.to_quantity(), Err(NumericError::Precision));
        assert_eq!(
            Amount::from(1_000_000).to_quantity().unwrap().to_amount(),
            Amount::from(1_000_000)
        );
        assert_eq!(
            Quantity::ONE.checked_remainder(Quantity::ZERO),
            Err(NumericError::DivisionByZero)
        );
        assert_eq!(
            literal::<Quantity>("1.23")
                .checked_remainder(literal("0.1"))
                .unwrap(),
            literal("0.03")
        );
    }
    #[test]
    fn json_strings_and_numbers_do_not_pass_through_float() {
        for text in ["90000000000.00000001", "\"90000000000.00000001\""] {
            let q: Quantity = serde_json::from_str(text).unwrap();
            assert_eq!(q.units(), 9_000_000_000_000_000_001);
            assert_eq!(
                serde_json::to_string(&q).unwrap(),
                "\"90000000000.00000001\""
            );
        }
        let a: Amount = serde_json::from_str("0.000000000000000000000001").unwrap();
        assert_eq!(a.units(), 1);
        assert!(serde_json::from_str::<Price>("0.000000001").is_err());
        for text in ["null", "true", "{}", "[]"] {
            assert!(serde_json::from_str::<Quantity>(text).is_err());
        }
        assert_eq!(
            serde_json::to_string(&literal::<Price>("200.30000000")).unwrap(),
            "\"200.3\""
        );
    }
}
