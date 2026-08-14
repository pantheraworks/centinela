mod ds18b20;

use centinela_core::Thermometer;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::peripherals::Peripherals;

use crate::ds18b20::Ds18b20;

const SAMPLE_PERIOD_MS: u32 = 1_000;

fn main() -> anyhow::Result<()> {
    centinela_esp::init_esp_idf();

    let peripherals = Peripherals::take()?;
    let mut thermometer = Ds18b20::new(peripherals.pins.gpio4)?;

    loop {
        match thermometer.read() {
            Ok(celsius) => log::info!("temperature: {celsius}"),
            Err(error) => {
                log::warn!("temperature unavailable: {error}; retrying in {SAMPLE_PERIOD_MS} ms")
            }
        }

        FreeRtos::delay_ms(SAMPLE_PERIOD_MS);
    }
}
