use clap::{Parser, Subcommand};
use n7n_git::pull::PullOutcome;
use n7n_git::{ch, getw, pull, push, NError, Result};

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
        Command::Pull { branch } => run_pull(branch.as_deref()),
        Command::Push { branch } => push::run(branch.as_deref()),
        Command::Ch => ch::run(),
    }
}

fn run_pull(branch: Option<&str>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let outcome = pull::run(&cwd, branch, None)?;
    match outcome {
        PullOutcome::AlreadyUpToDate => {
            println!(
                "✅ Вже актуально — origin/{} збігається з HEAD.",
                branch.unwrap_or("<поточна>")
            );
            Ok(())
        }
        PullOutcome::FastForwarded => {
            println!("✅ Готово! HEAD переміщено на origin (fast-forward). 🚀");
            Ok(())
        }
        PullOutcome::ReverseDelta { backup, merge } => {
            println!(
                "🛟 Бекап локального стану збережено. Відкат: {}",
                backup.recover_hint
            );
            println!("📊 Підсумок мерджу (Unstaged):");
            println!("   Tier 1 (git):      {} файл(ів)", merge.tier1);
            println!("   Tier 2 (mergiraf): {} файл(ів)", merge.tier2);
            println!("   Tier 3 (LLM):      {} файл(ів)", merge.tier3.len());
            if merge.is_clean() {
                println!("✅ Готово! HEAD на origin, локальну роботу накладено як unstaged — переглянь і закоміть. 🚀");
                Ok(())
            } else {
                println!("❌ Лишилися конфліктні маркери:");
                for f in &merge.conflict_files {
                    println!("   • {f}");
                }
                Err(NError::Message(format!(
                    "Reverse-delta мерж не завершено — розв'яжи конфлікти (git diff), потім закоміть. Повний відкат: {}",
                    backup.recover_hint
                )))
            }
        }
    }
}
