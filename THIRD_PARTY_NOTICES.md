# Third-Party Notices

DFRelay is released under the MIT License; see `LICENSE`. This file covers third-party software used by DFRelay.

## sing-box

DFRelay manages an external [sing-box](https://github.com/SagerNet/sing-box) process; it does not embed, link, or redistribute sing-box.

- License: GPL-3.0-or-later
- Copyright: SagerNet and the sing-box contributors
- Source: https://github.com/SagerNet/sing-box
- Releases: https://github.com/SagerNet/sing-box/releases

DFRelay downloads the official sing-box binary at runtime. Anyone redistributing sing-box must comply with the GPL, including providing the corresponding source. DFRelay release archives do not contain sing-box.

## Rust dependencies

DFRelay is built with Rust crates. The table below is generated from `Cargo.lock` and lists every resolved crate with the SPDX license expression reported by its package metadata. The resolved graph includes platform-specific, optional, and build-only packages that may not be present in a particular binary.

Where a crate offers a choice of licenses, DFRelay relies on the permissive option. License texts and copyright notices remain available in the upstream crate packages and repositories; the common license texts are linked below.

| Crate | Version | License |
|---|---|---|
| `adler2` | `2.0.1` | 0BSD OR MIT OR Apache-2.0 |
| `aes` | `0.8.4` | MIT OR Apache-2.0 |
| `allocator-api2` | `0.2.21` | MIT OR Apache-2.0 |
| `android_system_properties` | `0.1.6` | MIT OR Apache-2.0 |
| `anstream` | `1.0.0` | MIT OR Apache-2.0 |
| `anstyle` | `1.0.14` | MIT OR Apache-2.0 |
| `anstyle-parse` | `1.0.0` | MIT OR Apache-2.0 |
| `anstyle-query` | `1.1.5` | MIT OR Apache-2.0 |
| `anstyle-wincon` | `3.0.11` | MIT OR Apache-2.0 |
| `anyhow` | `1.0.104` | MIT OR Apache-2.0 |
| `arbitrary` | `1.5.0` | MIT OR Apache-2.0 |
| `atomic-waker` | `1.1.2` | Apache-2.0 OR MIT |
| `autocfg` | `1.5.1` | Apache-2.0 OR MIT |
| `base64` | `0.22.1` | MIT OR Apache-2.0 |
| `base64` | `0.23.1` | MIT OR Apache-2.0 |
| `bitflags` | `2.13.2` | MIT OR Apache-2.0 |
| `block-buffer` | `0.10.4` | MIT OR Apache-2.0 |
| `bumpalo` | `3.20.3` | MIT OR Apache-2.0 |
| `byteorder` | `1.5.0` | Unlicense OR MIT |
| `bytes` | `1.12.1` | MIT |
| `bzip2` | `0.5.2` | MIT OR Apache-2.0 |
| `bzip2-sys` | `0.1.13+1.0.8` | MIT/Apache-2.0 |
| `cassowary` | `0.3.0` | MIT / Apache-2.0 |
| `castaway` | `0.2.4` | MIT |
| `cc` | `1.10.0` | MIT OR Apache-2.0 |
| `cfg-if` | `1.0.5` | MIT OR Apache-2.0 |
| `cfg_aliases` | `0.2.2` | MIT |
| `chacha20` | `0.10.2` | MIT OR Apache-2.0 |
| `chrono` | `0.4.45` | MIT OR Apache-2.0 |
| `cipher` | `0.4.4` | MIT OR Apache-2.0 |
| `clap` | `4.6.7` | MIT OR Apache-2.0 |
| `clap_builder` | `4.6.7` | MIT OR Apache-2.0 |
| `clap_derive` | `4.6.7` | MIT OR Apache-2.0 |
| `clap_lex` | `1.1.1` | MIT OR Apache-2.0 |
| `colorchoice` | `1.0.5` | MIT OR Apache-2.0 |
| `compact_str` | `0.8.2` | MIT |
| `constant_time_eq` | `0.3.1` | CC0-1.0 OR MIT-0 OR Apache-2.0 |
| `core-foundation-sys` | `0.8.7` | MIT OR Apache-2.0 |
| `cpufeatures` | `0.2.17` | MIT OR Apache-2.0 |
| `cpufeatures` | `0.3.1` | MIT OR Apache-2.0 |
| `crc` | `3.4.0` | MIT OR Apache-2.0 |
| `crc-catalog` | `2.5.0` | MIT OR Apache-2.0 |
| `crc32fast` | `1.5.3` | MIT OR Apache-2.0 |
| `crossbeam-utils` | `0.8.23` | MIT OR Apache-2.0 |
| `crossterm` | `0.28.1` | MIT |
| `crossterm_winapi` | `0.9.1` | MIT |
| `crypto-common` | `0.1.7` | MIT OR Apache-2.0 |
| `darling` | `0.24.1` | MIT |
| `darling_core` | `0.24.1` | MIT |
| `darling_macro` | `0.24.1` | MIT |
| `deflate64` | `0.1.12` | MIT |
| `deranged` | `0.5.9` | MIT OR Apache-2.0 |
| `derive_arbitrary` | `1.5.0` | MIT OR Apache-2.0 |
| `digest` | `0.10.7` | MIT OR Apache-2.0 |
| `directories` | `6.0.0` | MIT OR Apache-2.0 |
| `dirs-sys` | `0.5.0` | MIT OR Apache-2.0 |
| `displaydoc` | `0.2.7` | MIT OR Apache-2.0 |
| `either` | `1.19.0` | MIT OR Apache-2.0 |
| `equivalent` | `1.0.3` | Apache-2.0 OR MIT |
| `errno` | `0.3.14` | MIT OR Apache-2.0 |
| `fastrand` | `2.5.0` | Apache-2.0 OR MIT |
| `filetime` | `0.2.29` | MIT/Apache-2.0 |
| `find-msvc-tools` | `0.1.15` | MIT OR Apache-2.0 |
| `flate2` | `1.1.10` | MIT OR Apache-2.0 |
| `foldhash` | `0.1.5` | Zlib |
| `form_urlencoded` | `1.2.2` | MIT OR Apache-2.0 |
| `futures-channel` | `0.3.34` | MIT OR Apache-2.0 |
| `futures-core` | `0.3.34` | MIT OR Apache-2.0 |
| `futures-task` | `0.3.34` | MIT OR Apache-2.0 |
| `futures-util` | `0.3.34` | MIT OR Apache-2.0 |
| `generic-array` | `0.14.7` | MIT |
| `getrandom` | `0.2.17` | MIT OR Apache-2.0 |
| `getrandom` | `0.3.4` | MIT OR Apache-2.0 |
| `getrandom` | `0.4.3` | MIT OR Apache-2.0 |
| `hashbrown` | `0.15.5` | MIT OR Apache-2.0 |
| `hashbrown` | `0.17.1` | MIT OR Apache-2.0 |
| `heck` | `0.5.0` | MIT OR Apache-2.0 |
| `hmac` | `0.12.1` | MIT OR Apache-2.0 |
| `http` | `1.5.0` | MIT OR Apache-2.0 |
| `http-body` | `1.1.0` | MIT |
| `http-body-util` | `0.1.5` | MIT |
| `httparse` | `1.10.1` | MIT OR Apache-2.0 |
| `hyper` | `1.12.0` | MIT |
| `hyper-rustls` | `0.27.10` | Apache-2.0 OR ISC OR MIT |
| `hyper-util` | `0.1.21` | MIT |
| `iana-time-zone` | `0.1.65` | MIT OR Apache-2.0 |
| `iana-time-zone-haiku` | `0.1.2` | MIT OR Apache-2.0 |
| `icu_collections` | `2.4.0` | Unicode-3.0 |
| `icu_locale_core` | `2.4.0` | Unicode-3.0 |
| `icu_normalizer` | `2.4.0` | Unicode-3.0 |
| `icu_normalizer_data` | `2.4.0` | Unicode-3.0 |
| `icu_properties` | `2.4.0` | Unicode-3.0 |
| `icu_properties_data` | `2.4.0` | Unicode-3.0 |
| `icu_provider` | `2.4.0` | Unicode-3.0 |
| `ident_case` | `1.0.1` | MIT/Apache-2.0 |
| `idna` | `1.1.0` | MIT OR Apache-2.0 |
| `idna_adapter` | `1.2.2` | Apache-2.0 OR MIT |
| `if-addrs` | `0.15.0` | MIT OR BSD-3-Clause |
| `indexmap` | `2.14.2` | Apache-2.0 OR MIT |
| `indoc` | `2.0.8` | MIT OR Apache-2.0 |
| `inout` | `0.1.4` | MIT OR Apache-2.0 |
| `instability` | `0.3.14` | MIT |
| `ipnet` | `2.12.2` | MIT OR Apache-2.0 |
| `is_terminal_polyfill` | `1.70.2` | MIT OR Apache-2.0 |
| `itertools` | `0.13.0` | MIT OR Apache-2.0 |
| `itoa` | `1.0.18` | MIT OR Apache-2.0 |
| `jobserver` | `0.1.35` | MIT OR Apache-2.0 |
| `js-sys` | `0.3.106` | MIT OR Apache-2.0 |
| `libc` | `0.2.191` | MIT OR Apache-2.0 |
| `libredox` | `0.1.25` | MIT |
| `linux-raw-sys` | `0.12.1` | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `linux-raw-sys` | `0.4.15` | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `litemap` | `0.8.3` | Unicode-3.0 |
| `lock_api` | `0.4.14` | MIT OR Apache-2.0 |
| `log` | `0.4.34` | MIT OR Apache-2.0 |
| `lru` | `0.12.5` | MIT |
| `lru-slab` | `0.1.3` | MIT OR Apache-2.0 OR Zlib |
| `lzma-rs` | `0.3.0` | MIT |
| `lzma-sys` | `0.1.20` | MIT/Apache-2.0 |
| `memchr` | `2.8.3` | Unlicense OR MIT |
| `miniz_oxide` | `0.9.1` | MIT OR Zlib OR Apache-2.0 |
| `mio` | `1.2.4` | MIT |
| `num-conv` | `0.2.2` | MIT OR Apache-2.0 |
| `num-traits` | `0.2.19` | MIT OR Apache-2.0 |
| `once_cell` | `1.21.4` | MIT OR Apache-2.0 |
| `once_cell_polyfill` | `1.70.2` | MIT OR Apache-2.0 |
| `option-ext` | `0.2.0` | MPL-2.0 |
| `parking_lot` | `0.12.5` | MIT OR Apache-2.0 |
| `parking_lot_core` | `0.9.12` | MIT OR Apache-2.0 |
| `paste` | `1.0.15` | MIT OR Apache-2.0 |
| `pbkdf2` | `0.12.2` | MIT OR Apache-2.0 |
| `percent-encoding` | `2.3.2` | MIT OR Apache-2.0 |
| `pin-project-lite` | `0.2.17` | Apache-2.0 OR MIT |
| `pkg-config` | `0.3.34` | MIT OR Apache-2.0 |
| `potential_utf` | `0.1.6` | Unicode-3.0 |
| `powerfmt` | `0.2.1` | MIT OR Apache-2.0 |
| `ppv-lite86` | `0.2.21` | MIT OR Apache-2.0 |
| `proc-macro2` | `1.0.107` | MIT OR Apache-2.0 |
| `quinn` | `0.11.12` | MIT OR Apache-2.0 |
| `quinn-proto` | `0.11.19` | MIT OR Apache-2.0 |
| `quinn-udp` | `0.5.16` | MIT OR Apache-2.0 |
| `quote` | `1.0.47` | MIT OR Apache-2.0 |
| `r-efi` | `5.3.0` | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| `r-efi` | `6.0.0` | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| `rand` | `0.10.3` | MIT OR Apache-2.0 |
| `rand` | `0.8.8` | MIT OR Apache-2.0 |
| `rand_chacha` | `0.3.1` | MIT OR Apache-2.0 |
| `rand_core` | `0.10.1` | MIT OR Apache-2.0 |
| `rand_core` | `0.6.4` | MIT OR Apache-2.0 |
| `rand_pcg` | `0.10.2` | MIT OR Apache-2.0 |
| `ratatui` | `0.29.0` | MIT |
| `redox_syscall` | `0.5.18` | MIT |
| `redox_users` | `0.5.3` | MIT |
| `reqwest` | `0.12.28` | MIT OR Apache-2.0 |
| `ring` | `0.17.14` | Apache-2.0 AND ISC |
| `rustc-hash` | `2.1.3` | Apache-2.0 OR MIT |
| `rustix` | `0.38.44` | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `rustix` | `1.1.6` | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `rustls` | `0.23.46` | Apache-2.0 OR ISC OR MIT |
| `rustls-pki-types` | `1.15.1` | MIT OR Apache-2.0 |
| `rustls-webpki` | `0.103.16` | ISC |
| `rustversion` | `1.0.23` | MIT OR Apache-2.0 |
| `ryu` | `1.0.23` | Apache-2.0 OR BSL-1.0 |
| `scopeguard` | `1.2.0` | MIT OR Apache-2.0 |
| `serde` | `1.0.229` | MIT OR Apache-2.0 |
| `serde_core` | `1.0.229` | MIT OR Apache-2.0 |
| `serde_derive` | `1.0.229` | MIT OR Apache-2.0 |
| `serde_json` | `1.0.154` | MIT OR Apache-2.0 |
| `serde_spanned` | `0.6.9` | MIT OR Apache-2.0 |
| `serde_urlencoded` | `0.7.1` | MIT/Apache-2.0 |
| `serde_yaml_ng` | `0.10.0` | MIT |
| `sha1` | `0.10.7` | MIT OR Apache-2.0 |
| `sha2` | `0.10.9` | MIT OR Apache-2.0 |
| `shlex` | `2.0.1` | MIT OR Apache-2.0 |
| `signal-hook` | `0.3.18` | Apache-2.0/MIT |
| `signal-hook-mio` | `0.2.5` | MIT OR Apache-2.0 |
| `signal-hook-registry` | `1.4.8` | MIT OR Apache-2.0 |
| `simd-adler32` | `0.3.10` | MIT |
| `slab` | `0.4.12` | MIT |
| `smallvec` | `1.16.4` | MIT OR Apache-2.0 |
| `socket2` | `0.6.5` | MIT OR Apache-2.0 |
| `stable_deref_trait` | `1.2.1` | MIT OR Apache-2.0 |
| `static_assertions` | `1.1.0` | MIT OR Apache-2.0 |
| `strsim` | `0.11.1` | MIT |
| `strum` | `0.26.3` | MIT |
| `strum_macros` | `0.26.4` | MIT |
| `subtle` | `2.6.1` | BSD-3-Clause |
| `syn` | `2.0.119` | MIT OR Apache-2.0 |
| `syn` | `3.0.9` | MIT OR Apache-2.0 |
| `sync_wrapper` | `1.0.2` | Apache-2.0 |
| `synstructure` | `0.14.0` | MIT |
| `tar` | `0.4.46` | MIT OR Apache-2.0 |
| `tempfile` | `3.27.0` | MIT OR Apache-2.0 |
| `thiserror` | `2.0.21` | MIT OR Apache-2.0 |
| `thiserror-impl` | `2.0.21` | MIT OR Apache-2.0 |
| `time` | `0.3.55` | MIT OR Apache-2.0 |
| `time-core` | `0.1.9` | MIT OR Apache-2.0 |
| `tinystr` | `0.8.5` | Unicode-3.0 |
| `tinyvec` | `1.13.3` | Zlib OR Apache-2.0 OR MIT |
| `tokio` | `1.54.1` | MIT |
| `tokio-macros` | `2.7.2` | MIT |
| `tokio-rustls` | `0.26.6` | MIT OR Apache-2.0 |
| `toml` | `0.8.23` | MIT OR Apache-2.0 |
| `toml_datetime` | `0.6.11` | MIT OR Apache-2.0 |
| `toml_edit` | `0.22.27` | MIT OR Apache-2.0 |
| `toml_write` | `0.1.2` | MIT OR Apache-2.0 |
| `tower` | `0.5.3` | MIT |
| `tower-http` | `0.6.11` | MIT |
| `tower-layer` | `0.3.3` | MIT |
| `tower-service` | `0.3.3` | MIT |
| `tracing` | `0.1.44` | MIT |
| `tracing-core` | `0.1.36` | MIT |
| `try-lock` | `0.2.5` | MIT |
| `typenum` | `1.20.1` | MIT OR Apache-2.0 |
| `unicode-ident` | `1.0.27` | (MIT OR Apache-2.0) AND Unicode-3.0 |
| `unicode-segmentation` | `1.13.3` | MIT OR Apache-2.0 |
| `unicode-truncate` | `1.1.0` | MIT OR Apache-2.0 |
| `unicode-width` | `0.1.14` | MIT OR Apache-2.0 |
| `unicode-width` | `0.2.0` | MIT OR Apache-2.0 |
| `unsafe-libyaml` | `0.2.11` | MIT |
| `untrusted` | `0.9.0` | ISC |
| `url` | `2.5.8` | MIT OR Apache-2.0 |
| `utf8_iter` | `1.0.4` | Apache-2.0 OR MIT |
| `utf8parse` | `0.2.2` | Apache-2.0 OR MIT |
| `version_check` | `0.9.5` | MIT/Apache-2.0 |
| `want` | `0.3.2` | MIT |
| `wasi` | `0.11.1+wasi-snapshot-preview1` | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `wasip2` | `1.0.4+wasi-0.2.12` | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `wasm-bindgen` | `0.2.129` | MIT OR Apache-2.0 |
| `wasm-bindgen-futures` | `0.4.79` | MIT OR Apache-2.0 |
| `wasm-bindgen-macro` | `0.2.129` | MIT OR Apache-2.0 |
| `wasm-bindgen-macro-support` | `0.2.129` | MIT OR Apache-2.0 |
| `wasm-bindgen-shared` | `0.2.129` | MIT OR Apache-2.0 |
| `web-sys` | `0.3.106` | MIT OR Apache-2.0 |
| `web-time` | `1.1.0` | MIT OR Apache-2.0 |
| `webpki-roots` | `1.0.11` | CDLA-Permissive-2.0 |
| `winapi` | `0.3.9` | MIT/Apache-2.0 |
| `winapi-i686-pc-windows-gnu` | `0.4.0` | MIT/Apache-2.0 |
| `winapi-x86_64-pc-windows-gnu` | `0.4.0` | MIT/Apache-2.0 |
| `windows-core` | `0.62.2` | MIT OR Apache-2.0 |
| `windows-implement` | `0.60.2` | MIT OR Apache-2.0 |
| `windows-interface` | `0.59.3` | MIT OR Apache-2.0 |
| `windows-link` | `0.2.1` | MIT OR Apache-2.0 |
| `windows-result` | `0.4.1` | MIT OR Apache-2.0 |
| `windows-strings` | `0.5.1` | MIT OR Apache-2.0 |
| `windows-sys` | `0.52.0` | MIT OR Apache-2.0 |
| `windows-sys` | `0.59.0` | MIT OR Apache-2.0 |
| `windows-sys` | `0.61.2` | MIT OR Apache-2.0 |
| `windows-targets` | `0.52.6` | MIT OR Apache-2.0 |
| `windows_aarch64_gnullvm` | `0.52.6` | MIT OR Apache-2.0 |
| `windows_aarch64_msvc` | `0.52.6` | MIT OR Apache-2.0 |
| `windows_i686_gnu` | `0.52.6` | MIT OR Apache-2.0 |
| `windows_i686_gnullvm` | `0.52.6` | MIT OR Apache-2.0 |
| `windows_i686_msvc` | `0.52.6` | MIT OR Apache-2.0 |
| `windows_x86_64_gnu` | `0.52.6` | MIT OR Apache-2.0 |
| `windows_x86_64_gnullvm` | `0.52.6` | MIT OR Apache-2.0 |
| `windows_x86_64_msvc` | `0.52.6` | MIT OR Apache-2.0 |
| `winnow` | `0.7.15` | MIT |
| `wit-bindgen` | `0.57.1` | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `writeable` | `0.6.4` | Unicode-3.0 |
| `xattr` | `1.7.0` | MIT OR Apache-2.0 |
| `xz2` | `0.1.7` | MIT/Apache-2.0 |
| `yoke` | `0.8.4` | Unicode-3.0 |
| `yoke-derive` | `0.8.4` | Unicode-3.0 |
| `zerocopy` | `0.8.63` | BSD-2-Clause OR Apache-2.0 OR MIT |
| `zerocopy-derive` | `0.8.63` | BSD-2-Clause OR Apache-2.0 OR MIT |
| `zerofrom` | `0.1.8` | Unicode-3.0 |
| `zerofrom-derive` | `0.1.8` | Unicode-3.0 |
| `zeroize` | `1.9.1` | Apache-2.0 OR MIT |
| `zeroize_derive` | `1.5.0` | Apache-2.0 OR MIT |
| `zerotrie` | `0.2.5` | Unicode-3.0 |
| `zerovec` | `0.11.9` | Unicode-3.0 |
| `zerovec-derive` | `0.11.7` | Unicode-3.0 |
| `zip` | `2.6.1` | MIT |
| `zlib-rs` | `0.6.8` | Zlib |
| `zmij` | `1.0.23` | MIT |
| `zopfli` | `0.8.4` | Apache-2.0 |
| `zstd` | `0.13.3` | MIT |
| `zstd-safe` | `7.3.0` | BSD-3-Clause |
| `zstd-sys` | `2.1.2+zstd.1.5.7` | BSD-3-Clause |

## Common license texts

- MIT: https://opensource.org/license/mit
- Apache-2.0: https://www.apache.org/licenses/LICENSE-2.0
- Apache-2.0 WITH LLVM-exception: https://llvm.org/LICENSE.txt
- BSD-2-Clause: https://opensource.org/license/bsd-2-clause
- BSD-3-Clause: https://opensource.org/license/bsd-3-clause
- ISC: https://opensource.org/license/isc
- 0BSD: https://opensource.org/license/0bsd
- BSL-1.0: https://www.boost.org/LICENSE_1_0.txt
- CC0-1.0: https://creativecommons.org/publicdomain/zero/1.0/
- CDLA-Permissive-2.0: https://cdla.dev/permissive-2-0/
- MPL-2.0: https://www.mozilla.org/MPL/2.0/
- Unicode-3.0: https://www.unicode.org/license.txt
- Unlicense: https://unlicense.org/
- Zlib: https://opensource.org/license/zlib
- SPDX license list: https://spdx.org/licenses/

## Notes

- Copyright notices for individual crates remain with their respective authors.
- If you redistribute a DFRelay binary, include this file and `LICENSE` with the binary.
- Regenerate this list from `Cargo.lock` whenever dependencies change.
