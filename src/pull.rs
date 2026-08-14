//! `n pull [branch]` — Rust-порт `pull()` з `npm/src/pull.js` (монорепо `7n`).
//!
//! `git fetch origin <branch>` → спершу справжній fast-forward
//! (`git merge --ff-only`), і лише коли FF неможливий (розбіжна історія або локальні
//! зміни перетинаються з апдейтом) — **reverse-delta**: знімає повний локальний стан
//! (`git stash create`), переводить HEAD на `origin/<branch>` (`git reset --hard`) і
//! накладає локальну дельту `merge-base(origin, backup)..backup` назад як unstaged
//! через [`crate::merge::delta_merge`] з оберненими ролями (`ours=origin`,
//! `src=backup`). Підсумок: HEAD = origin (чиста історія), локальна робота лежить
//! зверху незакоміченою — pull ідемпотентний і чисто лягає під push.
//!
//! **Відмінність від zsh-оригіналу**: замість bash `trap ... INT TERM` (нефіксований
//! до платформи механізм) — крос-платформний обробник з `ctrlc` (Windows Ctrl handler
//! / Unix signal), який на перерив під час reverse-delta вікна відкочує
//! `git reset --hard <old_head>` (+ `git stash apply`, якщо був знімок) і завершує
//! процес з кодом 130 — спрощена, але еквівалентна за наслідком гарантія: жодне
//! переривання не лишає репозиторій у проміжному стані (HEAD=origin, робота втрачена).

use std::path::Path;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::merge::{delta_merge, ConflictResolver, DeltaMergeOpts, DeltaMergeOutcome};
use crate::{NError, Result};

#[derive(Debug)]
pub struct PullBackup {
    pub old_head: String,
    pub stash_sha: Option<String>,
    /// Людсько-читабельна команда відкату (для друку викликачем).
    pub recover_hint: String,
}

#[derive(Debug)]
pub enum PullOutcome {
    AlreadyUpToDate,
    FastForwarded,
    ReverseDelta {
        backup: PullBackup,
        merge: Box<DeltaMergeOutcome>,
    },
}

impl PullOutcome {
    pub fn is_clean(&self) -> bool {
        match self {
            PullOutcome::ReverseDelta { merge, .. } => merge.is_clean(),
            _ => true,
        }
    }
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(NError::Io)
}

fn git_ok(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = run_git(cwd, args)?;
    if !out.status.success() {
        return Err(NError::GitCommand {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `git reset --hard <old_head>` (+ `git stash apply <sha>`, якщо є) — той самий
/// відкат, що друкується як `recover_hint`, лише виконаний, а не показаний.
fn revert(cwd: &Path, old_head: &str, stash_sha: Option<&str>) {
    let _ = run_git(cwd, &["reset", "--hard", old_head]);
    if let Some(sha) = stash_sha {
        let _ = run_git(cwd, &["stash", "apply", sha]);
    }
}

/// `n pull [branch]` — `branch = None` означає поточну гілку
/// (`git branch --show-current`).
pub fn run(
    cwd: &Path,
    branch: Option<&str>,
    resolver: Option<&dyn ConflictResolver>,
) -> Result<PullOutcome> {
    let inside = run_git(cwd, &["rev-parse", "--is-inside-work-tree"])?;
    if !inside.status.success() {
        return Err(NError::Message("Ви не в Git репозиторії.".into()));
    }

    let branch = match branch {
        Some(b) if !b.is_empty() => b.to_string(),
        _ => {
            let current = git_ok(cwd, &["branch", "--show-current"])?;
            if current.is_empty() {
                return Err(NError::Message(
                    "Не вдалося визначити гілку (detached HEAD?). Вкажи явно: n pull <branch>"
                        .into(),
                ));
            }
            current
        }
    };

    let fetch = run_git(cwd, &["fetch", "origin", &branch])?;
    if !fetch.status.success() {
        return Err(NError::Message(format!(
            "Не вдалося отримати origin/{branch} (перевір назву гілки та доступ до remote)."
        )));
    }

    let remote_ref = format!("origin/{branch}");
    let verify = run_git(cwd, &["rev-parse", "--verify", &remote_ref])?;
    if !verify.status.success() {
        return Err(NError::Message(format!("Гілку {remote_ref} не знайдено.")));
    }

    let old_head = git_ok(cwd, &["rev-parse", "HEAD"])?;
    let remote_sha = git_ok(cwd, &["rev-parse", &remote_ref])?;
    if old_head == remote_sha {
        return Ok(PullOutcome::AlreadyUpToDate);
    }

    let is_ancestor = run_git(cwd, &["merge-base", "--is-ancestor", "HEAD", &remote_ref])?
        .status
        .success();
    if is_ancestor {
        let ff = run_git(cwd, &["merge", "--ff-only", &remote_ref])?;
        if ff.status.success() {
            return Ok(PullOutcome::FastForwarded);
        }
    }

    // FF неможливий — reverse-delta. Знімок ПОВНОГО локального стану ДО reset:
    // git stash create робить commit-знімок (HEAD-дерево + staged + unstaged tracked),
    // НЕ чіпаючи робоче дерево. На чистому дереві create нічого не повертає — тоді
    // джерелом дельти стає сам старий HEAD.
    let stash_msg = format!("n7pull: backup before reverse-delta ({branch})");
    let create = run_git(cwd, &["stash", "create", &stash_msg])?;
    let stash_sha_raw = String::from_utf8_lossy(&create.stdout).trim().to_string();
    let (stash_sha, backup_ref) = if stash_sha_raw.is_empty() {
        (None, old_head.clone())
    } else {
        run_git(cwd, &["stash", "store", "-m", &stash_msg, &stash_sha_raw])?;
        (Some(stash_sha_raw.clone()), stash_sha_raw)
    };
    let recover_hint = match &stash_sha {
        Some(sha) => format!("git reset --hard {old_head} && git stash apply {sha}"),
        None => format!("git reset --hard {old_head}"),
    };

    // Крос-платформний еквівалент `trap ... INT TERM`: на перерив під час
    // reset+delta-вікна — синхронний відкат, потім вихід з кодом 130.
    let reverse_done = Arc::new(AtomicBool::new(false));
    {
        let reverse_done = Arc::clone(&reverse_done);
        let cwd = cwd.to_path_buf();
        let old_head = old_head.clone();
        let stash_sha = stash_sha.clone();
        // Ігноруємо помилку встановлення (напр. handler уже стоїть від попереднього
        // виклику pull у тому самому процесі) — це best-effort safety net, не критичний шлях.
        let _ = ctrlc::set_handler(move || {
            if !reverse_done.load(Ordering::SeqCst) {
                eprintln!("⚠️ Перервано — відкочую до локального стану...");
                revert(&cwd, &old_head, stash_sha.as_deref());
            }
            std::process::exit(130);
        });
    }

    let reset = run_git(cwd, &["reset", "--hard", &remote_ref])?;
    if !reset.status.success() {
        reverse_done.store(true, Ordering::SeqCst);
        return Err(NError::Message(format!(
            "Не вдалося перевести HEAD на {remote_ref} — локальний стан недоторканий."
        )));
    }

    let merge_result = delta_merge(
        DeltaMergeOpts {
            cwd,
            ours: &remote_ref,
            src: &backup_ref,
            ours_label: Some(&remote_ref),
            src_label: Some("локальна робота"),
        },
        resolver,
    );
    reverse_done.store(true, Ordering::SeqCst);

    let merge = merge_result?;
    Ok(PullOutcome::ReverseDelta {
        backup: PullBackup {
            old_head,
            stash_sha,
            recover_hint,
        },
        merge: Box::new(merge),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(cwd: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn write(dir: &Path, rel: &str, content: &str) {
        std::fs::write(dir.join(rel), content).unwrap();
    }

    /// upstream (bare-ish working repo, служить `origin`) + local (клон, гілка `main`).
    fn init_upstream_and_clone() -> (tempfile::TempDir, tempfile::TempDir) {
        let upstream = tempfile::tempdir().unwrap();
        git(upstream.path(), &["init", "-q", "-b", "main"]);
        git(
            upstream.path(),
            &["config", "user.email", "test@example.com"],
        );
        git(upstream.path(), &["config", "user.name", "Test"]);
        write(upstream.path(), "a.txt", "hello\n");
        git(upstream.path(), &["add", "-A"]);
        git(upstream.path(), &["commit", "-q", "-m", "init"]);

        let local = tempfile::tempdir().unwrap();
        let out = Command::new("git")
            .args([
                "clone",
                "-q",
                upstream.path().to_str().unwrap(),
                local.path().to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "clone failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        git(local.path(), &["config", "user.email", "test@example.com"]);
        git(local.path(), &["config", "user.name", "Test"]);

        (upstream, local)
    }

    #[test]
    fn already_up_to_date_when_nothing_changed() {
        let (_upstream, local) = init_upstream_and_clone();
        let outcome = run(local.path(), None, None).unwrap();
        assert!(matches!(outcome, PullOutcome::AlreadyUpToDate));
    }

    #[test]
    fn fast_forwards_when_local_has_no_divergence() {
        let (upstream, local) = init_upstream_and_clone();
        write(upstream.path(), "b.txt", "second commit\n");
        git(upstream.path(), &["add", "-A"]);
        git(upstream.path(), &["commit", "-q", "-m", "second"]);

        let outcome = run(local.path(), None, None).unwrap();
        assert!(matches!(outcome, PullOutcome::FastForwarded));
        assert_eq!(
            std::fs::read_to_string(local.path().join("b.txt")).unwrap(),
            "second commit\n"
        );
    }

    #[test]
    fn reverse_delta_when_history_diverged() {
        let (upstream, local) = init_upstream_and_clone();

        // upstream діагностично рухається вперед своїм комітом (не перетинається з локальним).
        write(upstream.path(), "a.txt", "hello\nfrom upstream\n");
        git(upstream.path(), &["add", "-A"]);
        git(
            upstream.path(),
            &["commit", "-q", "-m", "upstream advances"],
        );

        // локально — свій комміт (розбіжна історія, FF неможливий).
        write(local.path(), "c.txt", "local work\n");
        git(local.path(), &["add", "-A"]);
        git(local.path(), &["commit", "-q", "-m", "local commit"]);

        let old_local_head = git_ok(local.path(), &["rev-parse", "HEAD"]).unwrap();

        let outcome = run(local.path(), None, None).unwrap();
        match outcome {
            PullOutcome::ReverseDelta { backup, merge } => {
                assert_eq!(backup.old_head, old_local_head);
                assert!(
                    backup.stash_sha.is_none(),
                    "чисте дерево перед reset — snapshot = HEAD"
                );
                assert!(merge.is_clean());
            }
            _ => panic!("expected ReverseDelta"),
        }

        // HEAD тепер = origin/main (чиста історія), а локальна робота (c.txt) — unstaged зверху.
        let new_head = git_ok(local.path(), &["rev-parse", "HEAD"]).unwrap();
        let origin_head = git_ok(local.path(), &["rev-parse", "origin/main"]).unwrap();
        assert_eq!(new_head, origin_head);
        assert_eq!(
            std::fs::read_to_string(local.path().join("c.txt")).unwrap(),
            "local work\n"
        );
        assert_eq!(
            std::fs::read_to_string(local.path().join("a.txt")).unwrap(),
            "hello\nfrom upstream\n"
        );
        let status = git_ok(local.path(), &["status", "--porcelain"]).unwrap();
        assert!(
            status.contains("c.txt"),
            "c.txt має бути unstaged: {status}"
        );
    }

    #[test]
    fn errors_on_unknown_remote_branch() {
        let (_upstream, local) = init_upstream_and_clone();
        let err = run(local.path(), Some("does-not-exist"), None).unwrap_err();
        assert!(matches!(err, NError::Message(_)));
    }
}
