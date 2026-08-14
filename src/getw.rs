//! `g getw` — Rust-порт `getw.js` (10.4K). fzf-вибір `.worktrees/`-гілки → делегація
//! в [`crate::merge::delta_merge`] → видалення worktree.
//!
//! `.worktrees/`-операції лишаються на shell-`git`, а не `gix-worktree`: gitoxide не
//! реалізує взаємодію `GIT_COMMON_DIR`/`GIT_WORK_TREE` (перевірено в ADR 20260814-195911).

use crate::{NError, Result};

pub fn run() -> Result<()> {
    Err(NError::NotPorted("getw"))
}
