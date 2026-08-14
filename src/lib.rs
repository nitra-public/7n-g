//! `n7n_git` — бібліотечне ядро CLI `n`. Кожен модуль відповідає одній команді
//! JS-оригіналу (`npm/src/*.js` у монорепо `7n`) — див. ADR 20260814-195911 у `docs/adr/`.

pub mod ch;
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
}

pub type Result<T> = std::result::Result<T, NError>;
