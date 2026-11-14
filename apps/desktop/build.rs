//! Compiles the message catalogs in `i18n/` into the binary (see `src/i18n.rs`).

use std::path::Path;

fn main() -> std::io::Result<()> {
    let out = std::env::var_os("OUT_DIR").ok_or_else(|| std::io::Error::other("OUT_DIR is not set"))?;
    rune_i18n::codegen::generate(Path::new("i18n"), Path::new(&out))
}
