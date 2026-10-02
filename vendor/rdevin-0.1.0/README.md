# rdevin

[![Crate](https://img.shields.io/crates/v/rdevin.svg)](https://crates.io/crates/rdevin)
[![API](https://docs.rs/rdevin/badge.svg)](https://docs.rs/rdevin)

Cross-platform simulation and global listening for keyboard and mouse input.

> [!WARNING]
> This crate is subject to extreme change. There is still great room for improvement. It is only presently published for use by [NuhxBoard](https://github.com/justdeeevin/nuhxboard).

## Listening for input

The `listen` and `grab` [^1] functions can be used to run a callback for all input events.

```rust
rdevin::listen(|e| dbg!(e))?;
```

## Simulating input

The `simulate` function can be used to send input events.

```rust
use rdevin::{simulate, EventType, Key};

simulate(&EventType::KeyPress(Key::KeyS))?;
```

## Serialization

Serde support is gated behind the `serde` feature.

## Acknowledgements

- This crate is a fork of a fork of a fork of [Narsil's `rdev`
  crate](https://crates.io/crates/rdev), created to ensure continued maintenance and to make
  Rustdesk's many useful additions available on crates.io.
- [Enigo](https://github.com/Enigo-rs/Enigo), an input simulation library, served as inspiration and reference for Narsil's original crate.

[^1]: Not available on Linux
