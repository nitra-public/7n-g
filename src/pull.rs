//! `n pull [branch]` — Rust-порт `pull.js` (8.6K). `git fetch origin <branch>` →
//! [`crate::merge::delta_merge`] з `src = origin/<branch>`.

use crate::{NError, Result};

pub fn run(_branch: Option<&str>) -> Result<()> {
    Err(NError::NotPorted("pull"))
}
