mod ds18b20;

use std::time::Duration;

use centinela_core::ds18b20::Resolution;
use centinela_core::Thermometer;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::reset::WakeupReason;
use esp_idf_svc::hal::sleep::DeepSleep;
use esp_idf_svc::sys::esp_timer_get_time;

use crate::ds18b20::Ds18b20;

const SAMPLE_PERIOD: Duration = Duration::from_secs(30);
const FLASH_WINDOW: Duration = Duration::from_secs(10);
const MINIMUM_SLEEP: Duration = Duration::from_secs(1);
const RESOLUTION: Resolution = Resolution::Bits11;

fn main() -> anyhow::Result<()> {
    centinela_esp::init_esp_idf();

    let wakeup = WakeupReason::get();
    log::info!("woke on {wakeup:?}");

    if wakeup != WakeupReason::Timer {
        log::info!(
            "holding the serial port open for {} s",
            FLASH_WINDOW.as_secs()
        );
        std::thread::sleep(FLASH_WINDOW);
    }

    let peripherals = Peripherals::take()?;
    let mut thermometer = Ds18b20::new(peripherals.pins.gpio4, RESOLUTION)?;

    match thermometer.read() {
        Ok(celsius) => log::info!("temperature: {celsius}"),
        Err(error) => log::warn!("temperature unavailable: {error}"),
    }

    let awake = since_boot();
    let sleep_for = SAMPLE_PERIOD.saturating_sub(awake).max(MINIMUM_SLEEP);

    log::info!(
        "awake for {} ms, sleeping for {} ms",
        awake.as_millis(),
        sleep_for.as_millis()
    );

    DeepSleep::new()?.wakeup_on_timer(sleep_for)?.enter()
}

fn since_boot() -> Duration {
    Duration::from_micros(unsafe { esp_timer_get_time() }.max(0) as u64)
}
