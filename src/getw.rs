//! `g getw` — Rust-порт `getw()` з `npm/src/getw.js` (монорепо `7n`).
//!
//! Через інтерактивний пікер обирає git-worktree з-під `.worktrees/`, комітить там
//! незакомічені зміни тимчасовим комітом і накочує ЛИШЕ дельту цієї гілки
//! (`merge-base..target`) у поточну гілку як unstaged через
//! [`crate::merge::delta_merge`]. Worktree з порожньою дельтою (ні незакомічених
//! змін, ні комітів поверх `merge-base` із поточною гілкою) прибирається мовчки під
//! час побудови списку — у пікері не показується. Після успішного мерджу (без
//! невирішених маркерів) worktree і його гілка видаляються.
//!
//! **Відмінність від zsh-оригіналу**: інтерактивний вибір — injectable
//! [`WorktreePicker`] (за ADR 20260814-195911: нативний TUI fuzzy-picker замість
//! `fzf`-бінарника — seam, аналогічний [`crate::merge::ConflictResolver`] для
//! Tier 3; реалізація — [`crate::tui_picker::TuiPicker`]). Бібліотека тому НЕ
//! ставить `fzf` через `brew` (це взагалі не потрібно — picker нативний,
//! in-process).
//! Формат `created`/`modified` — крос-платформний (`chrono`, локальний час) замість
//! macOS-специфічного `stat -f`/`date -r` з JS-оригіналу.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::merge::{delta_merge, ConflictResolver, DeltaMergeOpts, DeltaMergeOutcome};
use crate::{NError, Result};

#[derive(Debug, Clone)]
pub struct WorktreeCandidate {
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
    /// Текст після `**Задача:**` у `<path>.md`, якщо файл і рядок існують.
    pub task: Option<String>,
    /// `YYYY-MM-DD HH:MM` — час створення директорії (fallback: mtime).
    pub created: Option<String>,
    /// `YYYY-MM-DD HH:MM` — mtime найсвіжішого файлу (без `.git`/`node_modules`).
    pub modified: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PrunedWorktree {
    pub path: PathBuf,
    pub branch: String,
}

#[derive(Debug, Default)]
pub struct DiscoverOutcome {
    pub candidates: Vec<WorktreeCandidate>,
    pub pruned: Vec<PrunedWorktree>,
}

/// Tier для інтерактивного вибору worktree. `Ok(None)` — користувач скасував вибір.
pub trait WorktreePicker {
    fn pick<'a>(
        &self,
        candidates: &'a [WorktreeCandidate],
    ) -> std::io::Result<Option<&'a WorktreeCandidate>>;
}

#[derive(Debug)]
pub enum GetwOutcome {
    /// У `.worktrees/` немає жодного робочого дерева.
    NoWorktrees,
    /// Усі worktree мали порожню дельту — прибрано, вибирати нічого.
    AllPruned { pruned: Vec<PrunedWorktree> },
    /// Користувач скасував вибір у пікері.
    Cancelled,
    Done {
        target_branch: String,
        merge: Box<DeltaMergeOutcome>,
        worktree_deleted: bool,
    },
    /// Мердж лишив невирішені маркери — worktree збережено для ручного доведення.
    MergeUnresolved {
        target_branch: String,
        merge: Box<DeltaMergeOutcome>,
    },
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(NError::Io)
}

#[cfg(test)]
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

/// Еквівалент `_getw_delta_empty`: `true`, лише якщо немає незакомічених змін у
/// worktree, є визначений `merge-base`, і немає діффу `merge-base..wt_branch`. При
/// невизначеному `merge-base` — `false` (не чіпати те, що не змогли оцінити).
fn delta_is_empty(wt_path: &Path, wt_branch: &str, base_branch: &str) -> Result<bool> {
    if crate::gix_util::worktree_is_dirty(wt_path) {
        return Ok(false);
    }
    let Some(merge_base) = crate::gix_util::merge_base(wt_path, base_branch, wt_branch) else {
        return Ok(false);
    };
    Ok(!crate::gix_util::trees_differ(wt_path, &merge_base, wt_branch))
}

fn task_description(md_path: &Path) -> Option<String> {
    const MARKER: &str = "**Задача:**";
    let content = std::fs::read_to_string(md_path).ok()?;
    for line in content.lines() {
        if let Some(idx) = line.find(MARKER) {
            let rest = line[idx + MARKER.len()..].trim_start();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

fn format_time(t: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Local>::from(t)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

fn created_at(dir: &Path) -> Option<String> {
    let meta = std::fs::metadata(dir).ok()?;
    let t = meta.created().or_else(|_| meta.modified()).ok()?;
    Some(format_time(t))
}

/// mtime найсвіжішого файлу в дереві (без `.git`/`node_modules`); fallback — mtime директорії.
fn modified_at(dir: &Path) -> Option<String> {
    let newest = newest_file_mtime(dir);
    match newest {
        Some(t) => Some(format_time(t)),
        None => std::fs::metadata(dir)
            .ok()
            .and_then(|m| m.modified().ok())
            .map(format_time),
    }
}

fn newest_file_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if name == ".git" || name == "node_modules" {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(mtime) = meta.modified() {
                        if newest.is_none_or(|n| mtime > n) {
                            newest = Some(mtime);
                        }
                    }
                }
            }
        }
    }
    newest
}

/// Будує список worktree-кандидатів під `.worktrees/`, попутно прибираючи ті з
/// порожньою дельтою (worktree + гілка видаляються мовчки, у результат не потрапляють).
pub fn discover(cwd: &Path) -> Result<DiscoverOutcome> {
    let current_branch = crate::gix_util::current_branch(cwd).unwrap_or_default();
    let list = crate::gix_util::list_worktrees(cwd);

    let mut outcome = DiscoverOutcome::default();
    for (wt_path, branch) in list {
        if !wt_path.to_string_lossy().contains("/.worktrees/") {
            continue;
        }

        if wt_path != cwd {
            if let Some(branch) = &branch {
                if delta_is_empty(&wt_path, branch, &current_branch)? {
                    let _ = run_git(
                        cwd,
                        &["worktree", "remove", "-f", &wt_path.to_string_lossy()],
                    );
                    let _ = crate::gix_util::delete_branch(cwd, branch);
                    outcome.pruned.push(PrunedWorktree {
                        path: wt_path,
                        branch: branch.clone(),
                    });
                    continue;
                }
            }
        }

        let name = wt_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let md_path = PathBuf::from(format!("{}.md", wt_path.display()));
        outcome.candidates.push(WorktreeCandidate {
            task: task_description(&md_path),
            created: created_at(&wt_path),
            modified: modified_at(&wt_path),
            name,
            branch: branch.unwrap_or_default(),
            path: wt_path,
        });
    }

    Ok(outcome)
}

/// `g getw` — `cwd` має бути коренем поточного (не worktree-) робочого дерева.
pub fn run(
    cwd: &Path,
    picker: &dyn WorktreePicker,
    resolver: Option<&dyn ConflictResolver>,
) -> Result<GetwOutcome> {
    if !crate::is_inside_work_tree(cwd) {
        return Err(NError::Message("Ви не в Git репозиторії.".into()));
    }

    let current_branch = crate::gix_util::current_branch(cwd).unwrap_or_default();
    let discovered = discover(cwd)?;

    if discovered.candidates.is_empty() && discovered.pruned.is_empty() {
        return Ok(GetwOutcome::NoWorktrees);
    }
    if discovered.candidates.is_empty() {
        return Ok(GetwOutcome::AllPruned {
            pruned: discovered.pruned,
        });
    }

    let selected = picker.pick(&discovered.candidates).map_err(NError::Io)?;
    let Some(selected) = selected else {
        return Ok(GetwOutcome::Cancelled);
    };

    if selected.branch.is_empty() {
        return Err(NError::Message("Не вдалося визначити гілку.".into()));
    }
    let target_branch = selected.branch.clone();
    let target_wt_path = selected.path.clone();

    // Тимчасовий коміт незакомічених змін у worktree (щоб delta_merge бачив їх у гілці).
    run_git(&target_wt_path, &["add", "-A"])?;
    if crate::gix_util::index_differs_from_tree(&target_wt_path, "HEAD") {
        run_git(
            &target_wt_path,
            &["commit", "-m", "temp_merge_before_pull", "--no-verify"],
        )?;
    }

    let merge = delta_merge(
        DeltaMergeOpts {
            cwd,
            ours: &current_branch,
            src: &target_branch,
            ours_label: None,
            src_label: None,
        },
        resolver,
    )?;

    if !merge.is_clean() {
        return Ok(GetwOutcome::MergeUnresolved {
            target_branch,
            merge: Box::new(merge),
        });
    }

    let removed = run_git(
        cwd,
        &[
            "worktree",
            "remove",
            "-f",
            &target_wt_path.to_string_lossy(),
        ],
    )?;
    let worktree_deleted = if removed.status.success() {
        if !crate::gix_util::delete_branch(cwd, &target_branch) {
            return Err(NError::Message(format!(
                "Не вдалося видалити гілку {target_branch}."
            )));
        }
        true
    } else {
        false
    };

    Ok(GetwOutcome::Done {
        target_branch,
        merge: Box::new(merge),
        worktree_deleted,
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
        if let Some(parent) = dir.join(rel).parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(dir.join(rel), content).unwrap();
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "test@example.com"]);
        git(dir.path(), &["config", "user.name", "Test"]);
        write(dir.path(), "a.txt", "hello\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);
        dir
    }

    fn add_worktree(repo: &Path, name: &str) -> PathBuf {
        let wt_path = repo.join(".worktrees").join(name);
        git(
            repo,
            &[
                "worktree",
                "add",
                "-b",
                name,
                wt_path.to_str().unwrap(),
                "main",
            ],
        );
        wt_path
    }

    struct FirstPicker;
    impl WorktreePicker for FirstPicker {
        fn pick<'a>(
            &self,
            candidates: &'a [WorktreeCandidate],
        ) -> std::io::Result<Option<&'a WorktreeCandidate>> {
            Ok(candidates.first())
        }
    }

    struct CancelPicker;
    impl WorktreePicker for CancelPicker {
        fn pick<'a>(
            &self,
            _candidates: &'a [WorktreeCandidate],
        ) -> std::io::Result<Option<&'a WorktreeCandidate>> {
            Ok(None)
        }
    }

    #[test]
    fn no_worktrees_dir() {
        let repo = init_repo();
        let outcome = run(repo.path(), &FirstPicker, None).unwrap();
        assert!(matches!(outcome, GetwOutcome::NoWorktrees));
    }

    #[test]
    fn prunes_worktree_with_empty_delta() {
        let repo = init_repo();
        add_worktree(repo.path(), "empty-one");

        let outcome = run(repo.path(), &FirstPicker, None).unwrap();
        match outcome {
            GetwOutcome::AllPruned { pruned } => {
                assert_eq!(pruned.len(), 1);
                assert_eq!(pruned[0].branch, "empty-one");
            }
            other => panic!("expected AllPruned, got {other:?}"),
        }
        let list = git_ok(repo.path(), &["worktree", "list"]).unwrap();
        assert!(!list.contains("empty-one"));
    }

    #[test]
    fn cancel_leaves_worktree_untouched() {
        let repo = init_repo();
        let wt = add_worktree(repo.path(), "feature");
        write(&wt, "b.txt", "new\n");
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-q", "-m", "add b.txt"]);

        let outcome = run(repo.path(), &CancelPicker, None).unwrap();
        assert!(matches!(outcome, GetwOutcome::Cancelled));
        assert!(wt.is_dir());
    }

    #[test]
    fn merges_delta_and_deletes_worktree() {
        let repo = init_repo();
        let wt = add_worktree(repo.path(), "feature");
        write(&wt, "b.txt", "new file\n");
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-q", "-m", "add b.txt"]);

        let outcome = run(repo.path(), &FirstPicker, None).unwrap();
        match outcome {
            GetwOutcome::Done {
                target_branch,
                merge,
                worktree_deleted,
            } => {
                assert_eq!(target_branch, "feature");
                assert!(merge.is_clean());
                assert!(worktree_deleted);
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert!(!wt.is_dir(), "worktree dir should be removed");
        assert_eq!(
            std::fs::read_to_string(repo.path().join("b.txt")).unwrap(),
            "new file\n"
        );
        let branches = git_ok(repo.path(), &["branch", "--list", "feature"]).unwrap();
        assert!(branches.is_empty(), "feature branch should be deleted");
    }

    #[test]
    fn commits_uncommitted_worktree_changes_before_merge() {
        let repo = init_repo();
        let wt = add_worktree(repo.path(), "feature");
        // НЕзакомічена зміна у worktree — має піти тимчасовим комітом перед мерджем.
        write(&wt, "c.txt", "uncommitted\n");

        let outcome = run(repo.path(), &FirstPicker, None).unwrap();
        assert!(matches!(outcome, GetwOutcome::Done { .. }));
        assert_eq!(
            std::fs::read_to_string(repo.path().join("c.txt")).unwrap(),
            "uncommitted\n"
        );
    }

    #[test]
    fn unresolved_conflict_keeps_worktree() {
        let repo = init_repo();
        write(repo.path(), "a.txt", "A\nB\nC\n");
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "expand a.txt"]);

        let wt = add_worktree(repo.path(), "feature");
        write(&wt, "a.txt", "A\nB-feature\nC\n");
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-q", "-m", "feature changes B"]);

        write(repo.path(), "a.txt", "A\nB-main\nC\n");
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "main changes B too"]);

        let outcome = run(repo.path(), &FirstPicker, None).unwrap();
        match outcome {
            GetwOutcome::MergeUnresolved {
                target_branch,
                merge,
            } => {
                assert_eq!(target_branch, "feature");
                assert!(!merge.is_clean());
            }
            other => panic!("expected MergeUnresolved, got {other:?}"),
        }
        assert!(wt.is_dir(), "worktree must survive an unresolved merge");
    }
}
