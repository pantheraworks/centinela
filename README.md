# Centinela

IoT temperature monitoring with distributed sensor nodes. ESP32-C3 firmware in Rust (ESP-IDF).

Hardware:
- ESP32-C3-SUPERMINI

Design and rationale: [docs/architecture.md](docs/architecture.md).

## Layout

```
core/       centinela-core: domain types and ports. No ESP dependencies. All unit tests live here.
esp/        centinela-esp:  ESP adapters shared by both firmwares.
centinela/  sensor node firmware.
torre/      gateway firmware.
```

## Setup

```bash
./deps.sh
```

Open a new terminal (or `source ~/export-esp.sh`) so the ESP toolchain is on `PATH`.

## Build and flash

The workspace root is a virtual manifest, so `esp-idf-sys` has to be told which package owns the ESP-IDF configuration. Each firmware also builds into its own target directory, so that switching between them does not rebuild ESP-IDF. The per-firmware scripts handle both and forward everything else to `cargo`:

```bash
./scripts/centinela.sh build --release
./scripts/torre.sh build --release
```

Connect the board, then:

```bash
./scripts/centinela.sh run
```

## Test

Host unit tests (no board required):

```bash
./test.sh
```
