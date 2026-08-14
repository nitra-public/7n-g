use clap::{Parser, Subcommand};
use n7n_git::{ch, getw, pull, push, Result};

#[derive(Parser)]
#[command(name = "n", version, about = "n — git-дельта CLI (getw/pull/push/ch)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Перенести дельту з worktree у поточну гілку.
    Getw,
    /// Накотити дельту origin/<гілка> у поточне робоче дерево.
    Pull { branch: Option<String> },
    /// Сквошити локальні зміни в один коміт і запушити.
    Push { branch: Option<String> },
    /// Тонка обгортка над nitra-cursor change.
    Ch,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    match cli.command {
        Command::Getw => getw::run(),
        Command::Pull { branch } => pull::run(branch.as_deref()),
        Command::Push { branch } => push::run(branch.as_deref()),
        Command::Ch => ch::run(),
    }
}
