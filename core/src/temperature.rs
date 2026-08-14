use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Celsius(i16);

impl Celsius {
    pub const ZERO: Self = Self(0);

    pub const fn from_sixteenths(sixteenths: i16) -> Self {
        Self(sixteenths)
    }

    pub const fn sixteenths(self) -> i16 {
        self.0
    }

    pub const fn microdegrees(self) -> i32 {
        self.0 as i32 * 62_500
    }

    pub const fn millidegrees(self) -> i32 {
        let scaled = self.0 as i32 * 125;
        if scaled >= 0 {
            (scaled + 1) / 2
        } else {
            (scaled - 1) / 2
        }
    }

    pub const fn whole_degrees(self) -> i16 {
        self.0 / 16
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ThermometerError {
    #[error("no sensor present on the bus")]
    NotPresent,
    #[error("sensor payload failed its checksum")]
    Crc,
    #[error("sensor did not respond in time")]
    Timeout,
    #[error("sensor bus failure")]
    Bus,
}

pub trait Thermometer {
    fn read(&mut self) -> Result<Celsius, ThermometerError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    const DATASHEET_TABLE: [(i16, i32, i32); 10] = [
        (0x07D0u16 as i16, 125_000_000, 125_000),
        (0x0550u16 as i16, 85_000_000, 85_000),
        (0x0191u16 as i16, 25_062_500, 25_063),
        (0x00A2u16 as i16, 10_125_000, 10_125),
        (0x0008u16 as i16, 500_000, 500),
        (0x0000u16 as i16, 0, 0),
        (0xFFF8u16 as i16, -500_000, -500),
        (0xFF5Eu16 as i16, -10_125_000, -10_125),
        (0xFE6Fu16 as i16, -25_062_500, -25_063),
        (0xFC90u16 as i16, -55_000_000, -55_000),
    ];

    #[test]
    fn datasheet_values_convert_exactly_in_microdegrees() {
        for (sixteenths, microdegrees, _) in DATASHEET_TABLE {
            assert_eq!(
                Celsius::from_sixteenths(sixteenths).microdegrees(),
                microdegrees,
                "sixteenths {sixteenths}"
            );
        }
    }

    #[test]
    fn millidegrees_rounds_half_away_from_zero() {
        for (sixteenths, _, millidegrees) in DATASHEET_TABLE {
            assert_eq!(
                Celsius::from_sixteenths(sixteenths).millidegrees(),
                millidegrees,
                "sixteenths {sixteenths}"
            );
        }
    }

    #[test]
    fn sixteenths_survive_a_round_trip() {
        for (sixteenths, _, _) in DATASHEET_TABLE {
            assert_eq!(
                Celsius::from_sixteenths(sixteenths).sixteenths(),
                sixteenths
            );
        }
    }

    #[test]
    fn whole_degrees_truncate_towards_zero() {
        assert_eq!(Celsius::from_sixteenths(401).whole_degrees(), 25);
        assert_eq!(Celsius::from_sixteenths(-401).whole_degrees(), -25);
        assert_eq!(Celsius::ZERO.whole_degrees(), 0);
    }

    #[test]
    fn ordering_follows_the_temperature() {
        let mut readings = [
            Celsius::from_sixteenths(8),
            Celsius::from_sixteenths(-880),
            Celsius::ZERO,
        ];
        readings.sort();
        assert_eq!(
            readings,
            [
                Celsius::from_sixteenths(-880),
                Celsius::ZERO,
                Celsius::from_sixteenths(8),
            ]
        );
    }
}
