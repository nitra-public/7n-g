use std::io::Write;

use clap::{Parser, Subcommand};
use n7n_g::getw::{GetwOutcome, WorktreeCandidate, WorktreePicker};
use n7n_g::pull::PullOutcome;
use n7n_g::{ch, getw, pull, push, NError, Result};

#[derive(Parser)]
#[command(name = "g", version, about = "g — git-дельта CLI (getw/pull/push/ch)")]
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
        Command::Getw => run_getw(),
        Command::Pull { branch } => run_pull(branch.as_deref()),
        Command::Push { branch } => push::run(branch.as_deref()),
        Command::Ch => ch::run(),
    }
}

/// Тимчасовий stdin-пікер (нумерований список + `read_line`) — до нативного TUI
/// fuzzy-picker (ADR 20260814-195911, ідея #3: `skim`/`nucleo` замість `fzf`).
struct StdinPicker;

impl WorktreePicker for StdinPicker {
    fn pick<'a>(
        &self,
        candidates: &'a [WorktreeCandidate],
    ) -> std::io::Result<Option<&'a WorktreeCandidate>> {
        println!("Оберіть worktree для перенесення змін:");
        println!("   0) ❌ Відміна");
        for (i, c) in candidates.iter().enumerate() {
            println!("   {}) {}", i + 1, c.name);
            if let Some(task) = &c.task {
                println!("      Задача: {task}");
            }
            if let Some(created) = &c.created {
                println!("      🕒 Створено: {created}");
            }
            if let Some(modified) = &c.modified {
                println!("      ✏️  Змінено:  {modified}");
            }
        }
        print!("> ");
        std::io::stdout().flush()?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let choice: usize = input.trim().parse().unwrap_or(0);
        Ok(if choice == 0 {
            None
        } else {
            candidates.get(choice - 1)
        })
    }
}

fn run_getw() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let outcome = getw::run(&cwd, &StdinPicker, None)?;
    match outcome {
        GetwOutcome::NoWorktrees => {
            println!("📭 У папці .worktrees не знайдено жодного робочого дерева.");
            println!("Гарного дня! 👋");
            Ok(())
        }
        GetwOutcome::AllPruned { pruned } => {
            for p in &pruned {
                println!(
                    "🧹 Порожня дельта — прибрано: {} ({})",
                    p.branch,
                    p.path.display()
                );
            }
            println!(
                "📭 Усі worktree з .worktrees мали порожню дельту — прибрано, переносити нічого."
            );
            println!("Гарного дня! 👋");
            Ok(())
        }
        GetwOutcome::Cancelled => {
            println!("Дію скасовано. Всього найкращого! 👋✨");
            Ok(())
        }
        GetwOutcome::MergeUnresolved {
            target_branch,
            merge,
        } => {
            println!("📊 Підсумок мерджу (Unstaged):");
            println!("   Tier 1 (git):      {} файл(ів)", merge.tier1);
            println!("   Tier 2 (mergiraf): {} файл(ів)", merge.tier2);
            println!("   Tier 3 (LLM):      {} файл(ів)", merge.tier3.len());
            Err(NError::Message(format!(
                "Мерж не завершено — worktree '{target_branch}' збережено для ручного доведення."
            )))
        }
        GetwOutcome::Done {
            target_branch,
            merge,
            worktree_deleted,
        } => {
            println!("📊 Підсумок мерджу (Unstaged):");
            println!("   Tier 1 (git):      {} файл(ів)", merge.tier1);
            println!("   Tier 2 (mergiraf): {} файл(ів)", merge.tier2);
            println!("   Tier 3 (LLM):      {} файл(ів)", merge.tier3.len());
            if worktree_deleted {
                println!("✅ Успішно! Зміни перенесено, ворктрі {target_branch} видалено. Роботу завершено! 🚀");
            } else {
                println!("⚠️ Зміни перенесено, але не вдалося видалити worktree {target_branch}.");
            }
            Ok(())
        }
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
