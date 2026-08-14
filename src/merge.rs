//! Спільне ядро delta-merge — Rust-порт `merge.js` (24.5K, JS-оригінал у монорепо `7n`).
//!
//! Тіри авторезолву конфліктів (ADR 20260814-195911):
//! - Tier 0 — `git apply` еквівалент → `gix-merge` blob-merge без індексу.
//! - Tier 1 — пофайловий 3-way `git merge-file --diff3` → `gix-merge` diff3-режим.
//! - Tier 2 (опційно) — Mergiraf; оскільки він теж написаний на Rust, планується
//!   library-залежність замість спавну бінарника (ідея #41 сесії брейнштормингу).
//! - Tier 3 — LLM-агент через ACP (`llm-lib`, feature `agents`).
//!
//! Pre-flight бекап через `git stash create` → `gix-stash` (реалізований у gitoxide,
//! на відміну від worktree-операцій — див. `getw.rs`).

use crate::{NError, Result};

pub struct DeltaMergeOpts<'a> {
    pub ours: &'a str,
    pub src: &'a str,
}

pub struct DeltaMergeOutcome {
    pub markers_remaining: bool,
    pub stash_sha: Option<String>,
}

/// Rust-еквівалент `_n7merge_delta(ours, src)` з `merge.js`.
pub fn delta_merge(_opts: DeltaMergeOpts) -> Result<DeltaMergeOutcome> {
    Err(NError::NotPorted("merge"))
}
