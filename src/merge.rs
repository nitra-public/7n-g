//! Спільне ядро delta-merge — Rust-порт `merge.js` (426 рядків, JS-оригінал —
//! `npm/src/merge.js` у монорепо `7n`). Обидві команди (`getw`, `pull`) переносять у
//! поточне робоче дерево як unstaged ЛИШЕ дельту `merge-base(ours, src)..src` (а не
//! весь зріз `src`), щоб не затирати файли, які поточна сторона змінювала самостійно.
//!
//! Тіри авторезолву конфліктів:
//! - **Tier 0** — чистий `git apply` (без `--index`) усієї дельти одразу.
//! - **Tier 1** — пофайловий 3-way `git merge-file --diff3` (без індексу).
//! - **Tier 2** (опційно) — структурний AST-авторезолвер `mergiraf solve`, лише якщо
//!   вже є в `PATH` (на відміну від JS-оригіналу, ця бібліотека НЕ ставить його сама
//!   через brew/cargo — auto-install є side-effect, недоречний у переюзовуваному
//!   crate; це відповідальність CLI-шару).
//! - **Tier 3** — LLM-агент через injectable [`ConflictResolver`] (ACP/`llm-lib` —
//!   окрема задача портування, ще не підключена; без резолвера файли лишаються з
//!   маркерами).
//!
//! Pre-flight бекап через `git stash create` — commit-знімок незакомічених змін, що
//! НЕ чіпає робоче дерево (`gix-stash` реалізований у gitoxide, але цей порт свідомо
//! лишається на shell-`git` для всіх git-plumbing операцій — ADR 20260814-195911:
//! "gitoxide поетапно", міграція conflicting-heavy шляхів — окрема задача з власними
//! тестами на паритет поведінки).
//!
//! **Архітектурна відмінність від zsh-оригіналу**: ядро тут нічого не друкує й не
//! запускає `bun install` автоматично — повертає структурований [`DeltaMergeOutcome`],
//! рендер і побічні дії (bun install, brew install mergiraf) — відповідальність
//! викликача (`getw`/`pull`/CLI-шар). Це узгоджено з рішенням сесії "один crate — і
//! CLI, і library" (ADR, ідея #51): бібліотека, підключена в інший Rust-проєкт, не
//! повинна мовчки друкувати в stdout чи ставити бінарники.

use std::path::Path;
use std::process::{Command, Output};

use crate::{NError, Result};

const LOCK_FILES_TAKE_SRC: &[&str] = &["package-lock.json", "pnpm-lock.yaml", "yarn.lock"];

pub struct DeltaMergeOpts<'a> {
    pub cwd: &'a Path,
    pub ours: &'a str,
    pub src: &'a str,
    /// Людський підпис сторони `ours` лише для звіту (дефолт — сам `ours`-ref).
    pub ours_label: Option<&'a str>,
    /// Людський підпис сторони `src` лише для звіту (дефолт — сам `src`-ref).
    pub src_label: Option<&'a str>,
}

/// Tier 3 — резолв конфліктних маркерів агентом. Викликається лише на файли, що
/// лишились нерозв'язаними після Tier 0-2. Реалізації: ACP (`llm-lib`, cloud) чи
/// локальний omlx-резолвер — обидві поки не портовані, це injectable seam для них.
pub trait ConflictResolver {
    /// `files` — repo-relative шляхи з маркерами (`cwd` з [`DeltaMergeOpts`] — корінь).
    /// Повертає per-file людський підсумок (thinking/коментар) для звіту.
    fn resolve(&self, cwd: &Path, files: &[String]) -> std::io::Result<String>;
}

#[derive(Debug, Clone)]
pub struct RescuedFile {
    pub path: String,
    /// Сторона, яка файл видалила.
    pub deleted_by: RescueSide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RescueSide {
    Ours,
    Src,
}

#[derive(Debug, Clone)]
pub struct Tier3Conflict {
    pub path: String,
    /// Diff3-маркований вміст файлу до спроби резолву (для рендеру блоків/diff опісля).
    pub pre_content: String,
}

impl Tier3Conflict {
    pub fn ours_block(&self) -> String {
        extract_block(&self.pre_content, BlockKind::Ours)
    }

    pub fn theirs_block(&self) -> String {
        extract_block(&self.pre_content, BlockKind::Theirs)
    }
}

#[derive(Debug, Default)]
pub struct DeltaMergeOutcome {
    pub backup_stash_sha: Option<String>,
    /// Дельта `merge-base(ours, src)..src` порожня — переносити нічого.
    pub empty_delta: bool,
    /// `true`, якщо Tier 0 (`git apply` усієї дельти) спрацював одразу.
    pub applied_clean: bool,
    pub total_files: usize,
    pub tier1: usize,
    pub tier2: usize,
    pub tier3: Vec<Tier3Conflict>,
    pub rescued: Vec<RescuedFile>,
    pub lock_files_taken_from_src: Vec<String>,
    /// Файли, що й після Tier 3 лишились з конфліктними маркерами (порожньо = успіх).
    pub conflict_files: Vec<String>,
    /// Кореневий `bun.lock` відрізнявся від `src` — викликачу варто перегенерувати
    /// (`bun install`), якщо після резолву конфліктів немає.
    pub regen_bun_lock: bool,
    /// Коли Tier 3 викликався — сирий per-file коментар резолвера.
    pub agent_summary: Option<String>,
}

impl DeltaMergeOutcome {
    pub fn is_clean(&self) -> bool {
        self.conflict_files.is_empty()
    }
}

enum BlockKind {
    Ours,
    Theirs,
}

/// Еквівалент `_n7merge_block_ours`/`_n7merge_block_theirs` (awk-стейтмашина в JS).
fn extract_block(diff3_content: &str, kind: BlockKind) -> String {
    let mut lines = Vec::new();
    let mut inside = false;
    for line in diff3_content.lines() {
        match kind {
            BlockKind::Ours => {
                if line.starts_with("<<<<<<< ") {
                    inside = true;
                    continue;
                }
                if line.starts_with("|||||||") || line == "=======" || line.starts_with(">>>>>>> ")
                {
                    inside = false;
                    continue;
                }
            }
            BlockKind::Theirs => {
                if line == "=======" {
                    inside = true;
                    continue;
                }
                if line.starts_with(">>>>>>> ") {
                    inside = false;
                    continue;
                }
            }
        }
        if inside {
            lines.push(line);
        }
    }
    lines.join("\n")
}

fn has_markers(content: &str) -> bool {
    content
        .lines()
        .any(|l| l.starts_with("<<<<<<<") || l.starts_with(">>>>>>>"))
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(NError::Io)
}

/// `git show <ref>:<path>` — `None`, якщо шлях не існує на цьому ref (типово для
/// новостворених/видалених файлів; JS-еквівалент: `git show ... 2>/dev/null || : > tmp`).
fn show(cwd: &Path, git_ref: &str, rel: &str) -> Result<Option<Vec<u8>>> {
    Ok(crate::gix_util::read_blob_at(cwd, git_ref, rel))
}

fn file_exists_at(cwd: &Path, git_ref: &str, rel: &str) -> Result<bool> {
    Ok(crate::gix_util::path_exists_at(cwd, git_ref, rel))
}

/// Еквівалент `_n7merge_bun_lock_differs`: порівнює кореневий `bun.lock` поточної
/// сторони (робоче дерево, якщо є, інакше `ours:bun.lock`) з `src:bun.lock`.
fn bun_lock_differs(cwd: &Path, ours: &str, src: &str) -> Result<bool> {
    let ours_bytes = if cwd.join("bun.lock").is_file() {
        std::fs::read(cwd.join("bun.lock"))?
    } else {
        show(cwd, ours, "bun.lock")?.unwrap_or_default()
    };
    let src_bytes = show(cwd, src, "bun.lock")?.unwrap_or_default();
    Ok(ours_bytes != src_bytes)
}

fn mergiraf_available() -> bool {
    let disabled = std::env::var("N7MERGE_NO_MERGIRAF")
        .or_else(|_| std::env::var("GETW_NO_MERGIRAF"))
        .map(|v| v == "1")
        .unwrap_or(false);
    if disabled {
        return false;
    }
    which_on_path("mergiraf")
}

fn which_on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file()))
        .unwrap_or(false)
}

/// Tier 2: `mergiraf solve <file>` in-place. `true`, якщо файл повністю розв'язано.
fn mergiraf_solve(cwd: &Path, rel: &str) -> bool {
    let ran = Command::new("mergiraf")
        .args(["solve", rel])
        .current_dir(cwd)
        .output();
    match ran {
        Ok(_) => match std::fs::read_to_string(cwd.join(rel)) {
            Ok(content) => !has_markers(&content),
            Err(_) => false,
        },
        Err(_) => false,
    }
}

/// Ядро: переносить дельту `merge-base(ours, src)..src` у `opts.cwd` як unstaged.
/// Багаторівнево: `git apply` → пофайловий 3-way → mergiraf → `resolver` (Tier 3).
/// `resolver = None` — Tier 3 пропускається, файли з маркерами лишаються в
/// `conflict_files` (чесний незавершений стан, а не мовчазна відмова).
pub fn delta_merge(
    opts: DeltaMergeOpts,
    resolver: Option<&dyn ConflictResolver>,
) -> Result<DeltaMergeOutcome> {
    let cwd = opts.cwd;
    let ours = opts.ours;
    let src = opts.src;
    let ours_label = opts.ours_label.unwrap_or(ours);
    let src_label = opts.src_label.unwrap_or(src);

    let mut outcome = DeltaMergeOutcome::default();

    // Pre-flight: git stash create — commit-знімок, що не чіпає робоче дерево.
    let stash_msg = format!("n7merge: backup before delta ({ours_label} <- {src_label})");
    let create = run_git(cwd, &["stash", "create", &stash_msg])?;
    let sha = String::from_utf8_lossy(&create.stdout).trim().to_string();
    if !sha.is_empty() {
        run_git(cwd, &["stash", "store", "-m", &stash_msg, &sha])?;
        outcome.backup_stash_sha = Some(sha);
    }

    let merge_base = crate::gix_util::merge_base(cwd, ours, src)
        .ok_or_else(|| NError::Message(format!("немає спільного предка для {ours} і {src}")))?;

    // --no-renames: rename = delete(old)+add(new), обидва кейси покриті циклом нижче.
    let changed_files = crate::gix_util::changed_paths(cwd, &merge_base, src);
    outcome.total_files = changed_files.len();

    let patch = run_git(cwd, &["diff", "--binary", &merge_base, src])?.stdout;
    if patch.is_empty() {
        outcome.empty_delta = true;
        return Ok(outcome);
    }

    // Tier 0: чистий apply усієї дельти одразу.
    let mut patch_file = tempfile::NamedTempFile::new()?;
    std::io::Write::write_all(&mut patch_file, &patch)?;
    let apply = Command::new("git")
        .args(["apply", "--whitespace=nowarn"])
        .arg(patch_file.path())
        .current_dir(cwd)
        .output()
        .map_err(NError::Io)?;
    if apply.status.success() {
        outcome.applied_clean = true;
        outcome.tier1 = outcome.total_files;
        return Ok(outcome);
    }

    let mergiraf_ok = mergiraf_available();
    let mut conflict_paths: Vec<String> = Vec::new();

    for rel in &changed_files {
        if !file_exists_at(cwd, src, rel)? {
            // Видалено у src: прибираємо локально лише якщо ours файл не міняла.
            let working = cwd.join(rel);
            if working.is_file() {
                let base_bytes = show(cwd, &merge_base, rel)?.unwrap_or_default();
                let working_bytes = std::fs::read(&working)?;
                if base_bytes == working_bytes {
                    std::fs::remove_file(&working)?;
                    outcome.tier1 += 1;
                } else {
                    outcome.rescued.push(RescuedFile {
                        path: rel.clone(),
                        deleted_by: RescueSide::Src,
                    });
                    outcome.tier1 += 1;
                }
            }
            continue;
        }

        let basename = Path::new(rel)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // bun.lock лише в корені репо — не мержимо; regen лише якщо lock відрізняється.
        if rel == "bun.lock" {
            if bun_lock_differs(cwd, ours, src)? {
                outcome.regen_bun_lock = true;
            }
            outcome.tier1 += 1;
            continue;
        }
        // Інші lock-файли: пофайловий merge-file дає лише шум — беремо версію src.
        if LOCK_FILES_TAKE_SRC.contains(&basename.as_str()) {
            if let Some(dir) = Path::new(rel).parent() {
                std::fs::create_dir_all(cwd.join(dir))?;
            }
            if let Some(bytes) = show(cwd, src, rel)? {
                std::fs::write(cwd.join(rel), bytes)?;
            }
            outcome.lock_files_taken_from_src.push(rel.clone());
            outcome.tier1 += 1;
            continue;
        }

        let base_bytes = show(cwd, &merge_base, rel)?.unwrap_or_default();
        let theirs_bytes = show(cwd, src, rel)?.unwrap_or_default();
        let working = cwd.join(rel);
        let ours_exists = working.is_file();

        // Дзеркало верхньої гілки: ours видалив файл, що існував у базі й змінений у
        // src (інакше не потрапив би у changed_files) — modify-beats-delete, лишаємо src.
        if !ours_exists && show(cwd, &merge_base, rel)?.is_some() {
            if let Some(dir) = Path::new(rel).parent() {
                std::fs::create_dir_all(cwd.join(dir))?;
            }
            std::fs::write(&working, &theirs_bytes)?;
            outcome.rescued.push(RescuedFile {
                path: rel.clone(),
                deleted_by: RescueSide::Ours,
            });
            outcome.tier1 += 1;
            continue;
        }

        let ours_bytes = if ours_exists {
            std::fs::read(&working)?
        } else {
            Vec::new()
        };

        let (mf_status, merged) = merge_file_diff3(
            &ours_bytes,
            &base_bytes,
            &theirs_bytes,
            &format!("поточна ({ours_label})"),
            "база",
            &format!("джерело ({src_label})"),
        )?;

        if let Some(dir) = Path::new(rel).parent() {
            std::fs::create_dir_all(cwd.join(dir))?;
        }

        match mf_status {
            MergeFileStatus::Error => {
                // 3-way неможливий (ймовірно бінарний) — беремо версію src.
                std::fs::write(&working, &theirs_bytes)?;
                outcome.tier1 += 1;
            }
            MergeFileStatus::Clean => {
                std::fs::write(&working, &merged)?;
                outcome.tier1 += 1;
            }
            MergeFileStatus::Conflict => {
                std::fs::write(&working, &merged)?;
                if mergiraf_ok && mergiraf_solve(cwd, rel) {
                    outcome.tier2 += 1;
                } else {
                    outcome.tier3.push(Tier3Conflict {
                        path: rel.clone(),
                        pre_content: String::from_utf8_lossy(&merged).to_string(),
                    });
                    conflict_paths.push(rel.clone());
                }
            }
        }
    }

    // Tier 3: резолвер прибирає маркери; вердикт (лишились чи ні) виносимо тут.
    if !conflict_paths.is_empty() {
        if let Some(resolver) = resolver {
            match resolver.resolve(cwd, &conflict_paths) {
                Ok(summary) => {
                    outcome.agent_summary = Some(summary);
                    outcome.conflict_files = conflict_paths
                        .iter()
                        .filter(|rel| {
                            std::fs::read_to_string(cwd.join(rel))
                                .map(|c| has_markers(&c))
                                .unwrap_or(true)
                        })
                        .cloned()
                        .collect();
                }
                Err(_) => outcome.conflict_files = conflict_paths,
            }
        } else {
            outcome.conflict_files = conflict_paths;
        }
    }

    Ok(outcome)
}

enum MergeFileStatus {
    Clean,
    Conflict,
    Error,
}

/// `git merge-file --diff3 -p` через тимчасові файли (сам `git merge-file` не читає
/// зі stdin) — повертає (статус, злитий вміст із diff3-маркерами при конфлікті).
fn merge_file_diff3(
    ours: &[u8],
    base: &[u8],
    theirs: &[u8],
    ours_label: &str,
    base_label: &str,
    theirs_label: &str,
) -> Result<(MergeFileStatus, Vec<u8>)> {
    let mut ours_f = tempfile::NamedTempFile::new()?;
    let mut base_f = tempfile::NamedTempFile::new()?;
    let mut theirs_f = tempfile::NamedTempFile::new()?;
    std::io::Write::write_all(&mut ours_f, ours)?;
    std::io::Write::write_all(&mut base_f, base)?;
    std::io::Write::write_all(&mut theirs_f, theirs)?;

    let out = Command::new("git")
        .args(["merge-file", "--diff3", "-p"])
        .args(["-L", ours_label, "-L", base_label, "-L", theirs_label])
        .arg(ours_f.path())
        .arg(base_f.path())
        .arg(theirs_f.path())
        .output()
        .map_err(NError::Io)?;

    let status = match out.status.code() {
        Some(0) => MergeFileStatus::Clean,
        Some(255) | None => MergeFileStatus::Error,
        Some(_) => MergeFileStatus::Conflict,
    };
    Ok((status, out.stdout))
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

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "test@example.com"]);
        git(dir.path(), &["config", "user.name", "Test"]);
        dir
    }

    fn write(dir: &Path, rel: &str, content: &str) {
        std::fs::write(dir.join(rel), content).unwrap();
    }

    #[test]
    fn empty_delta_when_ours_equals_src() {
        let dir = init_repo();
        write(dir.path(), "a.txt", "hello\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);

        let outcome = delta_merge(
            DeltaMergeOpts {
                cwd: dir.path(),
                ours: "main",
                src: "main",
                ours_label: None,
                src_label: None,
            },
            None,
        )
        .unwrap();

        assert!(outcome.empty_delta);
        assert!(outcome.is_clean());
    }

    #[test]
    fn tier0_clean_apply_for_new_file() {
        let dir = init_repo();
        write(dir.path(), "a.txt", "hello\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);

        git(dir.path(), &["checkout", "-q", "-b", "feature"]);
        write(dir.path(), "b.txt", "new file\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "add b.txt"]);
        git(dir.path(), &["checkout", "-q", "main"]);

        let outcome = delta_merge(
            DeltaMergeOpts {
                cwd: dir.path(),
                ours: "main",
                src: "feature",
                ours_label: None,
                src_label: None,
            },
            None,
        )
        .unwrap();

        assert!(outcome.applied_clean);
        assert!(outcome.is_clean());
        assert_eq!(outcome.tier1, 1);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "new file\n"
        );
    }

    #[test]
    fn tier1_3way_merges_non_overlapping_changes() {
        let dir = init_repo();
        write(dir.path(), "a.txt", "A\nB\nC\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);

        git(dir.path(), &["checkout", "-q", "-b", "src-branch"]);
        write(dir.path(), "a.txt", "A\nB\nC-src\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "src changes C"]);

        git(dir.path(), &["checkout", "-q", "-b", "ours-branch", "main"]);
        write(dir.path(), "a.txt", "A-ours\nB\nC\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "ours changes A"]);

        let outcome = delta_merge(
            DeltaMergeOpts {
                cwd: dir.path(),
                ours: "ours-branch",
                src: "src-branch",
                ours_label: None,
                src_label: None,
            },
            None,
        )
        .unwrap();

        assert!(
            !outcome.applied_clean,
            "context mismatch must force per-file 3-way"
        );
        assert!(
            outcome.is_clean(),
            "non-overlapping edits must merge without conflict"
        );
        assert_eq!(outcome.tier1, 1);
        assert_eq!(outcome.tier2, 0);
        assert!(outcome.tier3.is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "A-ours\nB\nC-src\n"
        );
    }

    #[test]
    fn tier3_conflict_stays_unresolved_without_resolver() {
        let dir = init_repo();
        write(dir.path(), "a.txt", "A\nB\nC\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);

        git(dir.path(), &["checkout", "-q", "-b", "src-branch"]);
        write(dir.path(), "a.txt", "A\nB-src\nC\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "src changes B"]);

        git(dir.path(), &["checkout", "-q", "-b", "ours-branch", "main"]);
        write(dir.path(), "a.txt", "A\nB-ours\nC\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "ours changes B too"]);

        let outcome = delta_merge(
            DeltaMergeOpts {
                cwd: dir.path(),
                ours: "ours-branch",
                src: "src-branch",
                ours_label: Some("ours"),
                src_label: Some("src"),
            },
            None,
        )
        .unwrap();

        assert!(!outcome.is_clean());
        assert_eq!(outcome.conflict_files, vec!["a.txt".to_string()]);
        assert_eq!(outcome.tier3.len(), 1);
        assert!(outcome.tier3[0].pre_content.contains("<<<<<<<"));
        assert!(outcome.tier3[0].ours_block().contains("B-ours"));
        assert!(outcome.tier3[0].theirs_block().contains("B-src"));
    }

    #[test]
    fn rescued_file_deleted_by_src_but_modified_by_ours() {
        let dir = init_repo();
        write(dir.path(), "a.txt", "keep me\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);

        git(dir.path(), &["checkout", "-q", "-b", "src-branch"]);
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "src deletes a.txt"]);

        git(dir.path(), &["checkout", "-q", "-b", "ours-branch", "main"]);
        write(dir.path(), "a.txt", "keep me, modified\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "ours modifies a.txt"]);

        let outcome = delta_merge(
            DeltaMergeOpts {
                cwd: dir.path(),
                ours: "ours-branch",
                src: "src-branch",
                ours_label: None,
                src_label: None,
            },
            None,
        )
        .unwrap();

        assert!(outcome.is_clean());
        assert_eq!(outcome.rescued.len(), 1);
        assert_eq!(outcome.rescued[0].path, "a.txt");
        assert_eq!(outcome.rescued[0].deleted_by, RescueSide::Src);
        assert!(dir.path().join("a.txt").is_file());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "keep me, modified\n"
        );
    }

    #[test]
    fn rescued_file_deleted_by_ours_but_modified_by_src() {
        let dir = init_repo();
        write(dir.path(), "a.txt", "keep me\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);

        git(dir.path(), &["checkout", "-q", "-b", "src-branch"]);
        write(dir.path(), "a.txt", "keep me, modified by src\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "src modifies a.txt"]);

        git(dir.path(), &["checkout", "-q", "-b", "ours-branch", "main"]);
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "ours deletes a.txt"]);

        let outcome = delta_merge(
            DeltaMergeOpts {
                cwd: dir.path(),
                ours: "ours-branch",
                src: "src-branch",
                ours_label: None,
                src_label: None,
            },
            None,
        )
        .unwrap();

        assert!(outcome.is_clean());
        assert_eq!(outcome.rescued.len(), 1);
        assert_eq!(outcome.rescued[0].path, "a.txt");
        assert_eq!(outcome.rescued[0].deleted_by, RescueSide::Ours);
        assert!(dir.path().join("a.txt").is_file());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "keep me, modified by src\n"
        );
    }

    #[test]
    fn lock_file_take_src_on_conflicting_changes() {
        let dir = init_repo();
        write(dir.path(), "package-lock.json", "{\"version\": \"base\"}\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);

        git(dir.path(), &["checkout", "-q", "-b", "src-branch"]);
        write(
            dir.path(),
            "package-lock.json",
            "{\"version\": \"from-src\"}\n",
        );
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "src changes lock"]);

        git(dir.path(), &["checkout", "-q", "-b", "ours-branch", "main"]);
        write(
            dir.path(),
            "package-lock.json",
            "{\"version\": \"from-ours-conflicting\"}\n",
        );
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "ours changes lock too"]);

        let outcome = delta_merge(
            DeltaMergeOpts {
                cwd: dir.path(),
                ours: "ours-branch",
                src: "src-branch",
                ours_label: None,
                src_label: None,
            },
            None,
        )
        .unwrap();

        assert!(outcome.is_clean());
        assert!(outcome.tier3.is_empty());
        assert_eq!(
            outcome.lock_files_taken_from_src,
            vec!["package-lock.json".to_string()]
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("package-lock.json")).unwrap(),
            "{\"version\": \"from-src\"}\n"
        );
    }

    #[test]
    fn bun_lock_regen_flag_set_when_differs() {
        let dir = init_repo();
        write(dir.path(), "bun.lock", "base\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);

        git(dir.path(), &["checkout", "-q", "-b", "src-branch"]);
        write(dir.path(), "bun.lock", "from-src\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "src changes bun.lock"]);

        git(dir.path(), &["checkout", "-q", "-b", "ours-branch", "main"]);
        write(dir.path(), "bun.lock", "from-ours\n");
        git(dir.path(), &["add", "-A"]);
        git(
            dir.path(),
            &["commit", "-q", "-m", "ours changes bun.lock differently"],
        );

        let outcome = delta_merge(
            DeltaMergeOpts {
                cwd: dir.path(),
                ours: "ours-branch",
                src: "src-branch",
                ours_label: None,
                src_label: None,
            },
            None,
        )
        .unwrap();

        assert!(outcome.is_clean());
        assert!(outcome.regen_bun_lock);
        assert!(outcome.lock_files_taken_from_src.is_empty());
        // bun.lock не мержиться пофайлово — робоче дерево лишається версією ours.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("bun.lock")).unwrap(),
            "from-ours\n"
        );
    }

    #[test]
    fn bun_lock_no_regen_when_same_content() {
        let dir = init_repo();
        write(dir.path(), "bun.lock", "base\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);

        git(dir.path(), &["checkout", "-q", "-b", "src-branch"]);
        write(dir.path(), "bun.lock", "same-final\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "src changes bun.lock"]);

        git(dir.path(), &["checkout", "-q", "-b", "ours-branch", "main"]);
        write(dir.path(), "bun.lock", "same-final\n");
        git(dir.path(), &["add", "-A"]);
        git(
            dir.path(),
            &[
                "commit",
                "-q",
                "-m",
                "ours independently converges to same lock",
            ],
        );

        let outcome = delta_merge(
            DeltaMergeOpts {
                cwd: dir.path(),
                ours: "ours-branch",
                src: "src-branch",
                ours_label: None,
                src_label: None,
            },
            None,
        )
        .unwrap();

        assert!(outcome.is_clean());
        assert!(!outcome.regen_bun_lock);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("bun.lock")).unwrap(),
            "same-final\n"
        );
    }

    #[test]
    fn nested_bun_lock_is_not_root_special_cased() {
        let dir = init_repo();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        write(dir.path(), "sub/bun.lock", "line1\nline2\nline3\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);

        git(dir.path(), &["checkout", "-q", "-b", "src-branch"]);
        write(dir.path(), "sub/bun.lock", "line1\nline2-src\nline3\n");
        git(dir.path(), &["add", "-A"]);
        git(
            dir.path(),
            &["commit", "-q", "-m", "src changes nested bun.lock"],
        );

        git(dir.path(), &["checkout", "-q", "-b", "ours-branch", "main"]);
        write(dir.path(), "sub/bun.lock", "line1\nline2-ours\nline3\n");
        git(dir.path(), &["add", "-A"]);
        git(
            dir.path(),
            &["commit", "-q", "-m", "ours changes nested bun.lock too"],
        );

        let outcome = delta_merge(
            DeltaMergeOpts {
                cwd: dir.path(),
                ours: "ours-branch",
                src: "src-branch",
                ours_label: None,
                src_label: None,
            },
            None,
        )
        .unwrap();

        // Некореневий bun.lock не є спецкейсом: конфліктуючі зміни йдуть через
        // звичайний Tier 1 3-way і лишаються нерозв'язаним конфліктом (без резолвера),
        // а не автоматично беруться з src чи позначаються на regen.
        assert!(!outcome.is_clean());
        assert!(!outcome.regen_bun_lock);
        assert!(outcome.lock_files_taken_from_src.is_empty());
        assert_eq!(outcome.conflict_files, vec!["sub/bun.lock".to_string()]);
    }
}
