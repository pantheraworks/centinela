# Centinela

IoT temperature monitoring with distributed sensor nodes. ESP32-C3 firmware in Rust (ESP-IDF).

Hardware:
- ESP32-C3-SUPERMINI

## Setup

```bash
./deps.sh
```

Open a new terminal (or `source ~/export-esp.sh`) so the ESP toolchain is on `PATH`.

## Flash

Connect the board, then:

```bash
cargo run
```

## Test

Host unit tests (no board required):

```bash
./test.sh
```
