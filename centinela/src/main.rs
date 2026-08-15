mod ds18b20;

use std::time::Duration;

use centinela_core::ds18b20::Resolution;
use centinela_core::log_buffer::Level as Severity;
use centinela_core::{LogBuffer, Thermometer};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{Level, PinDriver, Pull};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::reset::WakeupReason;
use esp_idf_svc::hal::sleep::DeepSleep;
use esp_idf_svc::sys::{
    esp_sleep_source_t_ESP_SLEEP_WAKEUP_GPIO as WAKEUP_GPIO, esp_timer_get_time,
};

use crate::ds18b20::Ds18b20;

const SAMPLE_PERIOD: Duration = Duration::from_secs(10);
const FLASH_WINDOW: Duration = Duration::from_secs(10);
const MINIMUM_SLEEP: Duration = Duration::from_secs(1);
const BUTTON_POLL_MS: u32 = 20;
const RESOLUTION: Resolution = Resolution::Bits11;
const JOURNAL_BYTES: usize = 512;

fn main() -> anyhow::Result<()> {
    centinela_esp::init_esp_idf();

    let wakeup = WakeupReason::get();

    if wakeup == WakeupReason::Unknown {
        log::info!(
            "holding the serial port open for {} s",
            FLASH_WINDOW.as_secs()
        );
        std::thread::sleep(FLASH_WINDOW);
    }

    let mut journal = LogBuffer::<JOURNAL_BYTES>::new();

    journal.info(format_args!(
        "woke on {}",
        match wakeup {
            WakeupReason::Timer => "timer",
            WakeupReason::Other(WAKEUP_GPIO) => "button",
            _ => "reset",
        }
    ));

    let peripherals = Peripherals::take()?;
    let button = PinDriver::input(peripherals.pins.gpio0, Pull::Up)?;
    let mut thermometer = Ds18b20::new(peripherals.pins.gpio4, RESOLUTION)?;

    match thermometer.read() {
        Ok(celsius) => journal.info(format_args!("temperature: {celsius}")),
        Err(error) => journal.warn(format_args!("temperature unavailable: {error}")),
    }

    while button.is_low() {
        FreeRtos::delay_ms(BUTTON_POLL_MS);
    }

    let awake = since_boot();
    let sleep_for = SAMPLE_PERIOD.saturating_sub(awake).max(MINIMUM_SLEEP);

    journal.info(format_args!(
        "awake for {} ms, sleeping for {} ms",
        awake.as_millis(),
        sleep_for.as_millis()
    ));

    flush(&journal);

    DeepSleep::new()?
        .wakeup_on_timer(sleep_for)?
        .wakeup_on_gpio(&button, Level::Low)?
        .enter()
}

fn flush(journal: &LogBuffer<JOURNAL_BYTES>) {
    for (severity, text) in journal.entries() {
        match severity {
            Severity::Info => log::info!("{text}"),
            Severity::Warn => log::warn!("{text}"),
        }
    }

    if journal.dropped() > 0 {
        log::warn!(
            "{} records did not fit in {JOURNAL_BYTES} bytes",
            journal.dropped()
        );
    }
}

fn since_boot() -> Duration {
    Duration::from_micros(unsafe { esp_timer_get_time() }.max(0) as u64)
}
