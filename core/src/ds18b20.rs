use crate::temperature::{Celsius, ThermometerError};

pub const SCRATCHPAD_LEN: usize = 9;

pub const CONVERT_TEMPERATURE: u8 = 0x44;
pub const WRITE_SCRATCHPAD: u8 = 0x4E;
pub const READ_SCRATCHPAD: u8 = 0xBE;
pub const FAMILY_CODE: u8 = 0x28;

pub const ALARM_HIGH_DEFAULT: u8 = 0x4B;
pub const ALARM_LOW_DEFAULT: u8 = 0x46;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Resolution {
    Bits9,
    Bits10,
    Bits11,
    Bits12,
}

impl Resolution {
    pub const fn config_byte(self) -> u8 {
        match self {
            Self::Bits9 => 0x1F,
            Self::Bits10 => 0x3F,
            Self::Bits11 => 0x5F,
            Self::Bits12 => 0x7F,
        }
    }

    pub const fn conversion_ms(self) -> u32 {
        match self {
            Self::Bits9 => 94,
            Self::Bits10 => 188,
            Self::Bits11 => 375,
            Self::Bits12 => 750,
        }
    }

    pub const fn step(self) -> Celsius {
        Celsius::from_sixteenths(match self {
            Self::Bits9 => 8,
            Self::Bits10 => 4,
            Self::Bits11 => 2,
            Self::Bits12 => 1,
        })
    }
}

pub fn parse_scratchpad(scratchpad: &[u8; SCRATCHPAD_LEN]) -> Result<Celsius, ThermometerError> {
    if crc8(scratchpad) != 0 {
        return Err(ThermometerError::Crc);
    }

    Ok(Celsius::from_sixteenths(i16::from_le_bytes([
        scratchpad[0],
        scratchpad[1],
    ])))
}

fn crc8(bytes: &[u8]) -> u8 {
    let mut crc = 0u8;

    for byte in bytes {
        let mut byte = *byte;

        for _ in 0..8 {
            let mix = (crc ^ byte) & 1;
            crc >>= 1;

            if mix != 0 {
                crc ^= 0x8C;
            }

            byte >>= 1;
        }
    }

    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAIL: [u8; 6] = [0x4B, 0x46, 0x7F, 0xFF, 0x0C, 0x10];

    fn scratchpad(lsb: u8, msb: u8, crc: u8) -> [u8; SCRATCHPAD_LEN] {
        let mut bytes = [0u8; SCRATCHPAD_LEN];
        bytes[0] = lsb;
        bytes[1] = msb;
        bytes[2..8].copy_from_slice(&TAIL);
        bytes[8] = crc;

        bytes
    }

    #[test]
    fn the_power_on_scratchpad_reads_85_degrees() {
        let parsed = parse_scratchpad(&scratchpad(0x50, 0x05, 0x1C));

        assert_eq!(parsed, Ok(Celsius::from_sixteenths(1360)));
        assert_eq!(parsed.unwrap().millidegrees(), 85_000);
    }

    #[test]
    fn positive_and_negative_temperatures_decode() {
        assert_eq!(
            parse_scratchpad(&scratchpad(0x91, 0x01, 0x70))
                .unwrap()
                .microdegrees(),
            25_062_500
        );
        assert_eq!(
            parse_scratchpad(&scratchpad(0x6F, 0xFE, 0xE8))
                .unwrap()
                .microdegrees(),
            -25_062_500
        );
        assert_eq!(
            parse_scratchpad(&scratchpad(0x90, 0xFC, 0x4F))
                .unwrap()
                .millidegrees(),
            -55_000
        );
    }

    #[test]
    fn a_corrupted_scratchpad_is_rejected() {
        let mut bytes = scratchpad(0x50, 0x05, 0x1C);
        bytes[1] ^= 0x01;

        assert_eq!(parse_scratchpad(&bytes), Err(ThermometerError::Crc));
    }

    #[test]
    fn a_wrong_checksum_is_rejected() {
        assert_eq!(
            parse_scratchpad(&scratchpad(0x50, 0x05, 0x1D)),
            Err(ThermometerError::Crc)
        );
    }

    #[test]
    fn resolutions_match_the_datasheet_config_register() {
        assert_eq!(Resolution::Bits9.config_byte(), 0x1F);
        assert_eq!(Resolution::Bits10.config_byte(), 0x3F);
        assert_eq!(Resolution::Bits11.config_byte(), 0x5F);
        assert_eq!(Resolution::Bits12.config_byte(), 0x7F);
    }

    #[test]
    fn conversion_time_halves_with_every_dropped_bit() {
        assert_eq!(Resolution::Bits12.conversion_ms(), 750);
        assert_eq!(Resolution::Bits11.conversion_ms(), 375);
        assert_eq!(Resolution::Bits10.conversion_ms(), 188);
        assert_eq!(Resolution::Bits9.conversion_ms(), 94);
    }

    #[test]
    fn the_step_is_the_smallest_distinguishable_change() {
        assert_eq!(Resolution::Bits12.step().millidegrees(), 63);
        assert_eq!(Resolution::Bits11.step().millidegrees(), 125);
        assert_eq!(Resolution::Bits10.step().millidegrees(), 250);
        assert_eq!(Resolution::Bits9.step().millidegrees(), 500);
    }

    #[test]
    fn the_reserved_tail_carries_the_configured_resolution() {
        assert_eq!(TAIL[2], Resolution::Bits12.config_byte());
    }

    #[test]
    fn a_floating_bus_is_rejected() {
        assert_eq!(
            parse_scratchpad(&[0xFF; SCRATCHPAD_LEN]),
            Err(ThermometerError::Crc)
        );
    }
}
