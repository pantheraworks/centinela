# Centinela — Architecture

## System

Two ESP32-C3 firmwares sharing one domain core:

- **centinela** — sensor node. DS18B20 temperature, button, screen. No WiFi.
- **torre** — gateway. Receives readings over the radio link, uplinks over WiFi. No sensors.

Hexagonal (ports and adapters). Dependency direction points inward: adapters depend on core, never the reverse.

## Recorded decisions

- **The sensor is mains/USB powered.** It is always on, so a long-lived `tick()` loop is valid and sequence counters live in RAM. If this ever changes to battery with deep sleep, the loop model breaks: `seq` and dedup state would have to survive reboots in RTC slow memory and the core would stop being a loop. Revisit this document before adding deep sleep.
- **Ports stay traits, and they block.** ESP-IDF gives us `std`, so `std::thread` maps to FreeRTOS tasks and a blocking wait yields the CPU rather than spinning. A thermometer read that takes 750 ms in its own task freezes nothing; concurrency comes from the RTOS, not from a cooperative loop in core. An earlier revision of this document specified a two-phase temperature port and a tick-driven scheduler in core to avoid blocking. That was solving a problem the RTOS already solves.
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
cargo tree -p centinela-core --edges normal --target all --prefix none | grep -qE '^(esp-|embuild|embassy)'
```

`--target all` is what makes it airtight: without it, an ESP dependency hidden behind a `cfg(target_os = "espidf")` gate would pass the check on a host.

## Core

### Domain types

`Celsius` is a newtype over the DS18B20's native representation: a signed 16-bit value in 1/16 °C steps, which covers −55..125 °C losslessly. **Not `f32`.** The C3 is `riscv32imc` with no FPU, so every float operation is soft-float; fixed point also gives exact equality, which deduplication needs, and a portable wire encoding. Rendering is `Display` on `Celsius` itself rather than a helper in an adapter: it is pure domain logic, it is host-testable, and as a `core::fmt` impl it needs no allocator, so it stays available to `no_std` callers and composes with `write!` and `log`.

`Instant` is a monotonic `u64` millisecond count since boot, only ever compared or differenced. Points in time are `Instant`; durations are plain `u32` milliseconds. Keeping the two in separate types is what stops a deadline being passed where a delay was meant.

`Reading { node_id, boot_id, seq, celsius }`.

### Errors

Every port that can fail returns `Result` with a domain error enum owned by core, named after its port — `ThermometerError { NotPresent, Crc, Timeout, Bus }`, `ScreenError`, `UplinkError`, `PublishError`. Naming them after the port rather than the role is deliberate: "sensor" already means the node, so a `SensorError` would read as an error of the whole device. Infallible port signatures force adapters to panic or fabricate values, which is precisely the behaviour that cannot be tested.

**`thiserror` in core, `anyhow` in the binaries.** They are not alternatives; they belong to different layers. Core's errors must be typed and matchable because the *policy* lives in core — "retry a `Crc`, treat `NotPresent` as fatal for this cycle" is only expressible over a closed set of variants, and `anyhow::Error` is type-erased, so core would have to downcast to make a decision it should be making directly. `thiserror` is a derive macro that generates exactly the `Display` and `Error` impls we would otherwise write by hand, so it does not appear in the API. It is declared with `default-features = false`, which targets `core::error::Error` and keeps core `no_std`; `anyhow` would additionally want `alloc` and a global allocator.

The firmware binaries and adapters use `anyhow`, where type erasure is the point: `main` and setup code only log and bail.

**No port ever returns `anyhow::Error`, and no core error carries an `EspError`.** The adapter boundary is where translation happens, and it is deliberately explicit rather than a `#[from]` impl: a single `EspError` can mean `Timeout` or `Bus` depending on context, so the adapter matches, logs the underlying code for the serial console, and returns the domain class. A blanket `From` would both lose that distinction and drag ESP types into core.

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

**`Thermometer::read` blocks and returns either a value or an error.** Everything about how the reading is obtained is internal to the adapter: issuing the convert command, waiting out the roughly 94 to 750 ms the configured resolution implies, reading the scratchpad, checking the CRC. Resolution never becomes a value core has to receive and reason about, and there is no ordering contract for a caller to get wrong.

**Whoever calls `read` decides what the result means.** Sampling cadence, whether a `Crc` failure is retried, what a stale reading does to the screen — none of that belongs to the port, and none of it is designed yet.

**`Clock` has only `now()`.** Sleeping belongs to whoever owns the thread. Core needs to know what time it is, for reading timestamps and retry backoff, not how to wait.

**`Screen::show` takes a domain view struct, not a string.** Formatting and layout are adapter concerns, and a `&str` argument would drag `alloc` into core.

**WiFi is not a port.** The port is `Publisher`. Association, DHCP, and session handling are implementation details inside the adapter. A WiFi trait would leak infrastructure into the domain.

### Injecting ports

There is no bundle type yet, because nothing in core takes more than one port. When a service does need several, two decisions are already made:

**A plain generic struct with public fields, not a trait with accessors.** An accessor trait fails on borrows: a `&mut self` getter means nothing else on the bundle can be touched while its result is alive, so touching two ports in consecutive lines needs temporaries, scopes, or a `split()`. Struct fields are borrow-checked independently.

**Ports passed as arguments, not held as fields.** Services own state and policy only, so they carry no type parameters and the generics live on the methods. Tests keep ownership of their fakes and can assert on them afterwards.

Trait objects would also work and are deliberately not used: everything here is monomorphised.

### Sensor service

Not designed yet, and deliberately not. The sensor thread calls `Thermometer::read` and gets a value or an error; what it does with either — cadence, retry, what the screen shows, when to uplink — is the next thing to work out, once a reading has actually come off the hardware.

The shape it will take is a thread in `centinela` calling into core for decisions, rather than logic spread across tasks behind shared state. Threads wait; one thread decides. The interesting failures are sequencing failures, and those are exhaustively testable inside a struct and merely observable across threads.

### Gateway service

Driven from outside, but by **two** entry points:

```rust
fn on_reading(&mut self, reading: Reading, now: Instant);
fn on_tick(&mut self, now: Instant);
```

`on_reading` alone is insufficient: buffering and retry must fire in the *absence* of input. If an uplink fails and the sensor then goes quiet, nothing would ever call `on_reading` again and buffered readings would sit forever. The gateway loop does a receive with timeout and dispatches to whichever applies. Both are directly callable in tests with fabricated times.

**The radio receive callback must not call `on_reading` directly.** The ESP-NOW receive callback runs in the WiFi task context; publishing MQTT from there is not permissible. The path is: callback → bounded queue → main loop drains → `decode` → `on_reading`. The queue's overflow policy (drop oldest) is a domain decision recorded in core rather than left to a FreeRTOS queue default.

### Deduplication policy

Key is `(node_id, boot_id, seq)`. `boot_id` is randomised once per sensor boot and costs one byte on the wire; it disambiguates the reboot case unconditionally. Without it, a node that restarts resets `seq` to 0 and every subsequent reading looks stale forever.

`seq` is `u16` and compared with serial-number arithmetic — `seq.wrapping_sub(last) < 0x8000` means newer — so wraparound is handled rather than avoided. This is a pure function and, after the protocol round-trip, the second highest-value test in the repo.

### Buffering and retry

Bounded ring buffer of readings, drop-oldest on overflow. `on_reading` enqueues then attempts a flush; `on_tick` attempts a flush when the backoff interval has elapsed. Backoff is exponential with a cap, all driven by `Instant` arguments.

### Wire protocol

`encode` / `decode` for `Reading`, little-endian, leading version byte: `version: u8`, `node_id: u8`, `boot_id: u8`, `seq: u16`, `raw: i16` — 7 bytes, well inside the ~250-byte ESP-NOW payload.

`decode` returns `Result<Reading, DecodeError>` with tests for truncated input, trailing bytes, and unknown version. The round-trip test is the highest-value shared test in the repo, since both firmwares depend on it agreeing.

ESP-NOW already provides a link-layer CRC and a send-status acknowledgement, so no application CRC is needed — but that ack is what makes `Uplink::send_reading` meaningfully fallible and worth retrying at the sensor.

### Pure decoding stays in core

DS18B20 scratchpad parsing lives in `core::ds18b20` — `parse_scratchpad(&[u8; 9]) -> Result<Celsius, ThermometerError>`, including the Dallas CRC-8 over all nine bytes, which is zero for a valid scratchpad. Adapters move bytes; they do not interpret them. The command bytes and the family code live there too, so the adapter names nothing magic.

Device-specific decoding sits in its own module rather than beside the port, which stays generic: `Thermometer` says nothing about DS18B20, and a second part would add a module without touching it.

The CRC check is also what detects a disconnected bus, with no special case: a floating line reads all `0xFF`, whose checksum does not come to zero. The 85 °C power-on value is deliberately *not* treated as an error — it is a legitimate temperature.

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

DS18B20 data line on GPIO4 with a 4.7 kΩ pull-up **to 3.3 V** (not 5 V — the C3 is not 5 V tolerant); VDD and pull-up both on 3.3 V. GPIO0 was the first choice but doubles as `XTAL_32K_P`, so a populated 32 kHz crystal would contend with the bus; the other pins to avoid are 2, 8 and 9 (strapping), 11 through 17 (flash and `VDD_SPI`), and 20 and 21 (console UART).

**The external pull-up is not optional.** `OWDriver::new` builds its bus config with `flags: Default::default()`, leaving `en_pull_up` clear, so the component calls `gpio_set_pull_mode(pin, GPIO_FLOATING)` on a pin it has already switched to open drain. Nothing on the chip can drive the line high; the resistor is the only thing that does.

The two electrical failures are distinguishable by error code, which makes bring-up much faster than guessing. A floating line never returns high, so the RMT receiver never observes the idle period that ends a reception, never completes, and `onewire_bus_rmt_reset` fails its one-second queue wait with `ESP_ERR_TIMEOUT`. A properly pulled-up line with no device answering *does* complete the reception and fails only the presence-pulse check, giving `ESP_ERR_NOT_FOUND`. So `ESP_ERR_TIMEOUT` on reset means wiring — missing pull-up, DQ shorted low, or VDD and GND reversed — while `ESP_ERR_NOT_FOUND` means the bus is electrically alive but the sensor is not responding.

The 1-Wire driver is `esp_idf_svc::hal::onewire` (`OWDriver`, `OWAddress`, `OWCommand`), which only exists when the `onewire_bus` remote component is declared:

```toml
[[package.metadata.esp-idf-sys.extra_components]]
remote_component = { name = "onewire_bus", version = "^1.0.4" }
```

Adding it requires `cargo clean` before the module appears. Opening the bus and discovering the single sensor is two calls, the doubled `?` being because `search()` yields `Result` items:

```rust
let mut bus = OWDriver::new(peripherals.pins.gpio4)?;
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
- Core holds `Celsius` with its `Display` impl, `ThermometerError`, the `Thermometer` port, and `ds18b20::parse_scratchpad`. Host tests cover the datasheet conversion table, the power-on scratchpad, negative temperatures, a corrupted byte, a wrong checksum, a floating bus, and the rendered form either side of zero.
- `centinela::ds18b20::Ds18b20` implements `Thermometer` over `esp_idf_svc::hal::onewire`: it opens the bus at construction, then on the first read takes the first device the search yields and rejects a non-`0x28` family code as `NotPresent`, and per read issues `MatchRom` plus convert, waits out the conversion, re-addresses, reads nine bytes, and hands them to core. `EspError` is logged and mapped to a domain class at that boundary.
- `centinela`'s `main` reads once a second and logs the value or the error class, and that loop *is* the retry policy: nothing after `Peripherals::take` is fatal. The split is that `Ds18b20::new` only opens the RMT bus, which fails solely on a bad pin or an exhausted RMT channel and so is genuinely fatal, while device discovery is lazy and cached inside the adapter. A read with no cached address enumerates first; any failure other than `Crc` clears the cache, since a CRC failure proves the transport is working whereas the rest cast doubt on the address itself. The practical effect is that the node survives a sensor that is missing at boot and starts reporting when the wiring is fixed, without a reflash.

Deliberately absent, so that each item lands with the code that needs it:

- **No `Clock`, no `Instant`, no services, no bundle.** They are described above as the shape to reach for, not as code that exists. A `Sampler` state machine did exist and was deleted along with its timing tests when the port stopped being two-phase; the cadence it scheduled is now a `delay_ms` in `main`.
- **No screen, button, or uplink, and no policy.** Nothing retries, deduplicates, or holds a last-good reading. `main` is a loop that reads and logs, which is the smallest thing that gets a number off the hardware.
- **No wire protocol.** `torre` is a skeleton binary: the gateway service, `Uplink`, and `Publisher` arrive with the radio link.

Two details about the adapter worth knowing before changing it. The conversion wait is a `FreeRtos::delay_ms(750)` inside `read`, hardcoded for 12-bit resolution — the device is never reconfigured, so it is the correct constant, but it belongs to whoever writes the resolution register if that ever happens. And the temperature formatting in `main` is sign-aware integer arithmetic rather than a float, because `-0.5 °C` would otherwise print as `0.500`; that logic moves into the screen adapter when there is one.
- **`esp_idf_sdkconfig_defaults` is still not declared.** The trigger for the explicit path-prefixed lists is the first setting that has to differ *between* firmwares, not the existence of the second firmware. Until then both correctly resolve the shared `sdkconfig.defaults` at the workspace root, and inventing role files with no settings in them would only obscure that. When the gateway needs its WiFi and MQTT configuration, both manifests get the full list at once — a role file added without the corresponding manifest entry is silently ignored, since the default is a single bare path.

Ports carry no doc comments, by project convention; their contracts live here. For the temperature port there is only one: `read` blocks for as long as the conversion takes and returns a value or an error. The adapter is where an `EspError` gets logged before being mapped, since the domain error keeps only the class.

## Deferred

- Deep sleep and battery operation on the sensor (see Recorded decisions).
- OTA and therefore the partition layout beyond a single app slot.
- More than one sensor node: `node_id` is on the wire and dedup is keyed per node, so the protocol is ready, but provisioning and peer management are not designed.
