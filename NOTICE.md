# Third-party notices

Argus bundles the following third-party components.

## uPlot

- https://github.com/leeoniya/uPlot
- MIT License, Copyright (c) 2022 Leon Sorokin
- Bundled in `crates/argus-ui/assets/vendor/uplot`, the full license text is in `LICENSE` next to it.

## Inter

- https://github.com/rsms/inter
- SIL Open Font License 1.1, Copyright 2016 The Inter Project Authors
- Bundled in `crates/argus-ui/assets/fonts`, the full license text is in `LICENSE-Inter.txt` next to it.

## Lucide

- https://lucide.dev
- ISC License, Copyright (c) for portions of Lucide are held by Cole Bemis 2013-2022 as part of Feather (MIT). All
  other copyright (c) for Lucide are held by Lucide Contributors 2022.
- The settings icon in `crates/argus-ui/src/icons.rs` is the Lucide `settings` icon, scaled to 16 px.

## SQLite

- https://sqlite.org
- Public domain, compiled into the node through rusqlite.

## Rust crates

All Rust dependencies are listed in `Cargo.lock`. Their licenses are MIT, Apache-2.0, BSD-3-Clause, ISC, Zlib,
BSL-1.0, CC0-1.0, Unicode-3.0, Unlicense alternatives and CDLA-Permissive-2.0 (webpki-roots). ring is licensed under
Apache-2.0 and ISC. `cargo tree -e normal --format "{p} {l}"` lists them per crate.
