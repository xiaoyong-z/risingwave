// Copyright 2022 Singularity Data
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use core::convert::From;
use std::any::type_name;
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime};
use num_traits::ToPrimitive;
use risingwave_common::error::ErrorCode::{InternalError, InvalidInputSyntax, ParseError};
use risingwave_common::error::{Result, RwError};
use risingwave_common::types::{
    Decimal, NaiveDateTimeWrapper, NaiveDateWrapper, NaiveTimeWrapper, OrderedF32, OrderedF64,
};

/// String literals for bool type.
///
/// See [`https://www.postgresql.org/docs/9.5/datatype-boolean.html`]
const TRUE_BOOL_LITERALS: [&str; 9] = ["true", "tru", "tr", "t", "on", "1", "yes", "ye", "y"];
const FALSE_BOOL_LITERALS: [&str; 10] = [
    "false", "fals", "fal", "fa", "f", "off", "of", "0", "no", "n",
];

#[inline(always)]
pub fn num_up<T, R>(n: T) -> Result<R>
where
    T: Into<R>,
{
    Ok(n.into())
}

#[inline(always)]
pub fn float_up(n: OrderedF32) -> Result<OrderedF64> {
    Ok((n.0 as f64).into())
}

/// Cast between different precision/length.
/// Eg. Char(5) -> Char(10)
/// Currently no-op. TODO: implement padding and overflow check (#2137)
#[inline(always)]
pub fn str_to_str(n: &str) -> Result<String> {
    Ok(n.into())
}

#[inline(always)]
pub fn str_to_date(elem: &str) -> Result<NaiveDateWrapper> {
    Ok(NaiveDateWrapper::new(
        NaiveDate::parse_from_str(elem, "%Y-%m-%d")
            .map_err(|e| RwError::from(ParseError(Box::new(e))))?,
    ))
}

#[inline(always)]
pub fn str_to_time(elem: &str) -> Result<NaiveTimeWrapper> {
    Ok(NaiveTimeWrapper::new(
        NaiveTime::parse_from_str(elem, "%H:%M:%S")
            .map_err(|e| RwError::from(ParseError(Box::new(e))))?,
    ))
}

#[inline(always)]
pub fn str_to_timestamp(elem: &str) -> Result<NaiveDateTimeWrapper> {
    Ok(NaiveDateTimeWrapper::new(
        NaiveDateTime::parse_from_str(elem, "%Y-%m-%d %H:%M:%S")
            .map_err(|e| RwError::from(ParseError(Box::new(e))))?,
    ))
}

#[inline(always)]
pub fn str_to_timestampz(elem: &str) -> Result<i64> {
    DateTime::parse_from_str(elem, "%Y-%m-%d %H:%M:%S %:z")
        .map(|ret| ret.timestamp_nanos() / 1000)
        .map_err(|e| RwError::from(ParseError(Box::new(e))))
}

#[inline(always)]
pub fn str_parse<T>(elem: &str) -> Result<T>
where
    T: FromStr,
    <T as FromStr>::Err: std::fmt::Display,
{
    elem.parse().map_err(|e| {
        RwError::from(InternalError(format!(
            "Can't cast {:?} to {:?}: {}",
            elem,
            type_name::<T>(),
            e
        )))
    })
}

#[inline(always)]
pub fn date_to_timestamp(elem: NaiveDateWrapper) -> Result<NaiveDateTimeWrapper> {
    Ok(NaiveDateTimeWrapper::new(elem.0.and_hms(0, 0, 0)))
}

/// Define the cast function to primitive types.
///
/// Due to the orphan rule, some data can't implement `TryFrom` trait for basic type.
/// We can only use [`ToPrimitive`] trait.
///
/// Note: this might be lossy according to the docs from [`ToPrimitive`]:
/// > On the other hand, conversions with possible precision loss or truncation
/// are admitted, like an `f32` with a decimal part to an integer type, or
/// even a large `f64` saturating to `f32` infinity.
macro_rules! define_cast_to_primitive {
    ($ty:ty) => {
        define_cast_to_primitive! { $ty, $ty }
    };
    ($ty:ty, $wrapper_ty:ty) => {
        paste::paste! {
            #[inline(always)]
            pub fn [<to_ $ty>]<T>(elem: T) -> Result<$wrapper_ty>
            where
                T: ToPrimitive + std::fmt::Debug,
            {
                elem.[<to_ $ty>]()
                    .ok_or_else(|| {
                        RwError::from(InternalError(format!(
                            "Can't cast {:?} to {}",
                            elem,
                            std::any::type_name::<$ty>()
                        )))
                    })
                    .map(Into::into)
            }
        }
    };
}

define_cast_to_primitive! { i16 }
define_cast_to_primitive! { i32 }
define_cast_to_primitive! { i64 }
define_cast_to_primitive! { f32, OrderedF32 }
define_cast_to_primitive! { f64, OrderedF64 }

// In postgresSql, the behavior of casting decimal to integer is rounding.
// We should write them separately
#[inline(always)]
pub fn dec_to_i16(elem: Decimal) -> Result<i16> {
    to_i16(elem.round_dp(0))
}

#[inline(always)]
pub fn dec_to_i32(elem: Decimal) -> Result<i32> {
    to_i32(elem.round_dp(0))
}

#[inline(always)]
pub fn dec_to_i64(elem: Decimal) -> Result<i64> {
    to_i64(elem.round_dp(0))
}

#[inline(always)]
pub fn general_cast<T1, T2>(elem: T1) -> Result<T2>
where
    T1: TryInto<T2> + std::fmt::Debug + Copy,
    <T1 as TryInto<T2>>::Error: std::fmt::Display,
{
    elem.try_into().map_err(|e| {
        RwError::from(InternalError(format!(
            "Can't cast {:?} to {:?}: {}",
            &elem,
            type_name::<T2>(),
            e
        )))
    })
}

#[inline(always)]
pub fn str_to_bool(input: &str) -> Result<bool> {
    let trimmed_input = input.trim();
    if TRUE_BOOL_LITERALS
        .iter()
        .any(|s| s.eq_ignore_ascii_case(trimmed_input))
    {
        Ok(true)
    } else if FALSE_BOOL_LITERALS
        .iter()
        .any(|s| trimmed_input.eq_ignore_ascii_case(*s))
    {
        Ok(false)
    } else {
        Err(InvalidInputSyntax(format!("'{}' is not a valid bool", input)).into())
    }
}

#[inline(always)]
pub fn bool_to_str(input: bool) -> Result<String> {
    match input {
        true => Ok("true".into()),
        false => Ok("false".into()),
    }
}

macro_rules! integer_to_bool {
    ($func_name:ident, $type:ty) => {
        #[inline(always)]
        pub fn $func_name(input: $type) -> Result<bool> {
            match input {
                0 => Ok(false),
                _ => Ok(true),
            }
        }
    };
}

integer_to_bool!(int16_to_bool, i16);
integer_to_bool!(int32_to_bool, i32);
integer_to_bool!(int64_to_bool, i64);
