# Centinela — Architecture

## System

Two ESP32-C3 firmwares sharing one domain core:

- **centinela** — sensor node. DS18B20 temperature, button, screen. No WiFi.
- **torre** — gateway. Receives readings over the radio link, uplinks over WiFi. No sensors.

Hexagonal (ports and adapters). Dependency direction points inward: adapters depend on core, never the reverse.

## Recorded decisions

- **The sensor is mains/USB powered.** It is always on, so a long-lived `tick()` loop is valid and sequence counters live in RAM. If this ever changes to battery with deep sleep, the loop model breaks: `seq` and dedup state would have to survive reboots in RTC slow memory and the core would stop being a loop. Revisit this document before adding deep sleep.
- **Ports stay traits, with two corrections to their shape**: the temperature port is two-phase, and both application services accept a time-driven entry point. See below.
- **The real driver for the workspace split is build isolation, not host testability.** `esp-idf-svc` is already target-gated in the current manifest and `test.sh` already defeats `build-std`; host tests break today only because `src/devices.rs` imports `esp_idf_svc` unconditionally from the library, which a `cfg` attribute would fix. What genuinely cannot work in one package is two firmwares needing two different ESP-IDF configurations. Host-test speed is a real but secondary benefit.

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

`[profile.release]` and `[profile.dev]` must move to the **root** manifest. Profiles declared in workspace members are ignored with a warning, so the current `opt-level = "s"` would silently stop applying after the split.

Set `default-members = ["core"]` at the root so a bare `cargo build` or `cargo test` does the safe thing instead of trying to build both firmwares at once (see Build system).

### Enforcing the dependency rule

An architecture rule that isn't machine-checked becomes a comment. CI asserts that the core's dependency tree contains no ESP crates:

```bash
! cargo tree -p centinela-core --edges normal --prefix none | grep -qE '^(esp-|embuild)'
```

## Core

### Domain types

`Celsius` is a newtype over the DS18B20's native representation: a signed 16-bit value in 1/16 °C steps, which covers −55..125 °C losslessly. **Not `f32`.** The C3 is `riscv32imc` with no FPU, so every float operation is soft-float; fixed point also gives exact equality, which deduplication needs, and a portable wire encoding. Conversion to a display string happens in the screen adapter, not in core.

`Millis` is a monotonic `u64` millisecond count, only ever compared as a difference.

`Reading { node_id, boot_id, seq, celsius }`.

### Errors

Every port that can fail returns `Result` with a domain error enum owned by core — `SensorError { NotPresent, Crc, Timeout }`, `ScreenError`, `UplinkError`, `PublishError`. `centinela-esp` maps `EspError` into these. Infallible port signatures force adapters to panic or fabricate values, which is precisely the behaviour that cannot be tested.

### Ports

Phrased in domain terms, no hardware types in signatures:

```rust
trait TemperatureSensor {
    fn start_conversion(&mut self) -> Result<(), SensorError>;
    fn read_conversion(&mut self) -> Result<Celsius, SensorError>;
}

trait Screen  { fn show(&mut self, view: &SensorView) -> Result<(), ScreenError>; }
trait Button  { fn is_pressed(&mut self) -> bool; }
trait Clock   { fn now_ms(&self) -> Millis; fn sleep_ms(&mut self, ms: u32); }
trait Uplink  { fn send_reading(&mut self, reading: &Reading) -> Result<(), UplinkError>; }
trait Publisher { fn publish(&mut self, reading: &Reading) -> Result<(), PublishError>; }
```

**The temperature port is two-phase because the hardware is.** A 12-bit DS18B20 conversion takes 750 ms; a blocking `read() -> Celsius` freezes the button and the screen for three quarters of a second per sample. Core owns the `CONVERSION_MS` constant and decides when enough time has elapsed. Port shape follows the device rather than flattening it.

**`Clock` exposes `now_ms()`, not just `sleep_ms()`.** Conversion timing, deduplication windows, and retry backoff all need a monotonic reading; with only `sleep_ms` none of them can be tested without real sleeps, which defeats the point of the port.

**`Screen::show` takes a domain view struct, not a string.** Formatting and layout are adapter concerns, and a `&str` argument would drag `alloc` into core.

**WiFi is not a port.** The port is `Publisher`. Association, DHCP, and session handling are implementation details inside the adapter. A WiFi trait would leak infrastructure into the domain.

### Devices bundle

A `Devices` trait with associated types bundles the per-role ports so application structs carry one generic parameter instead of five. The firmware crates provide the concrete implementation. The **test** bundle is a plain struct generic over its ports, so a fake sensor can be combined with a real clock without writing a new impl for every combination — with associated types alone, partial fakes are combinatorial.

### Sensor service

Loop-driven `tick()`. State machine: idle → conversion started at `t` → conversion readable at `t + CONVERSION_MS` → send. The button and screen are serviced on every tick regardless of conversion state, which is what the two-phase port buys.

### Gateway service

Driven from outside, but by **two** entry points:

```rust
fn on_reading(&mut self, reading: Reading, now: Millis);
fn on_tick(&mut self, now: Millis);
```

`on_reading` alone is insufficient: buffering and retry must fire in the *absence* of input. If an uplink fails and the sensor then goes quiet, nothing would ever call `on_reading` again and buffered readings would sit forever. The gateway loop does a receive with timeout and dispatches to whichever applies. Both are directly callable in tests with fabricated times.

**The radio receive callback must not call `on_reading` directly.** The ESP-NOW receive callback runs in the WiFi task context; publishing MQTT from there is not permissible. The path is: callback → bounded queue → main loop drains → `decode` → `on_reading`. The queue's overflow policy (drop oldest) is a domain decision recorded in core rather than left to a FreeRTOS queue default.

### Deduplication policy

Key is `(node_id, boot_id, seq)`. `boot_id` is randomised once per sensor boot and costs one byte on the wire; it disambiguates the reboot case unconditionally. Without it, a node that restarts resets `seq` to 0 and every subsequent reading looks stale forever.

`seq` is `u16` and compared with serial-number arithmetic — `seq.wrapping_sub(last) < 0x8000` means newer — so wraparound is handled rather than avoided. This is a pure function and, after the protocol round-trip, the second highest-value test in the repo.

### Buffering and retry

Bounded ring buffer of readings, drop-oldest on overflow. `on_reading` enqueues then attempts a flush; `on_tick` attempts a flush when the backoff interval has elapsed. Backoff is exponential with a cap, all driven by `Millis` arguments.

### Wire protocol

`encode` / `decode` for `Reading`, little-endian, leading version byte: `version: u8`, `node_id: u8`, `boot_id: u8`, `seq: u16`, `raw: i16` — 7 bytes, well inside the ~250-byte ESP-NOW payload.

`decode` returns `Result<Reading, DecodeError>` with tests for truncated input, trailing bytes, and unknown version. The round-trip test is the highest-value shared test in the repo, since both firmwares depend on it agreeing.

ESP-NOW already provides a link-layer CRC and a send-status acknowledgement, so no application CRC is needed — but that ack is what makes `Uplink::send_reading` meaningfully fallible and worth retrying at the sensor.

### Pure decoding stays in core

DS18B20 scratchpad parsing (`[u8; 9] -> Result<Celsius, SensorError>`, including the CRC-8 check) is a pure function with host tests. Adapters move bytes; they do not interpret them.

## Shared radio link

The sensor sends and the gateway receives, but peer setup and channel configuration are shared. `centinela-esp` exposes a shared configuration/initialisation type plus **separate sender and receiver handles**, so each firmware constructs only what it needs.

Relying on LTO to strip the unused half is a hope, not a mechanism: if a single constructor registers a receive callback, the sensor links the entire receive path and its queue regardless of optimisation level, and dev builds carry both in any case.

Framing is pure and lives in core alongside the proto — `centinela-esp` shares only transport configuration. The transport choice (ESP-NOW vs BLE) does not reach core.

## Build system

`esp-idf-sys` reads its configuration **only from the root crate's `Cargo.toml`** — the package in the workspace directory. A virtual manifest has none, so each firmware build must name it:

```bash
ESP_IDF_SYS_ROOT_CRATE=centinela cargo build -p centinela --release
ESP_IDF_SYS_ROOT_CRATE=torre     cargo build -p torre     --release
```

`extra_components` follows a looser rule — collected from the root crate and all **direct** dependencies. Components needed by both firmwares are declared once in `centinela-esp`; role-specific ones stay in that role's own manifest, in particular `onewire_bus`, which belongs to `centinela` only. Note the fragility: the rule is *direct* dependencies only, so components declared in a transitive crate are silently dropped. If `centinela-esp` is ever split further, its component declarations must be re-hoisted.

### `sdkconfig` paths are workspace-relative

`ESP_IDF_SYS_ROOT_CRATE` selects whose `[package.metadata.esp-idf-sys]` applies, **but relative paths inside that table resolve against the workspace directory, not the crate directory.** A bare `sdkconfig.defaults` in `centinela/Cargo.toml` therefore points at the repo root, and both firmwares would silently share one file. Each firmware must spell out the path, and can layer role settings on a shared base:

```toml
[package.metadata.esp-idf-sys]
esp_idf_sdkconfig_defaults = ["sdkconfig.defaults", "centinela/sdkconfig.defaults"]
```

Later entries override earlier ones, so the root file keeps the common stack-size settings and the role file carries radio, WiFi, and MQTT configuration.

Watch the inconsistency: `extra_components` paths use the **opposite** rule and resolve against the directory of the `Cargo.toml` that declares them.

Configuration shared by both firmwares (`ESP_IDF_VERSION`, `ESP_IDF_TOOLS_INSTALL_DIR`) stays in `[env]` in `.cargo/config.toml`, where it applies to every build and takes precedence over metadata. Only role-specific keys go in package metadata, which avoids duplicating the whole table twice.

### Partition tables are not an `sdkconfig` matter

`esp-idf-sys` explicitly does not consume `CONFIG_PARTITION_TABLE_CUSTOM`; a custom table is a flashing argument (`espflash flash --partition-table partitions.csv`). Since the workspace-level `runner` string in `.cargo/config.toml` cannot differ per firmware, flashing goes through the per-firmware scripts too.

### Per-firmware target directories

Two firmwares with different ESP-IDF inputs — a differing `sdkconfig` or, as is already the case, a differing set of `extra_components` — sharing one `target/` means alternating builds rebuild ESP-IDF each time. Each script sets its own `CARGO_TARGET_DIR`. Because embuild locates the workspace directory *from* the target directory, moving it requires pinning `CARGO_WORKSPACE_DIR` to the repo root explicitly — otherwise relative `sdkconfig` paths and the tools install directory resolve somewhere unintended.

Wrap all of this in `scripts/centinela.sh` and `scripts/torre.sh`, thin wrappers over a shared `scripts/firmware.sh` that set `ESP_IDF_SYS_ROOT_CRATE`, `CARGO_TARGET_DIR`, and `CARGO_WORKSPACE_DIR` and forward the rest to `cargo`. These scripts are load-bearing, not convenience.

### Single `.cargo/config.toml`

Both boards are ESP32-C3, so one workspace-root config covers `target`, `MCU`, linker, and `rustflags`. If the gateway ever moves to a different chip, each firmware needs its own config and must be built from its own directory. Note that `[unstable] build-std` cannot be target-scoped, which is why host test runs must override it (see Tooling).

## Tooling

Host tests are `cargo test -p centinela-core --target <host triple>`, which pulls in neither `embuild` nor `esp-idf-sys`. `test.sh` keeps its `--config 'unstable.build-std=[]'` override and its host-triple detection, but drops `--lib`, which would skip integration tests under `core/tests/` and doc tests.

`rust-toolchain.toml` pins `channel = "esp"`. The C3 is RISC-V, so the `esp-rs/xtensa-toolchain` action is a misnomer here; it is used because it installs that channel and `ldproxy`.

### CI

The current workflow breaks on the first day of the split: `cargo build --release` and `cargo clippy --workspace` at a virtual manifest root try to build both firmwares in one invocation, with one ambiguous root crate and one `sdkconfig`.

- **Firmware matrix** over `{centinela, torre}` × `{build, clippy}`, each invoking the per-firmware script so `ESP_IDF_SYS_ROOT_CRATE` is always set. Clippy must be per-package for the same reason as build; drop `--all-features`, which produces meaningless `esp-idf-svc` feature combinations.
- **Format** once at the root — `cargo fmt --all --check` compiles nothing and needs no ESP toolchain.
- **Host tests** on stable, no ESP toolchain, running `test.sh`.
- **Fitness check** asserting the core dependency tree is free of ESP crates.

## Hardware facts

DS18B20 data line on GPIO0 with a 4.7 kΩ pull-up **to 3.3 V** (not 5 V — the C3 is not 5 V tolerant); VDD and pull-up both on 3.3 V.

The 1-Wire driver is `esp_idf_svc::hal::onewire` (`OWDriver`, `OWAddress`, `OWCommand`), which only exists when the `onewire_bus` remote component is declared:

```toml
[[package.metadata.esp-idf-sys.extra_components]]
remote_component = { name = "onewire_bus", version = "^1.0.4" }
```

Adding it requires `cargo clean` before the module appears. Opening the bus and discovering the single sensor is two calls, the doubled `?` being because `search()` yields `Result` items:

```rust
let mut bus = OWDriver::new(peripherals.pins.gpio0)?;
let address = bus.search()?.next().context("no device on 1-Wire bus")??;
```

There is no `Ds18b20` type in the HAL; convert (`0x44`) and read scratchpad (`0xBE`) are implemented by hand, following `esp-idf-hal`'s `rmt_onewire_temperature.rs` example. The `ds18b20` crate on crates.io targets `embedded-hal` 0.2 bit-banging and is incompatible with this RMT-backed bus.

On the gateway, WiFi and BLE share one radio. ESP-NOW alongside a WiFi station connection must run on the access point's channel, which means the sensor's channel is dictated by the gateway's AP — if the AP moves channel, the link breaks until the sensor follows. WiFi power save must be disabled on the gateway (`WIFI_PS_NONE`) or it will miss ESP-NOW frames while dozing.

## Known warts to verify on the first build

- The remote-component lock file is written to the workspace directory and its name depends on the ESP-IDF version, not the crate. Two firmwares with different remote components will rewrite it back and forth. Gitignore it; if the churn causes rebuilds, the fallback is a per-firmware `CARGO_WORKSPACE_DIR` plus `ESP_IDF_TOOLS_INSTALL_DIR = "global"`, so the two ESP-IDF toolchain installs are not duplicated per firmware.
- Whether `ESP_IDF_SYS_ROOT_CRATE` is tracked as a build-script fingerprint input. If it is not, switching firmwares without switching `CARGO_TARGET_DIR` can produce a stale `sdkconfig`.

## Implementation status

In place:

- All four crates exist: `core/`, `esp/`, `centinela/`, `torre/`. Profiles and `default-members` at the root, per-firmware build scripts over a shared `scripts/firmware.sh`, the host test script, the core dependency fitness check, and a CI matrix over firmware × command.
- Each firmware builds into `target/<package>/` with `ESP_IDF_SYS_ROOT_CRATE` and `CARGO_WORKSPACE_DIR` set by its script. The two firmwares already differ in `extra_components` — `onewire_bus` for `centinela` only — so their ESP-IDF builds diverge and a shared target directory would rebuild it on every switch.
- `centinela-esp` holds `init_esp_idf()`, called by both firmware binaries.
- The temperature interface in core: `Celsius`, `SensorError`, `CONVERSION_MS`, and the two-phase `TemperatureSensor` port, with host tests over the DS18B20 datasheet conversion table.

Deliberately absent, so that each item lands with the code that needs it:

- **No adapter wiring.** Both firmwares initialise ESP-IDF and logging and nothing else. Nothing implements `TemperatureSensor`, there is no `Devices` bundle, and `centinela-esp` has no `EspError` mapping or radio link yet. The dependency edges are declared, so the next slice only adds code.
- **No `Clock` port, no application services, no wire protocol, no scratchpad parsing.** `torre` is a skeleton binary: the gateway service, `Uplink`, and `Publisher` arrive with the radio link.
- **`esp_idf_sdkconfig_defaults` is still not declared.** The trigger for the explicit path-prefixed lists is the first setting that has to differ *between* firmwares, not the existence of the second firmware. Until then both correctly resolve the shared `sdkconfig.defaults` at the workspace root, and inventing role files with no settings in them would only obscure that. When the gateway needs its WiFi and MQTT configuration, both manifests get the full list at once — a role file added without the corresponding manifest entry is silently ignored, since the default is a single bare path.

Ports carry no doc comments, by project convention; their contracts live here. For the temperature port: `read_conversion` is only valid `CONVERSION_MS` after `start_conversion`, and an early call is a `SensorError::Timeout` from the adapter rather than a panic.

## Deferred

- Deep sleep and battery operation on the sensor (see Recorded decisions).
- OTA and therefore the partition layout beyond a single app slot.
- More than one sensor node: `node_id` is on the wire and dedup is keyed per node, so the protocol is ready, but provisioning and peer management are not designed.
