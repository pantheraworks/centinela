use centinela_core::ds18b20::{
    parse_scratchpad, Resolution, ALARM_HIGH_DEFAULT, ALARM_LOW_DEFAULT, CONVERT_TEMPERATURE,
    FAMILY_CODE, READ_SCRATCHPAD, SCRATCHPAD_LEN, WRITE_SCRATCHPAD,
};
use centinela_core::{Celsius, Thermometer, ThermometerError};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{InputPin, OutputPin};
use esp_idf_svc::hal::onewire::{OWAddress, OWCommand, OWDriver};
use esp_idf_svc::sys::{EspError, ESP_ERR_NOT_FOUND, ESP_ERR_TIMEOUT};

const CONVERSION_MARGIN_MS: u32 = 50;

pub struct Ds18b20<'d> {
    bus: OWDriver<'d>,
    address: Option<OWAddress>,
    resolution: Resolution,
}

impl<'d> Ds18b20<'d> {
    pub fn new(
        pin: impl InputPin + OutputPin + 'd,
        resolution: Resolution,
    ) -> Result<Self, EspError> {
        Ok(Self {
            bus: OWDriver::new(pin)?,
            address: None,
            resolution,
        })
    }

    fn address(&mut self) -> Result<OWAddress, ThermometerError> {
        if let Some(address) = self.address {
            return Ok(address);
        }

        let address = self.discover()?;
        log::info!("DS18B20 found at {:#018x}", address.address());

        self.configure(address)?;
        self.address = Some(address);

        Ok(address)
    }

    fn discover(&mut self) -> Result<OWAddress, ThermometerError> {
        let mut search = self
            .bus
            .search()
            .map_err(|error| classify("start search", error))?;

        let address = search
            .next()
            .ok_or(ThermometerError::NotPresent)?
            .map_err(|error| classify("search", error))?;

        if address.family_code() != FAMILY_CODE {
            log::warn!(
                "device {:#018x} on the 1-Wire bus is not a DS18B20, family code {:#04x}",
                address.address(),
                address.family_code()
            );

            return Err(ThermometerError::NotPresent);
        }

        Ok(address)
    }

    fn configure(&self, address: OWAddress) -> Result<(), ThermometerError> {
        self.command(address, WRITE_SCRATCHPAD)
            .map_err(|error| classify("write scratchpad", error))?;

        self.bus
            .write(&[
                ALARM_HIGH_DEFAULT,
                ALARM_LOW_DEFAULT,
                self.resolution.config_byte(),
            ])
            .map_err(|error| classify("write scratchpad", error))?;

        log::info!(
            "resolution set to {:?}, {} ms per conversion, {} steps",
            self.resolution,
            self.resolution.conversion_ms(),
            self.resolution.step()
        );

        Ok(())
    }

    fn sample(&self, address: OWAddress) -> Result<Celsius, ThermometerError> {
        self.command(address, CONVERT_TEMPERATURE)
            .map_err(|error| classify("convert", error))?;

        FreeRtos::delay_ms(self.resolution.conversion_ms() + CONVERSION_MARGIN_MS);

        self.command(address, READ_SCRATCHPAD)
            .map_err(|error| classify("read scratchpad", error))?;

        let mut scratchpad = [0u8; SCRATCHPAD_LEN];
        self.bus
            .read(&mut scratchpad)
            .map_err(|error| classify("read scratchpad", error))?;

        parse_scratchpad(&scratchpad)
    }

    fn command(&self, address: OWAddress, command: u8) -> Result<(), EspError> {
        self.bus.reset()?;

        let mut frame = [0u8; 10];
        frame[0] = OWCommand::MatchRom as u8;
        frame[1..9].copy_from_slice(&address.address().to_le_bytes());
        frame[9] = command;

        self.bus.write(&frame)
    }
}

impl Thermometer for Ds18b20<'_> {
    fn read(&mut self) -> Result<Celsius, ThermometerError> {
        let address = self.address()?;

        match self.sample(address) {
            Ok(celsius) => Ok(celsius),
            Err(error) => {
                if error != ThermometerError::Crc {
                    self.address = None;
                }

                Err(error)
            }
        }
    }
}

fn classify(operation: &str, error: EspError) -> ThermometerError {
    log::warn!("1-Wire {operation} failed: {error}");

    match error.code() {
        ESP_ERR_TIMEOUT => ThermometerError::Timeout,
        ESP_ERR_NOT_FOUND => ThermometerError::NotPresent,
        _ => ThermometerError::Bus,
    }
}
