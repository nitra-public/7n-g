//! `n ch` — Rust-порт `ch.js` (22.0K). Тонка обгортка над `nitra-cursor change`
//! з авто-визначенням воркспейсу (`--path` звужує ціль до одного воркспейсу).

use crate::{NError, Result};

pub fn run() -> Result<()> {
    Err(NError::NotPorted("ch"))
}
