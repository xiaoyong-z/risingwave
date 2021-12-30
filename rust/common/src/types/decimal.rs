use std::ops::{Add, Div, Mul, Neg, Rem, Sub};

use num_traits::{CheckedAdd, CheckedDiv, CheckedMul, CheckedRem, CheckedSub};
pub use rust_decimal::prelude::{FromPrimitive, FromStr, ToPrimitive};
use rust_decimal::{Decimal as RustDecimal, Error};

#[derive(Debug, Copy, Clone, PartialEq, Hash, Eq, Ord, PartialOrd)]
pub enum Decimal {
    Normalized(RustDecimal),
    NaN,
    PositiveINF,
    NegativeINF,
}

macro_rules! impl_from_integer {
    ([$(($T:ty, $from_int:tt)), *]) => {
        $(fn $from_int(num: $T) -> Option<Self> {
            RustDecimal::$from_int(num).map(Decimal::Normalized)
        })*
    }
}

macro_rules! impl_to_integer {
    ([$(($T:ty, $to_int:tt)), *]) => {
        $(fn $to_int(&self) -> Option<$T> {
            match self {
                Self::Normalized(d) => d.$to_int(),
                _ => None,
            }
        })*
    }
}

macro_rules! impl_to_float {
    ([$(($T:ty, $to_float:tt)), *]) => {
        $(fn $to_float(&self) -> Option<$T> {
            match self {
                Self::Normalized(d) => d.$to_float(),
                Self::NaN => Some(<$T>::NAN),
                Self::PositiveINF => Some(<$T>::INFINITY),
                Self::NegativeINF => Some(<$T>::NEG_INFINITY),
            }
        })*
    }
}

macro_rules! impl_from_float {
    ([$(($T:ty, $from_float:tt)), *]) => {
        $(fn $from_float(num: $T) -> Option<Self> {
            match num {
                num if num.is_nan() => Some(Decimal::NaN),
                num if num.is_infinite() && num.is_sign_positive() => Some(Decimal::PositiveINF),
                num if num.is_infinite() && num.is_sign_negative() => Some(Decimal::NegativeINF),
                num => RustDecimal::$from_float(num).map(Decimal::Normalized),
            }
        })*
    }
}

macro_rules! impl_from {
    ($T:ty, $from_ty:path) => {
        impl core::convert::From<$T> for Decimal {
            #[inline]
            fn from(t: $T) -> Self {
                $from_ty(t).unwrap()
            }
        }
    };
}

macro_rules! impl_try_from_decimal {
    ($from_ty:ty, $to_ty:ty, $convert:path, $err:expr) => {
        impl core::convert::TryFrom<$from_ty> for $to_ty {
            type Error = Error;
            fn try_from(value: $from_ty) -> Result<Self, Self::Error> {
                $convert(&value).ok_or_else(|| Error::from($err))
            }
        }
    };
}

macro_rules! impl_try_from_float {
    ($from_ty:ty, $to_ty:ty, $convert:path, $err:expr) => {
        impl core::convert::TryFrom<$from_ty> for $to_ty {
            type Error = Error;
            fn try_from(value: $from_ty) -> Result<Self, Self::Error> {
                $convert(value).ok_or_else(|| Error::from($err))
            }
        }
    };
}

macro_rules! checked_proxy {
    ($trait:tt, $func:tt, $op: tt) => {
        impl $trait for Decimal {
            fn $func(&self, other: &Self) -> Option<Self> {
                match (self, other) {
                    (Self::Normalized(lhs), Self::Normalized(rhs)) => {
                        lhs.$func(rhs).map(Decimal::Normalized)
                    }
                    (lhs, rhs) => Some(*lhs $op *rhs),
                }
            }
        }
    }
}

impl_try_from_decimal!(Decimal, f32, Decimal::to_f32, "Failed to convert to f32");
impl_try_from_decimal!(Decimal, f64, Decimal::to_f64, "Failed to convert to f64");
impl_try_from_float!(
    f32,
    Decimal,
    Decimal::from_f32,
    "Failed to convert to Decimal"
);
impl_try_from_float!(
    f64,
    Decimal,
    Decimal::from_f64,
    "Failed to convert to Decimal"
);

impl FromPrimitive for Decimal {
    impl_from_integer!([
        (u8, from_u8),
        (u16, from_u16),
        (u32, from_u32),
        (u64, from_u64),
        (i8, from_i8),
        (i16, from_i16),
        (i32, from_i32),
        (i64, from_i64)
    ]);
    impl_from_float!([(f32, from_f32), (f64, from_f64)]);
}

impl_from!(isize, FromPrimitive::from_isize);
impl_from!(i8, FromPrimitive::from_i8);
impl_from!(i16, FromPrimitive::from_i16);
impl_from!(i32, FromPrimitive::from_i32);
impl_from!(i64, FromPrimitive::from_i64);
impl_from!(usize, FromPrimitive::from_usize);
impl_from!(u8, FromPrimitive::from_u8);
impl_from!(u16, FromPrimitive::from_u16);
impl_from!(u32, FromPrimitive::from_u32);
impl_from!(u64, FromPrimitive::from_u64);

checked_proxy!(CheckedRem, checked_rem, %);
checked_proxy!(CheckedSub, checked_sub, -);
checked_proxy!(CheckedAdd, checked_add, +);
checked_proxy!(CheckedDiv, checked_div, /);
checked_proxy!(CheckedMul, checked_mul, *);

impl Add for Decimal {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        match (self, other) {
            (Self::Normalized(lhs), Self::Normalized(rhs)) => Self::Normalized(lhs + rhs),
            (Self::NaN, _) => Self::NaN,
            (_, Self::NaN) => Self::NaN,
            (Self::PositiveINF, Self::NegativeINF) => Self::NaN,
            (Self::NegativeINF, Self::PositiveINF) => Self::NaN,
            (Self::PositiveINF, _) => Self::PositiveINF,
            (_, Self::PositiveINF) => Self::PositiveINF,
            (Self::NegativeINF, _) => Self::NegativeINF,
            (_, Self::NegativeINF) => Self::NegativeINF,
        }
    }
}

impl Neg for Decimal {
    type Output = Self;
    fn neg(self) -> Self {
        match self {
            Self::Normalized(d) => Self::Normalized(-d),
            Self::NaN => Self::NaN,
            Self::PositiveINF => Self::NegativeINF,
            Self::NegativeINF => Self::PositiveINF,
        }
    }
}

impl Rem for Decimal {
    type Output = Self;

    fn rem(self, other: Self) -> Self {
        match (self, other) {
            (Self::Normalized(lhs), Self::Normalized(rhs)) if !rhs.is_zero() => {
                Self::Normalized(lhs % rhs)
            }
            (Self::Normalized(_), Self::Normalized(_)) => Self::NaN,
            (Self::Normalized(lhs), Self::PositiveINF)
                if lhs.is_sign_positive() || lhs.is_zero() =>
            {
                Self::Normalized(lhs)
            }
            (Self::Normalized(d), Self::PositiveINF) => Self::Normalized(d),
            (Self::Normalized(lhs), Self::NegativeINF)
                if lhs.is_sign_negative() || lhs.is_zero() =>
            {
                Self::Normalized(lhs)
            }
            (Self::Normalized(d), Self::NegativeINF) => Self::Normalized(d),
            _ => Self::NaN,
        }
    }
}

impl Div for Decimal {
    type Output = Self;

    fn div(self, other: Self) -> Self {
        match (self, other) {
            // nan
            (Self::NaN, _) => Self::NaN,
            (_, Self::NaN) => Self::NaN,
            // div by zero
            (lhs, Self::Normalized(rhs)) if rhs.is_zero() => match lhs {
                Self::Normalized(lhs) => {
                    if lhs.is_sign_positive() && !lhs.is_zero() {
                        Self::PositiveINF
                    } else if lhs.is_sign_negative() && !lhs.is_zero() {
                        Self::NegativeINF
                    } else {
                        Self::NaN
                    }
                }
                Self::PositiveINF => Self::PositiveINF,
                Self::NegativeINF => Self::NegativeINF,
                _ => unreachable!(),
            },
            // div by +/-inf
            (Self::Normalized(_), Self::PositiveINF) => Self::Normalized(RustDecimal::from(0)),
            (_, Self::PositiveINF) => Self::NaN,
            (Self::Normalized(_), Self::NegativeINF) => Self::Normalized(RustDecimal::from(0)),
            (_, Self::NegativeINF) => Self::NaN,
            // div inf
            (Self::PositiveINF, Self::Normalized(d)) if d.is_sign_positive() => Self::PositiveINF,
            (Self::PositiveINF, Self::Normalized(d)) if d.is_sign_negative() => Self::NegativeINF,
            (Self::NegativeINF, Self::Normalized(d)) if d.is_sign_positive() => Self::NegativeINF,
            (Self::NegativeINF, Self::Normalized(d)) if d.is_sign_negative() => Self::PositiveINF,
            // normal case
            (Self::Normalized(lhs), Self::Normalized(rhs)) => Self::Normalized(lhs / rhs),
            _ => unreachable!(),
        }
    }
}

impl Mul for Decimal {
    type Output = Self;

    fn mul(self, other: Self) -> Self {
        match (self, other) {
            (Self::Normalized(lhs), Self::Normalized(rhs)) => Self::Normalized(lhs * rhs),
            (Self::NaN, _) => Self::NaN,
            (_, Self::NaN) => Self::NaN,
            (Self::PositiveINF, Self::Normalized(rhs))
                if !rhs.is_zero() && rhs.is_sign_negative() =>
            {
                Self::NegativeINF
            }
            (Self::PositiveINF, Self::Normalized(rhs))
                if !rhs.is_zero() && rhs.is_sign_positive() =>
            {
                Self::PositiveINF
            }
            (Self::PositiveINF, Self::PositiveINF) => Self::PositiveINF,
            (Self::PositiveINF, Self::NegativeINF) => Self::NegativeINF,
            (Self::Normalized(lhs), Self::PositiveINF)
                if !lhs.is_zero() && lhs.is_sign_negative() =>
            {
                Self::NegativeINF
            }
            (Self::Normalized(lhs), Self::PositiveINF)
                if !lhs.is_zero() && lhs.is_sign_positive() =>
            {
                Self::PositiveINF
            }
            (Self::NegativeINF, Self::PositiveINF) => Self::NegativeINF,
            (Self::NegativeINF, Self::Normalized(rhs))
                if !rhs.is_zero() && rhs.is_sign_negative() =>
            {
                Self::PositiveINF
            }
            (Self::NegativeINF, Self::Normalized(rhs))
                if !rhs.is_zero() && rhs.is_sign_positive() =>
            {
                Self::NegativeINF
            }
            (Self::NegativeINF, Self::NegativeINF) => Self::PositiveINF,
            (Self::Normalized(lhs), Self::NegativeINF)
                if !lhs.is_zero() && lhs.is_sign_negative() =>
            {
                Self::PositiveINF
            }
            (Self::Normalized(lhs), Self::NegativeINF)
                if !lhs.is_zero() && lhs.is_sign_positive() =>
            {
                Self::NegativeINF
            }
            // 0 * {inf, nan} => nan
            _ => Self::NaN,
        }
    }
}

impl Sub for Decimal {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        match (self, other) {
            (Self::Normalized(lhs), Self::Normalized(rhs)) => Self::Normalized(lhs - rhs),
            (Self::NaN, _) => Self::NaN,
            (_, Self::NaN) => Self::NaN,
            (Self::PositiveINF, Self::PositiveINF) => Self::NaN,
            (Self::NegativeINF, Self::NegativeINF) => Self::NaN,
            (Self::PositiveINF, _) => Self::PositiveINF,
            (_, Self::PositiveINF) => Self::NegativeINF,
            (Self::NegativeINF, _) => Self::NegativeINF,
            (_, Self::NegativeINF) => Self::PositiveINF,
        }
    }
}

impl ToString for Decimal {
    fn to_string(&self) -> String {
        match self {
            Self::Normalized(d) => d.to_string(),
            Self::NaN => "NaN".to_string(),
            Self::PositiveINF => "+Inf".to_string(),
            Self::NegativeINF => "-Inf".to_string(),
        }
    }
}

impl Decimal {
    /// TODO: handle nan and inf
    pub fn mantissa(&self) -> i128 {
        match self {
            Self::Normalized(d) => d.mantissa(),
            _ => 0,
        }
    }
    /// TODO: handle nan and inf
    pub fn scale(&self) -> u32 {
        match self {
            Self::Normalized(d) => d.scale(),
            _ => 0,
        }
    }
    pub fn new(num: i64, scale: u32) -> Self {
        Self::Normalized(RustDecimal::new(num, scale))
    }
    pub fn zero() -> Self {
        Self::from(0)
    }
    pub fn round_dp(&self, dp: u32) -> Self {
        match self {
            Self::Normalized(d) => Self::Normalized(d.round_dp(dp)),
            d => *d,
        }
    }
    pub fn from_i128_with_scale(num: i128, scale: u32) -> Self {
        Decimal::Normalized(RustDecimal::from_i128_with_scale(num, scale))
    }
    pub fn normalize(&self) -> Self {
        match self {
            Self::Normalized(d) => Self::Normalized(d.normalize()),
            d => *d,
        }
    }
    pub fn serialize(&self) -> [u8; 16] {
        // according to https://docs.rs/rust_decimal/1.18.0/src/rust_decimal/decimal.rs.html#665-684
        // the lower 15 bits is not used, so we can use first byte to distinguish nan and inf
        match self {
            Self::Normalized(d) => d.serialize(),
            Self::NaN => [vec![1u8], vec![0u8; 15]].concat().try_into().unwrap(),
            Self::PositiveINF => [vec![2u8], vec![0u8; 15]].concat().try_into().unwrap(),
            Self::NegativeINF => [vec![3u8], vec![0u8; 15]].concat().try_into().unwrap(),
        }
    }
    pub fn deserialize(bytes: [u8; 16]) -> Self {
        match bytes[0] {
            0u8 => Self::Normalized(RustDecimal::deserialize(bytes)),
            1u8 => Self::NaN,
            2u8 => Self::PositiveINF,
            3u8 => Self::NegativeINF,
            _ => unreachable!(),
        }
    }
}

impl Default for Decimal {
    fn default() -> Self {
        Self::Normalized(RustDecimal::default())
    }
}

impl ToPrimitive for Decimal {
    impl_to_integer!([
        (i64, to_i64),
        (i32, to_i32),
        (i16, to_i16),
        (i8, to_i8),
        (u64, to_u64),
        (u32, to_u32),
        (u16, to_u16),
        (u8, to_u8)
    ]);
    impl_to_float!([(f64, to_f64), (f32, to_f32)]);
}

impl FromStr for Decimal {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "nan" | "NaN" | "NAN" => Ok(Decimal::NaN),
            "inf" | "INF" | "+inf" | "+INF" => Ok(Decimal::PositiveINF),
            "-inf" | "-INF" => Ok(Decimal::NegativeINF),
            s => RustDecimal::from_str(s).map(Decimal::Normalized),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(lhs: f32, rhs: f32) -> bool {
        if lhs.is_nan() && rhs.is_nan() {
            true
        } else if lhs.is_infinite() && rhs.is_infinite() {
            if lhs.is_sign_positive() && rhs.is_sign_positive() {
                true
            } else {
                lhs.is_sign_negative() && rhs.is_sign_negative()
            }
        } else if lhs.is_finite() && rhs.is_finite() {
            lhs == rhs
        } else {
            false
        }
    }

    #[test]
    fn check_op_with_float() {
        let decimals = [
            Decimal::NaN,
            Decimal::PositiveINF,
            Decimal::NegativeINF,
            Decimal::from_f32(1.0).unwrap(),
            Decimal::from_f32(-1.0).unwrap(),
            Decimal::from_f32(0.0).unwrap(),
        ];
        let floats = [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            1.0f32,
            -1.0f32,
            0.0f32,
        ];
        for (d_lhs, f_lhs) in decimals.iter().zip(floats.iter()) {
            for (d_rhs, f_rhs) in decimals.iter().zip(floats.iter()) {
                assert!(check((*d_lhs + *d_rhs).to_f32().unwrap(), f_lhs + f_rhs));
                assert!(check((*d_lhs - *d_rhs).to_f32().unwrap(), f_lhs - f_rhs));
                assert!(check((*d_lhs * *d_rhs).to_f32().unwrap(), f_lhs * f_rhs));
                assert!(check((*d_lhs / *d_rhs).to_f32().unwrap(), f_lhs / f_rhs));
                assert!(check((*d_lhs % *d_rhs).to_f32().unwrap(), f_lhs % f_rhs));
            }
        }
    }

    #[test]
    fn basic_test() {
        assert_eq!(Decimal::from_str("nan").unwrap(), Decimal::NaN,);
        assert_eq!(Decimal::from_str("inf").unwrap(), Decimal::PositiveINF,);
        assert_eq!(Decimal::from_str("-inf").unwrap(), Decimal::NegativeINF,);
        assert_eq!(
            Decimal::from_f32(10.0).unwrap() / Decimal::PositiveINF,
            Decimal::from_f32(0.0).unwrap(),
        );
        assert_eq!(
            Decimal::from_f32(f32::INFINITY).unwrap(),
            Decimal::PositiveINF
        );
        assert_eq!(Decimal::from_f64(f64::NAN).unwrap(), Decimal::NaN);
        assert_eq!(
            Decimal::from_f64(f64::INFINITY).unwrap(),
            Decimal::PositiveINF
        );
        assert_eq!(
            Decimal::deserialize(Decimal::from_f64(1.234).unwrap().serialize()),
            Decimal::from_f64(1.234).unwrap(),
        );
        assert_eq!(
            Decimal::deserialize(Decimal::from_u8(1).unwrap().serialize()),
            Decimal::from_u8(1).unwrap(),
        );
        assert_eq!(
            Decimal::deserialize(Decimal::from_i8(1).unwrap().serialize()),
            Decimal::from_i8(1).unwrap(),
        );
        assert_eq!(
            Decimal::deserialize(Decimal::from_u16(1).unwrap().serialize()),
            Decimal::from_u16(1).unwrap(),
        );
        assert_eq!(
            Decimal::deserialize(Decimal::from_i16(1).unwrap().serialize()),
            Decimal::from_i16(1).unwrap(),
        );
        assert_eq!(
            Decimal::deserialize(Decimal::from_u32(1).unwrap().serialize()),
            Decimal::from_u32(1).unwrap(),
        );
        assert_eq!(
            Decimal::deserialize(Decimal::from_i32(1).unwrap().serialize()),
            Decimal::from_i32(1).unwrap(),
        );
        assert_eq!(
            Decimal::deserialize(Decimal::from_f64(f64::NAN).unwrap().serialize()),
            Decimal::from_f64(f64::NAN).unwrap(),
        );
        assert_eq!(
            Decimal::deserialize(Decimal::from_f64(f64::INFINITY).unwrap().serialize()),
            Decimal::from_f64(f64::INFINITY).unwrap(),
        );
        assert_eq!(Decimal::to_u8(&Decimal::from_u8(1).unwrap()).unwrap(), 1,);
        assert_eq!(Decimal::to_i8(&Decimal::from_i8(1).unwrap()).unwrap(), 1,);
        assert_eq!(Decimal::to_u16(&Decimal::from_u16(1).unwrap()).unwrap(), 1,);
        assert_eq!(Decimal::to_i16(&Decimal::from_i16(1).unwrap()).unwrap(), 1,);
        assert_eq!(Decimal::to_u32(&Decimal::from_u32(1).unwrap()).unwrap(), 1,);
        assert_eq!(Decimal::to_i32(&Decimal::from_i32(1).unwrap()).unwrap(), 1,);
        assert_eq!(Decimal::to_u64(&Decimal::from_u64(1).unwrap()).unwrap(), 1,);
        assert_eq!(Decimal::to_i64(&Decimal::from_i64(1).unwrap()).unwrap(), 1,);
    }
}