# Building from source

## Requirements

- Rust toolchain ([rustup.rs](https://rustup.rs/))
- Linux with KMS/DRM support, or a Linux framebuffer device (`/dev/fb*`)

## Build

```bash
cargo build --release
```

The binary is placed at `./target/release/kmsvnc`.
