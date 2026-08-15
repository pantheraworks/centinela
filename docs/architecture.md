# Centinela — Architecture

## System

Two ESP32-C3 firmwares share one domain core:

- **centinela** — sensor node. DS18B20 temperature, button, screen. No WiFi.
- **torre** — gateway. Receives readings over the radio link, uplinks over WiFi. No sensors.

Hexagonal (ports and adapters). The dependency direction points inward: adapters depend on core, never the reverse.

## Power model

The sensor runs on battery and samples on a deep-sleep duty cycle. Deep sleep on the C3 clears RAM and restarts execution at `main`, so a cycle is a boot: the sensor holds no state between samples, and anything that must outlive a cycle — sequence counters, dedup state, the sensor's ROM address — belongs in RTC slow memory, which survives deep sleep but not a battery change.

The gateway runs on USB power. It keeps an always-on loop and RAM-resident state.

## Concurrency

Ports are traits and they block. ESP-IDF provides `std`, so `std::thread` maps to FreeRTOS tasks and a blocking wait yields the CPU rather than spinning: a thermometer read that occupies 375 ms in its own task freezes nothing. Concurrency comes from the RTOS, not from a cooperative loop in core.

## Crate layout

Cargo workspace with a virtual manifest at the repo root:

```
core/          centinela-core:  domain types, ports (traits), proto encode/decode,
                                application services. No ESP dependencies whatsoever.
                                `#![no_std]`, `#![forbid(unsafe_code)]`.
                                All unit tests live here.
esp/           centinela-esp:   shared ESP adapters. Depends on core + esp-idf-svc.
                                init_esp_idf(), logger, EspError mapping, radio link.
centinela/     sensor binary  + sensor-only adapters (1-Wire, button, screen).
torre/         gateway binary + gateway-only adapters (WiFi, MQTT/HTTP publisher).
```

The workspace exists for build isolation: two firmwares need two different ESP-IDF configurations, which one package cannot express. Host-test speed is a secondary benefit.

`[profile.release]` and `[profile.dev]` live in the **root** manifest, because profiles declared in workspace members are ignored with a warning.

`default-members = ["core"]` at the root keeps a bare `cargo build` or `cargo test` on the crate that builds for the host.

### Enforcing the dependency rule

An architecture rule that isn't machine-checked becomes a comment. CI asserts that the core's dependency tree contains no ESP crates:

```bash
cargo tree -p centinela-core --edges normal --target all --prefix none | grep -qE '^(esp-|embuild|embassy)'
```

`--target all` is what makes it airtight: an ESP dependency hidden behind a `cfg(target_os = "espidf")` gate passes the check on a host without it.

## Core

### Domain types

`Celsius` is a newtype over the DS18B20's native representation: a signed 16-bit value in 1/16 °C steps, which covers −55..125 °C losslessly. **Not `f32`.** The C3 is `riscv32imc` with no FPU, so every float operation is soft-float; fixed point also gives exact equality, which deduplication needs, and a portable wire encoding.

`Celsius` renders itself through `core::fmt::Display`, which keeps the arithmetic — sign-aware, so −0.5 °C reads `-0.500 °C` — in one host-tested place, needs no allocator, and composes with `write!` and `log`.

`Instant` is a monotonic `u64` millisecond count since boot, only ever compared or differenced. Points in time are `Instant`; durations are plain `u32` milliseconds. Keeping the two in separate types is what stops a deadline being passed where a delay was meant.

`Reading { node_id, boot_id, seq, celsius }`.

### Errors

Every port that can fail returns `Result` with a domain error enum owned by core, named after its port — `ThermometerError { NotPresent, Crc, Timeout, Bus }`, `ScreenError`, `UplinkError`, `PublishError`. The name follows the port because "sensor" denotes the node, so a `SensorError` would read as an error of the whole device. Infallible port signatures force adapters to panic or fabricate values, which is precisely the behaviour that cannot be tested.

**`thiserror` in core, `anyhow` in the binaries.** They belong to different layers. Core's errors are typed and matchable because the *policy* lives in core — "retry a `Crc`, treat `NotPresent` as fatal for this cycle" is only expressible over a closed set of variants, and `anyhow::Error` is type-erased, so core would have to downcast to make a decision it should be making directly. `thiserror` is a derive macro generating exactly the `Display` and `Error` impls we would otherwise write by hand, so it does not appear in the API. It is declared with `default-features = false`, which targets `core::error::Error` and keeps core `no_std`; `anyhow` would additionally want `alloc` and a global allocator.

The firmware binaries and adapters use `anyhow`, where type erasure is the point: `main` and setup code only log and bail.

**No port returns `anyhow::Error`, and no core error carries an `EspError`.** Translation happens at the adapter boundary through an explicit match rather than a `#[from]` impl, because one `EspError` maps to `Timeout` or `Bus` depending on context: the adapter matches, logs the underlying code for the serial console, and returns the domain class. A blanket `From` would collapse that distinction and drag ESP types into core.

### Ports

Phrased in domain terms, no hardware types in signatures:

```rust
trait Thermometer {
    fn read(&mut self) -> Result<Celsius, ThermometerError>;
}

trait Screen  { fn show(&mut self, view: &SensorView) -> Result<(), ScreenError>; }
trait Button  { fn is_pressed(&mut self) -> bool; }
trait Clock   { fn now(&self) -> Instant; }
trait Uplink  { fn send_reading(&mut self, reading: &Reading) -> Result<(), UplinkError>; }
trait Publisher { fn publish(&mut self, reading: &Reading) -> Result<(), PublishError>; }
```

Ports carry no doc comments, by project convention; their contracts live here.

**`Thermometer::read` blocks and returns either a value or an error.** Everything about how the reading is obtained is internal to the adapter: issuing the convert command, waiting out the 94 to 750 ms the configured resolution implies, reading the scratchpad, checking the CRC. Resolution stays out of the port, so core receives no device parameter to reason about, and the caller has no ordering contract to get wrong.

**Whoever calls `read` decides what the result means.** Sampling cadence, whether a `Crc` failure is retried, and what a stale reading does to the screen are policy, and policy sits above the port.

**`Clock` has only `now()`.** Sleeping belongs to whoever owns the thread. Core needs to know what time it is, for reading timestamps and retry backoff, not how to wait.

**`Screen::show` takes a domain view struct, not a string.** Formatting and layout are adapter concerns, and a `&str` argument would drag `alloc` into core.

**WiFi is not a port.** The port is `Publisher`. Association, DHCP, and session handling are implementation details inside the adapter. A WiFi trait would leak infrastructure into the domain.

### Injecting ports

Ports are monomorphised through generics.

A service that needs several ports takes **a plain generic struct with public fields, not a trait with accessors.** An accessor trait fails on borrows: a `&mut self` getter means nothing else on the bundle can be touched while its result is alive, so touching two ports in consecutive lines needs temporaries, scopes, or a `split()`. Struct fields are borrow-checked independently.

**Ports are passed as arguments, not held as fields.** Services own state and policy only, so they carry no type parameters and the generics live on the methods. Tests keep ownership of their fakes and can assert on them afterwards.

### Sensor cycle

One wake produces one reading. `main` opens the 1-Wire bus, takes a reading, logs it, and deep-sleeps the balance of a 10 s period through `DeepSleep::wakeup_on_timer`.

The balance is computed from `esp_timer_get_time`, which counts from chip reset rather than from the start of `main`, so the roughly 300 ms of bootloader and init is charged against the period and the cadence holds instead of drifting to the period plus the awake time.

A button on GPIO0, pulled up and shorted to ground when pressed, is armed as a second wake source through `DeepSleep::wakeup_on_gpio` at level low. Both wake sources run the same path, so a press produces a reading on demand and re-anchors the period from that boot; the cadence is a floor on sample age, not a schedule. Before sleeping, `main` waits for the pin to return high, otherwise a held button re-wakes the chip the instant it sleeps.

A wake that is neither the timer nor the GPIO is a reset or a power-on, and `main` then idles for 10 s before sampling so a flash attempt has a window to catch the chip.

A timer or button cycle is awake for roughly 600 ms, which is shorter than the second or more macOS needs to re-enumerate the USB-Serial-JTAG device after a sleep, so a monitor reopening the port usually misses the output: nothing is buffered for a host that is not yet attached. Steady-state cycles are therefore only intermittently observable over USB, and the reset path's window is what a monitor can rely on.

A failed read costs the node one cycle: it is logged, and the next wake is the retry. Retrying within a wake would mean dropping the driver and reopening the bus, since a reset that fails with `ESP_ERR_TIMEOUT` leaves the RMT receive channel disabled and every later call on that driver returns `ESP_ERR_INVALID_STATE`. `Ds18b20::new` opens the RMT bus and nothing else, so it fails only on a bad pin or an exhausted RMT channel, both fatal. Device discovery is lazy: a read without a cached address enumerates the bus first, and any failure other than `Crc` clears the cache, since a CRC failure proves the transport works while the rest cast doubt on the address itself.

A reset timeout poisons the bus object. `onewire_bus_rmt_reset` leaves the RMT receive channel disabled when its queue wait expires, and every subsequent call on that `OWDriver` fails with `ESP_ERR_INVALID_STATE`. Recovery is a new `OWDriver`, which the deep-sleep cycle provides for free by rebooting.

### Gateway service

Driven from outside, through **two** entry points:

```rust
fn on_reading(&mut self, reading: Reading, now: Instant);
fn on_tick(&mut self, now: Instant);
```

`on_reading` alone is insufficient: buffering and retry must fire in the *absence* of input. If an uplink fails and the sensor then goes quiet, nothing would ever call `on_reading` again and buffered readings would sit forever. The gateway loop does a receive with timeout and dispatches to whichever applies. Both are directly callable in tests with fabricated times.

**The radio receive callback must not call `on_reading` directly.** The ESP-NOW receive callback runs in the WiFi task context; publishing MQTT from there is not permissible. The path is: callback → bounded queue → main loop drains → `decode` → `on_reading`. The queue's overflow policy (drop oldest) is a domain decision recorded in core rather than left to a FreeRTOS queue default.

### Deduplication policy

Key is `(node_id, boot_id, seq)`. `boot_id` is randomised once per sensor boot and costs one byte on the wire; it disambiguates the reboot case unconditionally. Without it, a node that restarts resets `seq` to 0 and every subsequent reading looks stale forever. On a battery sensor that reboots on every deep-sleep cycle, `boot_id` and `seq` both live in RTC slow memory.

`seq` is `u16` and compared with serial-number arithmetic — `seq.wrapping_sub(last) < 0x8000` means newer — so wraparound is handled rather than avoided. This is a pure function and, after the protocol round-trip, the second highest-value test in the repo.

### Buffering and retry

Bounded ring buffer of readings, drop-oldest on overflow. `on_reading` enqueues then attempts a flush; `on_tick` attempts a flush when the backoff interval has elapsed. Backoff is exponential with a cap, all driven by `Instant` arguments.

### Wire protocol

`encode` / `decode` for `Reading`, little-endian, leading version byte: `version: u8`, `node_id: u8`, `boot_id: u8`, `seq: u16`, `raw: i16` — 7 bytes, well inside the ~250-byte ESP-NOW payload.

`decode` returns `Result<Reading, DecodeError>` with tests for truncated input, trailing bytes, and unknown version. The round-trip test is the highest-value shared test in the repo, since both firmwares depend on it agreeing.

ESP-NOW provides a link-layer CRC and a send-status acknowledgement, so the application adds no CRC of its own — and that ack is what makes `Uplink::send_reading` meaningfully fallible and worth retrying at the sensor.

### Device knowledge in core

DS18B20 decoding lives in `core::ds18b20` — `parse_scratchpad(&[u8; 9]) -> Result<Celsius, ThermometerError>`, including the Dallas CRC-8 over all nine bytes, which is zero for a valid scratchpad. Adapters move bytes; they do not interpret them. The command bytes and the family code live there too, so the adapter names nothing magic.

`Resolution` carries the four rows of the datasheet's config register: `config_byte()` (`0x1F`, `0x3F`, `0x5F`, `0x7F`), `conversion_ms()` (94, 188, 375, 750), and `step()`, the smallest distinguishable change, as a `Celsius`. Holding them in one enum keeps a config byte and its conversion time from drifting apart, and every row is host-tested.

Device-specific decoding sits in its own module, away from the port, which stays generic: `Thermometer` says nothing about DS18B20, and a second part adds a module without touching it.

The CRC check is what detects a disconnected bus, with no special case: a floating line reads all `0xFF`, whose checksum does not come to zero. The power-on value of 85 °C parses as the ordinary temperature it is.

## Shared radio link

The sensor sends and the gateway receives, but peer setup and channel configuration are shared. `centinela-esp` exposes a shared configuration/initialisation type plus **separate sender and receiver handles**, so each firmware constructs only what it needs.

Relying on LTO to strip the unused half is a hope, not a mechanism: if a single constructor registers a receive callback, the sensor links the entire receive path and its queue regardless of optimisation level, and dev builds carry both in any case.

Framing is pure and lives in core alongside the proto — `centinela-esp` shares only transport configuration. The transport choice (ESP-NOW vs BLE) does not reach core.

## Build system

`esp-idf-sys` reads its configuration **only from the root crate's `Cargo.toml`** — the package in the workspace directory. A virtual manifest has none, so each firmware build names it:

```bash
ESP_IDF_SYS_ROOT_CRATE=centinela cargo build -p centinela --release
ESP_IDF_SYS_ROOT_CRATE=torre     cargo build -p torre     --release
```

A build that omits it gets no `[package.metadata.esp-idf-sys]` at all, which surfaces as a missing HAL module rather than as a configuration error: without `onewire_bus`, `esp_idf_svc::hal::onewire` does not exist and the firmware fails to compile on an unresolved import.

`extra_components` follows a looser rule — collected from the root crate and all **direct** dependencies. Components needed by both firmwares are declared once in `centinela-esp`; role-specific ones stay in that role's own manifest, in particular `onewire_bus`, which belongs to `centinela` only. Note the fragility: the rule is *direct* dependencies only, so components declared in a transitive crate are silently dropped. If `centinela-esp` is ever split further, its component declarations must be re-hoisted.

The remote-component lock file is written to the workspace directory and named after the ESP-IDF version rather than the crate, so two firmwares with different remote components rewrite it in turn. It is gitignored.

### `sdkconfig` paths are workspace-relative

`ESP_IDF_SYS_ROOT_CRATE` selects whose `[package.metadata.esp-idf-sys]` applies, **but relative paths inside that table resolve against the workspace directory, not the crate directory.** A bare `sdkconfig.defaults` in `centinela/Cargo.toml` therefore points at the repo root, and both firmwares silently share one file. Each firmware spells out the path, and can layer role settings on a shared base:

```toml
[package.metadata.esp-idf-sys]
esp_idf_sdkconfig_defaults = ["sdkconfig.defaults", "centinela/sdkconfig.defaults"]
```

Later entries override earlier ones, so the root file keeps the common stack-size settings and the role file carries radio, WiFi, and MQTT configuration. A role file added without the corresponding manifest entry is silently ignored, since the default is a single bare path.

Watch the inconsistency: `extra_components` paths use the **opposite** rule and resolve against the directory of the `Cargo.toml` that declares them.

Configuration shared by both firmwares (`ESP_IDF_VERSION`, `ESP_IDF_TOOLS_INSTALL_DIR`) stays in `[env]` in `.cargo/config.toml`, where it applies to every build and takes precedence over metadata. Only role-specific keys go in package metadata, which avoids duplicating the whole table twice.

### Partition tables are not an `sdkconfig` matter

`esp-idf-sys` does not consume `CONFIG_PARTITION_TABLE_CUSTOM`; a custom table is a flashing argument (`espflash flash --partition-table partitions.csv`). Since the workspace-level `runner` string in `.cargo/config.toml` cannot differ per firmware, flashing goes through the per-firmware scripts too.

### Per-firmware target directories

Two firmwares with different ESP-IDF inputs — a differing `sdkconfig`, or the differing set of `extra_components` that `onewire_bus` creates — sharing one `target/` means alternating builds rebuild ESP-IDF each time. Each script sets its own `CARGO_TARGET_DIR`. Because embuild locates the workspace directory *from* the target directory, moving it requires pinning `CARGO_WORKSPACE_DIR` to the repo root explicitly — otherwise relative `sdkconfig` paths and the tools install directory resolve somewhere unintended.

`scripts/centinela.sh` and `scripts/torre.sh` are thin wrappers over a shared `scripts/firmware.sh` that set `ESP_IDF_SYS_ROOT_CRATE`, `CARGO_TARGET_DIR`, and `CARGO_WORKSPACE_DIR` and forward the rest to `cargo`. These scripts are load-bearing, not convenience: `cargo` invoked directly against a firmware package builds with none of the three.

### Single `.cargo/config.toml`

Both boards are ESP32-C3, so one workspace-root config covers `target`, `MCU`, linker, and `rustflags`. If the gateway ever moves to a different chip, each firmware needs its own config and must be built from its own directory. `[unstable] build-std` cannot be target-scoped, which is why host test runs override it (see Tooling).

## Tooling

Host tests are `cargo test -p centinela-core --target <host triple>`, which pulls in neither `embuild` nor `esp-idf-sys`. `test.sh` carries the `--config 'unstable.build-std=[]'` override and host-triple detection, and includes integration tests under `core/tests/` and doc tests.

`monitor.sh` reads the serial device directly rather than through `espflash monitor`, so it neither resets the chip nor dies when deep sleep drops the port; it waits for the device to reappear and resumes. Only one process can usefully read the port, and a reader that holds it starves espflash's handshake into a `Failed to connect`, so `firmware.sh` kills any running monitor and anything else holding `/dev/cu.usbmodem*` before it hands off to `cargo run`. The monitor forwards the signal to its `cat` child, since killing the loop alone would orphan a reader still holding the port.

`rust-toolchain.toml` pins `channel = "esp"`. The C3 is RISC-V, so the `esp-rs/xtensa-toolchain` action is a misnomer here; it is used because it installs that channel and `ldproxy`.

### CI

- **Firmware matrix** over `{centinela, torre}` × `{build, clippy}`, each invoking the per-firmware script so `ESP_IDF_SYS_ROOT_CRATE` is always set. Clippy is per-package for the same reason as build, and without `--all-features`, which produces meaningless `esp-idf-svc` feature combinations.
- **Format** once at the root — `cargo fmt --all --check` compiles nothing and needs no ESP toolchain.
- **Host tests** on stable, no ESP toolchain, running `test.sh`.
- **Fitness check** asserting the core dependency tree is free of ESP crates.

## Hardware facts

DS18B20 data line on GPIO4 with a 4.7 kΩ pull-up **to 3.3 V** (not 5 V — the C3 is not 5 V tolerant); VDD and pull-up both on 3.3 V. Button on GPIO0 to ground, pulled up. On the C3, avoid GPIO2, 8 and 9 (strapping), 11 through 17 (flash and `VDD_SPI`), and 20 and 21 (console UART). Only GPIO0 through 5 are RTC-capable, so a deep-sleep wake pin has to be one of them; GPIO0 doubles as `XTAL_32K_P` and is free here because no 32 kHz crystal is populated.

**The external pull-up is not optional.** `OWDriver::new` builds its bus config with `flags: Default::default()`, leaving `en_pull_up` clear, so the component calls `gpio_set_pull_mode(pin, GPIO_FLOATING)` on a pin it has already switched to open drain. Nothing on the chip drives the line high; the resistor is what does.

The two electrical failures are distinguishable by error code. A floating line never returns high, so the RMT receiver never observes the idle period that ends a reception, never completes, and `onewire_bus_rmt_reset` fails its one-second queue wait with `ESP_ERR_TIMEOUT`. A pulled-up line with no device answering completes the reception and fails only the presence-pulse check, giving `ESP_ERR_NOT_FOUND`. So `ESP_ERR_TIMEOUT` on reset points at wiring — an unpowered pull-up rail, DQ shorted low, or VDD and GND reversed — while `ESP_ERR_NOT_FOUND` means the bus is electrically alive and the sensor is not responding.

The 1-Wire driver is `esp_idf_svc::hal::onewire` (`OWDriver`, `OWAddress`, `OWCommand`), which exists only when the `onewire_bus` remote component is declared:

```toml
[[package.metadata.esp-idf-sys.extra_components]]
remote_component = { name = "onewire_bus", version = "^1.0.4" }
```

Adding it requires `cargo clean` before the module appears. `search()` yields `Result` items and ends with `None` on `ESP_ERR_NOT_FOUND`, so an exhausted bus and a bus error are separate cases.

The HAL has no `Ds18b20` type; convert (`0x44`), write scratchpad (`0x4E`), and read scratchpad (`0xBE`) are implemented by hand, following `esp-idf-hal`'s `rmt_onewire_temperature.rs` example. The `ds18b20` crate on crates.io targets `embedded-hal` 0.2 bit-banging and is incompatible with this RMT-backed bus.

A `WRITE_SCRATCHPAD` lands in the sensor's volatile scratchpad, so the adapter writes the resolution on every boot, which a deep-sleep cycle makes cheap. Surviving a sensor power cycle would take `COPY_SCRATCHPAD` (`0x48`) burning it into EEPROM, at the cost of write cycles.

Deep sleep powers down the USB-Serial-JTAG peripheral, so the serial monitor drops on every cycle and a flash attempt has only the awake window to catch the chip. Holding **BOOT** (GPIO9) while tapping **RESET** enters download mode and holds it there regardless.

On the gateway, WiFi and BLE share one radio. ESP-NOW alongside a WiFi station connection must run on the access point's channel, which means the sensor's channel is dictated by the gateway's AP — if the AP moves channel, the link breaks until the sensor follows. WiFi power save must be disabled on the gateway (`WIFI_PS_NONE`) or it will miss ESP-NOW frames while dozing.
