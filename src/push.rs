//! `n push [branch]` — Rust-порт `push.js` (31.9K, найбільший command-файл JS-оригіналу).
//! Squash локальних комітів + робочого дерева в один коміт, ACP-агент генерує
//! commit-меседж (Gitmoji + Monorepo / Conventional Commits), push з auto-pull дивергенції
//! через [`crate::merge::delta_merge`].

use crate::{NError, Result};

pub fn run(_branch: Option<&str>) -> Result<()> {
    Err(NError::NotPorted("push"))
}
