//! `n7n_g` — бібліотечне ядро CLI `g` (скорочено від git). Кожен модуль відповідає
//! одній команді JS-оригіналу (`npm/src/*.js` у монорепо `7n`) — див. ADR
//! 20260814-195911 у `docs/adr/`.

#[cfg(feature = "agents")]
pub mod acp_agents;
pub mod ch;
pub mod diff_context;
pub mod getw;
pub mod merge;
pub mod pull;
pub mod push;

#[derive(Debug, thiserror::Error)]
pub enum NError {
    #[error("git: {0}")]
    Git(#[from] Box<gix::open::Error>),

    #[error("{0} ще не портовано з JS — див. TODO в src/{0}.rs")]
    NotPorted(&'static str),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("git {args}: {stderr}")]
    GitCommand { args: String, stderr: String },

    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, NError>;

/// Еквівалент `git rev-parse --is-inside-work-tree`: `true` лише якщо `cwd`
/// лежить у не-bare репозиторії з робочим деревом (а не всередині `.git/`
/// чи в bare-репозиторії).
pub fn is_inside_work_tree(cwd: &std::path::Path) -> bool {
    gix::discover(cwd)
        .map(|repo| repo.work_dir().is_some())
        .unwrap_or(false)
}
