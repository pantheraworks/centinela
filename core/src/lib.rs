#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod temperature;

pub use temperature::{Celsius, Thermometer, ThermometerError};
