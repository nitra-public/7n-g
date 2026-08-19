use clap::{Parser, Subcommand};
#[cfg(feature = "agents")]
use n7n_g::acp_agents::AcpAgentAdapter;
use n7n_g::ch::ChReport;
use n7n_g::getw::GetwOutcome;
use n7n_g::pull::PullOutcome;
use n7n_g::push::PushOutcome;
use n7n_g::tui_picker::TuiPicker;
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
    Ch {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    match cli.command {
        Command::Getw => run_getw(),
        Command::Pull { branch } => run_pull(branch.as_deref()),
        Command::Push { branch } => run_push(branch.as_deref()),
        Command::Ch { args } => run_ch(&args),
    }
}

fn run_getw() -> Result<()> {
    let cwd = std::env::current_dir()?;
    #[cfg(feature = "agents")]
    let agent = AcpAgentAdapter::new(&cwd, "g getw");
    #[cfg(feature = "agents")]
    let resolver = Some(&agent as &dyn n7n_g::merge::ConflictResolver);
    #[cfg(not(feature = "agents"))]
    let resolver: Option<&dyn n7n_g::merge::ConflictResolver> = None;

    let outcome = getw::run(&cwd, &TuiPicker, resolver)?;
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
    #[cfg(feature = "agents")]
    let agent = AcpAgentAdapter::new(&cwd, "g pull");
    #[cfg(feature = "agents")]
    let resolver = Some(&agent as &dyn n7n_g::merge::ConflictResolver);
    #[cfg(not(feature = "agents"))]
    let resolver: Option<&dyn n7n_g::merge::ConflictResolver> = None;

    let outcome = pull::run(&cwd, branch, resolver)?;
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

fn run_ch(args: &[String]) -> Result<()> {
    let cwd = std::env::current_dir()?;
    #[cfg(feature = "agents")]
    let agent = AcpAgentAdapter::new(&cwd, "g ch");
    #[cfg(feature = "agents")]
    let generator = Some(&agent as &dyn n7n_g::ch::MessageGenerator);
    #[cfg(not(feature = "agents"))]
    let generator: Option<&dyn n7n_g::ch::MessageGenerator> = None;

    let report = ch::run(&cwd, args, generator)?;
    match report {
        ChReport::Nothing { orphans } => {
            if !orphans.is_empty() {
                println!(
                    "ℹ️ Поза воркспейсами (CHANGELOG не ведеться, пропускаю): {}",
                    orphans.join(", ")
                );
            }
            println!("✅ Немає змін у воркспейсах, що ведуть CHANGELOG — нічого створювати. 👋");
            Ok(())
        }
        ChReport::Completed {
            orphans,
            skipped_by_path,
            written,
            failures,
        } => {
            if !orphans.is_empty() {
                println!(
                    "ℹ️ Поза воркспейсами (CHANGELOG не ведеться, пропускаю): {}",
                    orphans.join(", ")
                );
            }
            if !skipped_by_path.is_empty() {
                println!(
                    "↪️ --path звужує ціль, пропускаю: {}",
                    skipped_by_path.join(", ")
                );
            }
            for w in &written {
                println!("✅ {}", w.file.display());
            }
            for f in &failures {
                println!("❌ {}: {}", f.ws, f.reason);
            }
            if failures.is_empty() {
                Ok(())
            } else {
                Err(NError::Message(format!(
                    "{} з {} воркспейсів не вдалося обробити",
                    failures.len(),
                    written.len() + failures.len()
                )))
            }
        }
    }
}

fn run_push(branch: Option<&str>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    #[cfg(feature = "agents")]
    let agent = AcpAgentAdapter::new(&cwd, "g push");
    #[cfg(feature = "agents")]
    let resolver = Some(&agent as &dyn n7n_g::merge::ConflictResolver);
    #[cfg(feature = "agents")]
    let generator = Some(&agent as &dyn n7n_g::push::CommitMessageGenerator);
    #[cfg(not(feature = "agents"))]
    let resolver: Option<&dyn n7n_g::merge::ConflictResolver> = None;
    #[cfg(not(feature = "agents"))]
    let generator: Option<&dyn n7n_g::push::CommitMessageGenerator> = None;

    let outcome = push::run(&cwd, branch, resolver, generator)?;
    match outcome {
        PushOutcome::NothingToPush { base } => {
            println!("✅ Немає змін відносно {base} — пушити нічого. 👋");
            Ok(())
        }
        PushOutcome::Done {
            branch,
            subject,
            file_names,
            adr_count,
            pushed_new_branch,
            auto_pulled,
            ..
        } => {
            if auto_pulled.is_some() {
                println!("🔀 origin/{branch} мав нові коміти — підтягнуто автоматично.");
            }
            println!();
            println!("📝 Commit: {subject}");
            println!("📂 Файли:");
            for f in &file_names {
                println!("   {f}");
            }
            if adr_count > 0 {
                println!("   📄 docs/adr/: {adr_count} файл(ів)");
            }
            println!();
            println!("🚀 Пушу origin/{branch} одним комітом...");
            if pushed_new_branch {
                println!("✅ Готово! Нову гілку {branch} запушено (-u). 🚀");
            } else {
                println!("✅ Готово! Локальні зміни запушено одним комітом у origin/{branch}. 🚀");
            }
            Ok(())
        }
    }
}
