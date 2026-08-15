//! `g ch` — Rust-порт `runCh()` з `npm/src/ch.js` (монорепо `7n`).
//!
//! Автоматично визначає воркспейси-підпакети, зачеплені змінами git (`git status
//! --porcelain=v1 -z`), і пише окремий change-файл (`<ws>/.changes/YYMMDD-HHMM.md`)
//! у КОЖЕН зачеплений воркспейс. Цілі — ЛИШЕ підпакети з кореневого `package.json`
//! `workspaces` (їхні `.changes/` веде CHANGELOG); кореневі/поза-воркспейсні файли —
//! `orphans`, пропускаються. `--path <шлях>` звужує до одного воркспейса-власника.
//! Без `--message` — опис кожного воркспейса генерується через injectable
//! [`MessageGenerator`].
//!
//! **Відмінність від JS-оригіналу**: генератор опису — seam (аналогічний
//! [`crate::merge::ConflictResolver`]), а не жорстко вшитий локальний omlx-сервер
//! (Apple Silicon-специфічний MLX-інференс, окрема непортована підсистема). Без
//! `--message` і без підключеного генератора воркспейс просто потрапляє в `failures`
//! замість падіння всього виклику — решта воркспейсів обробляються.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::diff_context::{self, DEFAULT_LIMITS};
use crate::{NError, Result};

pub const DEFAULT_BUMP: &str = "minor";
pub const DEFAULT_SECTION: &str = "Changed";

const USAGE: &str = "Використання: g ch [--message \"<опис>\"] [--bump <major|minor|patch>] [--section <Added|Changed|Fixed|Removed>] [--path <шлях>]";

#[derive(Debug, Default, Clone)]
pub struct ChArgs {
    pub bump: Option<String>,
    pub section: Option<String>,
    pub message: Option<String>,
    pub path: Option<String>,
}

/// Парсить `--bump/--section/--message/--path` з argv (без валідації значень).
pub fn parse_ch_args(argv: &[String]) -> ChArgs {
    let get = |flag: &str| -> Option<String> {
        argv.iter()
            .position(|a| a == flag)
            .and_then(|i| argv.get(i + 1))
            .cloned()
    };
    ChArgs {
        bump: get("--bump"),
        section: get("--section"),
        message: get("--message"),
        path: get("--path"),
    }
}

pub struct ChangeSpec {
    pub bump: String,
    pub section: String,
    pub message: String,
    /// `None` — корінь (`.`).
    pub ws: Option<String>,
}

fn build_change_spec(partial: &ChArgs, message: &str, ws: &str) -> Result<ChangeSpec> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Err(NError::Message(
            "порожній опис (--message обов'язковий)".into(),
        ));
    }
    Ok(ChangeSpec {
        bump: partial
            .bump
            .clone()
            .unwrap_or_else(|| DEFAULT_BUMP.to_string()),
        section: partial
            .section
            .clone()
            .unwrap_or_else(|| DEFAULT_SECTION.to_string()),
        message: trimmed.to_string(),
        ws: if ws == "." {
            None
        } else {
            Some(ws.to_string())
        },
    })
}

/// Розбирає вивід `git status --porcelain=v1 -z` у список шляхів (posix, унікальні,
/// відносно кореня репо). Для rename/copy бере цільовий шлях, пропускає вихідний.
pub fn parse_porcelain_z(raw: &str) -> Vec<String> {
    let fields: Vec<&str> = raw.split('\0').collect();
    let mut paths: Vec<String> = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let rec = fields[i];
        if rec.len() < 4 {
            i += 1;
            continue;
        }
        let xy = &rec[0..2];
        let path = &rec[3..];
        let is_rename_or_copy = xy.contains('R') || xy.contains('C');
        if !path.is_empty() {
            let p = path.replace('\\', "/");
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
        i += if is_rename_or_copy { 2 } else { 1 };
    }
    paths
}

#[derive(serde::Deserialize)]
struct PackageJson {
    workspaces: Option<WorkspacesField>,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum WorkspacesField {
    List(Vec<String>),
    Packages { packages: Vec<String> },
}

/// Читає `workspaces` з кореневого `package.json`, повертає наявні директорії
/// (posix, відносно кореня). Підтримує масив і `{ packages: [...] }`; розгортає
/// лише простий хвостовий glob `<dir>/*`.
pub fn resolve_workspaces(repo_root: &Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(repo_root.join("package.json")) else {
        return Vec::new();
    };
    let Ok(pkg) = serde_json::from_str::<PackageJson>(&content) else {
        return Vec::new();
    };
    let raw = match pkg.workspaces {
        Some(WorkspacesField::List(l)) => l,
        Some(WorkspacesField::Packages { packages }) => packages,
        None => Vec::new(),
    };

    let mut out: Vec<String> = Vec::new();
    for entry in raw {
        if entry.is_empty() {
            continue;
        }
        let norm = entry.replace('\\', "/");
        let norm = norm.trim_end_matches('/');
        if let Some(parent) = norm.strip_suffix("/*") {
            let Ok(entries) = std::fs::read_dir(repo_root.join(parent)) else {
                continue;
            };
            for e in entries.flatten() {
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if is_dir
                    && repo_root
                        .join(parent)
                        .join(e.file_name())
                        .join("package.json")
                        .is_file()
                {
                    let ws = format!("{parent}/{}", e.file_name().to_string_lossy());
                    if !out.contains(&ws) {
                        out.push(ws);
                    }
                }
            }
        } else if repo_root.join(norm).join("package.json").is_file()
            && !out.contains(&norm.to_string())
        {
            out.push(norm.to_string());
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct ChangeGroup {
    pub ws: String,
    pub files: Vec<String>,
}

#[derive(Debug, Default)]
pub struct PlanOutcome {
    pub groups: Vec<ChangeGroup>,
    pub orphans: Vec<String>,
}

fn by_len_desc(workspaces: &[String]) -> Vec<&String> {
    let mut v: Vec<&String> = workspaces.iter().collect();
    v.sort_by_key(|w| std::cmp::Reverse(w.len()));
    v
}

/// Планує цілі за змінами git. Валідні цілі — ЛИШЕ підпакети-воркспейси; корінь за
/// наявності підпакетів НЕ є ціллю (кореневі/`docs/`-файли йдуть у `orphans`). Якщо
/// підпакетів немає (однопакетне репо) — корінь `.` сам є пакетом.
pub fn plan_changes(changed: &[String], workspaces: &[String]) -> PlanOutcome {
    if workspaces.is_empty() {
        return PlanOutcome {
            groups: if changed.is_empty() {
                Vec::new()
            } else {
                vec![ChangeGroup {
                    ws: ".".to_string(),
                    files: changed.to_vec(),
                }]
            },
            orphans: Vec::new(),
        };
    }
    let by_len = by_len_desc(workspaces);
    let mut map: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut orphans = Vec::new();
    for p in changed {
        let owner = by_len
            .iter()
            .find(|w| p == **w || p.starts_with(&format!("{w}/")));
        match owner {
            Some(w) => map.entry((*w).clone()).or_default().push(p.clone()),
            None => orphans.push(p.clone()),
        }
    }
    let groups = map
        .into_iter()
        .map(|(ws, files)| ChangeGroup { ws, files })
        .collect();
    PlanOutcome { groups, orphans }
}

/// Резолвить, який воркспейс володіє шляхом з `--path` (та сама «найдовший префікс
/// виграє» логіка, що й [`plan_changes`]). Якщо жоден воркспейс не покриває шлях —
/// повертає нормалізований шлях як є.
pub fn resolve_workspace_for_path(path: &str, workspaces: &[String]) -> String {
    let norm = path.replace('\\', "/");
    let norm = norm.trim_end_matches('/');
    by_len_desc(workspaces)
        .into_iter()
        .find(|w| norm == w.as_str() || norm.starts_with(&format!("{w}/")))
        .cloned()
        .unwrap_or_else(|| norm.to_string())
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

fn git_text(cwd: &Path, args: &[&str]) -> String {
    run_git(cwd, args)
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

fn git_text_owned(cwd: &Path, args: &[String]) -> String {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    git_text(cwd, &refs)
}

/// Diff-контекст воркспейса для генерації опису: name-status + повний diff проти
/// HEAD (staged+unstaged) для його файлів + вміст untracked-файлів. Шумні шляхи
/// виключено (`diff_context::NOISE_GLOBS`), контекст обрізано трирівнево.
pub fn workspace_diff_context(repo_root: &Path, ws: &str, files: &[String]) -> String {
    let pathspec: Vec<String> = if !files.is_empty() {
        files.to_vec()
    } else {
        vec![if ws == "." {
            ".".to_string()
        } else {
            ws.to_string()
        }]
    };
    let exclude = diff_context::exclude_pathspecs(diff_context::NOISE_GLOBS);
    let build_args = |base: &[&str]| -> Vec<String> {
        base.iter()
            .map(|s| s.to_string())
            .chain(pathspec.iter().cloned())
            .chain(exclude.iter().cloned())
            .collect()
    };
    let names_args = build_args(&[
        "-c",
        "core.quotepath=false",
        "diff",
        "--name-status",
        "HEAD",
        "--",
    ]);
    let names = git_text_owned(repo_root, &names_args).trim().to_string();

    let diff_args = build_args(&["-c", "core.quotepath=false", "diff", "HEAD", "--"]);
    let diff = git_text_owned(repo_root, &diff_args);

    let untracked_args = build_args(&[
        "-c",
        "core.quotepath=false",
        "ls-files",
        "--others",
        "--exclude-standard",
        "--",
    ]);
    let untracked: Vec<String> = git_text_owned(repo_root, &untracked_args)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();

    let mut parts = vec![format!(
        "Змінені файли:\n{}",
        if names.is_empty() {
            "(нема відстежуваних змін)"
        } else {
            &names
        }
    )];
    if !diff.trim().is_empty() {
        parts.push(format!("Diff:\n{diff}"));
    }
    for f in &untracked {
        let content = std::fs::read_to_string(repo_root.join(f))
            .map(|c| diff_context::clamp_bytes(&c, DEFAULT_LIMITS.max_file_bytes))
            .unwrap_or_default();
        parts.push(format!("Новий файл {f}:\n{content}"));
    }
    diff_context::truncate_context(&parts.join("\n\n"), &DEFAULT_LIMITS)
}

pub struct ChatMessage {
    pub role: &'static str,
    pub content: String,
}

/// Chat-повідомлення для генерації одно-рядкового опису зміни воркспейса.
pub fn build_gen_messages(ws: &str, context: &str) -> Vec<ChatMessage> {
    let ws_label = if ws == "." { "корінь" } else { ws };
    let sys = format!(
        "Ти пишеш ОДИН рядок опису зміни (changelog entry) українською для воркспейса «{ws_label}».\n\
Дано git diff цього воркспейса. Опиши СУТЬ зміни одним коротким рядком.\n\
Правила:\n\
- Мова — українська; технічні ідентифікатори, шляхи, команди та API-назви лишай англійською.\n\
- Один рядок, ≤ 72 символи, без крапки в кінці.\n\
- БЕЗ emoji, БЕЗ префіксів типу feat/fix/refactor, БЕЗ лапок, БЕЗ code fence, БЕЗ пояснень.\n\
- Виведи РІВНО сам опис, нічого більше."
    );
    vec![
        ChatMessage {
            role: "system",
            content: sys,
        },
        ChatMessage {
            role: "user",
            content: context.to_string(),
        },
    ]
}

/// Чистить вихід моделі до одного рядка: перший непорожній рядок без code fence,
/// провідного `- `/`* `, обрамлювальних лапок/беків і крапки в кінці.
pub fn clean_generated_message(output: &str) -> String {
    let Some(line) = output
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("```"))
    else {
        return String::new();
    };

    let mut s = line;
    if let Some(rest) = s.strip_prefix('-').or_else(|| s.strip_prefix('*')) {
        let trimmed = rest.trim_start();
        if trimmed.len() != rest.len() {
            s = trimmed;
        }
    }
    let s = s.trim_end_matches('.');
    let s = s.trim_matches(|c| c == '"' || c == '\'' || c == '`');
    s.trim().to_string()
}

/// Tier для генерації опису воркспейса без `--message` — seam для ACP/`llm-lib`
/// (JS-оригінал використовував локальний omlx, окрема непортована підсистема).
pub trait MessageGenerator {
    fn generate(&self, ws: &str, files: &[String], repo_root: &Path) -> std::io::Result<String>;
}

#[derive(Debug)]
pub struct GitContext {
    pub repo_root: PathBuf,
    pub workspaces: Vec<String>,
    pub changed: Vec<String>,
}

fn git_context(cwd: &Path) -> Result<GitContext> {
    let repo_root = PathBuf::from(git_ok(cwd, &["rev-parse", "--show-toplevel"])?);
    let raw = run_git(&repo_root, &["status", "--porcelain=v1", "-z"])?;
    let changed = parse_porcelain_z(&String::from_utf8_lossy(&raw.stdout));
    let workspaces = resolve_workspaces(&repo_root);
    Ok(GitContext {
        repo_root,
        workspaces,
        changed,
    })
}

fn write_change_file(spec: &ChangeSpec, repo_root: &Path) -> Result<PathBuf> {
    let dir = match &spec.ws {
        Some(ws) => repo_root.join(ws).join(".changes"),
        None => repo_root.join(".changes"),
    };
    std::fs::create_dir_all(&dir)?;

    let base = chrono::Local::now().format("%y%m%d-%H%M").to_string();
    let mut filename = format!("{base}.md");
    let mut counter = 2;
    while dir.join(&filename).exists() {
        filename = format!("{base}-{counter}.md");
        counter += 1;
    }

    let content = format!(
        "---\nbump: {}\nsection: {}\n---\n{}\n",
        spec.bump, spec.section, spec.message
    );
    let path = dir.join(&filename);
    std::fs::write(&path, content)?;
    Ok(path)
}

fn ws_label(ws: &str) -> String {
    if ws == "." {
        "<корінь>".to_string()
    } else {
        ws.to_string()
    }
}

#[derive(Debug)]
pub struct WrittenChange {
    pub ws: String,
    pub file: PathBuf,
}

#[derive(Debug)]
pub struct ChFailure {
    pub ws: String,
    pub reason: String,
}

#[derive(Debug)]
pub enum ChReport {
    /// Немає змін у воркспейсах, що ведуть CHANGELOG — успіх, нічого не створено.
    Nothing { orphans: Vec<String> },
    Completed {
        orphans: Vec<String>,
        skipped_by_path: Vec<String>,
        written: Vec<WrittenChange>,
        failures: Vec<ChFailure>,
    },
}

impl ChReport {
    pub fn is_success(&self) -> bool {
        match self {
            ChReport::Nothing { .. } => true,
            ChReport::Completed { failures, .. } => failures.is_empty(),
        }
    }
}

/// `g ch [--message "<опис>"] [--bump ...] [--section ...] [--path <шлях>]`.
pub fn run(
    cwd: &Path,
    argv: &[String],
    generator: Option<&dyn MessageGenerator>,
) -> Result<ChReport> {
    let partial = parse_ch_args(argv);
    if let Some(m) = &partial.message {
        if m.trim().is_empty() {
            return Err(NError::Message(format!("Порожній --message.\n{USAGE}")));
        }
    }

    let ctx = git_context(cwd)?;
    let plan = plan_changes(&ctx.changed, &ctx.workspaces);
    let mut groups = plan.groups;
    let orphans = plan.orphans;
    let mut skipped_by_path = Vec::new();

    if let Some(path) = &partial.path {
        let target_ws = resolve_workspace_for_path(path, &ctx.workspaces);
        skipped_by_path = groups
            .iter()
            .filter(|g| g.ws != target_ws)
            .map(|g| ws_label(&g.ws))
            .collect();
        groups.retain(|g| g.ws == target_ws);
        if groups.is_empty() {
            return Err(NError::Message(format!(
                "Немає змін у воркспейсі за --path {path} ({}) — нічого не створено.",
                ws_label(&target_ws)
            )));
        }
    }

    if groups.is_empty() {
        return Ok(ChReport::Nothing { orphans });
    }

    let mut written = Vec::new();
    let mut failures = Vec::new();
    for group in &groups {
        let message = match &partial.message {
            Some(m) => m.clone(),
            None => {
                let Some(gen) = generator else {
                    failures.push(ChFailure {
                        ws: group.ws.clone(),
                        reason:
                            "немає --message і не підключено генератор опису (MessageGenerator)"
                                .into(),
                    });
                    continue;
                };
                match gen.generate(&group.ws, &group.files, &ctx.repo_root) {
                    Ok(m) if !m.trim().is_empty() => m,
                    Ok(_) => {
                        failures.push(ChFailure {
                            ws: group.ws.clone(),
                            reason: "порожній опис від генератора".into(),
                        });
                        continue;
                    }
                    Err(e) => {
                        failures.push(ChFailure {
                            ws: group.ws.clone(),
                            reason: e.to_string(),
                        });
                        continue;
                    }
                }
            }
        };

        let spec = build_change_spec(&partial, &message, &group.ws)?;
        match write_change_file(&spec, &ctx.repo_root) {
            Ok(file) => written.push(WrittenChange {
                ws: group.ws.clone(),
                file,
            }),
            Err(e) => failures.push(ChFailure {
                ws: group.ws.clone(),
                reason: e.to_string(),
            }),
        }
    }

    Ok(ChReport::Completed {
        orphans,
        skipped_by_path,
        written,
        failures,
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
        dir
    }

    // ── parse_porcelain_z ──────────────────────────────────────────────────

    #[test]
    fn parse_porcelain_z_basic() {
        let raw = " M src/a.js\0?? new.txt\0";
        let paths = parse_porcelain_z(raw);
        assert_eq!(paths, vec!["src/a.js".to_string(), "new.txt".to_string()]);
    }

    #[test]
    fn parse_porcelain_z_rename_skips_source() {
        // "R  new.js\0old.js\0" — рядок з наступним полем (вихідний шлях), який треба пропустити.
        let raw = "R  new.js\0old.js\0";
        let paths = parse_porcelain_z(raw);
        assert_eq!(paths, vec!["new.js".to_string()]);
    }

    #[test]
    fn parse_porcelain_z_dedupes() {
        let raw = " M a.txt\0 M a.txt\0";
        assert_eq!(parse_porcelain_z(raw), vec!["a.txt".to_string()]);
    }

    // ── plan_changes / resolve_workspace_for_path ─────────────────────────

    #[test]
    fn plan_changes_single_package_repo() {
        let changed = vec!["a.txt".to_string(), "src/b.txt".to_string()];
        let plan = plan_changes(&changed, &[]);
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].ws, ".");
        assert_eq!(plan.groups[0].files, changed);
        assert!(plan.orphans.is_empty());
    }

    #[test]
    fn plan_changes_groups_by_workspace_longest_prefix_wins() {
        let workspaces = vec!["pkg".to_string(), "pkg/nested".to_string()];
        let changed = vec![
            "pkg/a.txt".to_string(),
            "pkg/nested/b.txt".to_string(),
            "README.md".to_string(),
        ];
        let plan = plan_changes(&changed, &workspaces);
        assert_eq!(plan.orphans, vec!["README.md".to_string()]);
        let pkg_group = plan.groups.iter().find(|g| g.ws == "pkg").unwrap();
        assert_eq!(pkg_group.files, vec!["pkg/a.txt".to_string()]);
        let nested_group = plan.groups.iter().find(|g| g.ws == "pkg/nested").unwrap();
        assert_eq!(nested_group.files, vec!["pkg/nested/b.txt".to_string()]);
    }

    #[test]
    fn resolve_workspace_for_path_longest_prefix() {
        let workspaces = vec!["pkg".to_string(), "pkg/nested".to_string()];
        assert_eq!(
            resolve_workspace_for_path("pkg/nested/file.txt", &workspaces),
            "pkg/nested"
        );
        assert_eq!(
            resolve_workspace_for_path("pkg/file.txt", &workspaces),
            "pkg"
        );
        assert_eq!(
            resolve_workspace_for_path("unrelated/file.txt", &workspaces),
            "unrelated/file.txt"
        );
    }

    // ── resolve_workspaces (package.json) ─────────────────────────────────

    #[test]
    fn resolve_workspaces_expands_glob() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "package.json",
            r#"{"workspaces":["packages/*"]}"#,
        );
        write(dir.path(), "packages/foo/package.json", "{}");
        write(dir.path(), "packages/bar/package.json", "{}");
        write(dir.path(), "packages/not-a-pkg/README.md", "x");

        let mut ws = resolve_workspaces(dir.path());
        ws.sort();
        assert_eq!(
            ws,
            vec!["packages/bar".to_string(), "packages/foo".to_string()]
        );
    }

    #[test]
    fn resolve_workspaces_packages_object_form() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "package.json",
            r#"{"workspaces":{"packages":["pkg"]}}"#,
        );
        write(dir.path(), "pkg/package.json", "{}");
        assert_eq!(resolve_workspaces(dir.path()), vec!["pkg".to_string()]);
    }

    #[test]
    fn resolve_workspaces_no_package_json() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_workspaces(dir.path()).is_empty());
    }

    // ── clean_generated_message ─────────────────────────────────────────

    #[test]
    fn clean_generated_message_strips_fence_and_marker() {
        let out = "```\n- Виправлено помилку типізації.\n```";
        assert_eq!(clean_generated_message(out), "Виправлено помилку типізації");
    }

    #[test]
    fn clean_generated_message_strips_quotes() {
        assert_eq!(clean_generated_message("\"Опис зміни\""), "Опис зміни");
    }

    #[test]
    fn clean_generated_message_empty_input() {
        assert_eq!(clean_generated_message(""), "");
        assert_eq!(clean_generated_message("```\n```"), "");
    }

    // ── run() end-to-end (single-package repo, --message provided) ───────

    #[test]
    fn run_writes_change_file_with_explicit_message() {
        let repo = init_repo();
        write(repo.path(), "package.json", r#"{"name":"root"}"#);
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "init"]);
        write(repo.path(), "src/a.txt", "changed\n");

        let argv = vec!["--message".to_string(), "Опис зміни".to_string()];
        let report = run(repo.path(), &argv, None).unwrap();
        match report {
            ChReport::Completed {
                written, failures, ..
            } => {
                assert!(failures.is_empty());
                assert_eq!(written.len(), 1);
                let content = std::fs::read_to_string(&written[0].file).unwrap();
                assert!(content.contains("bump: minor"));
                assert!(content.contains("section: Changed"));
                assert!(content.contains("Опис зміни"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn run_nothing_when_no_changes() {
        let repo = init_repo();
        write(repo.path(), "package.json", r#"{"name":"root"}"#);
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "init"]);

        let report = run(repo.path(), &[], None).unwrap();
        assert!(matches!(report, ChReport::Nothing { .. }));
    }

    #[test]
    fn run_without_message_and_generator_reports_failure() {
        let repo = init_repo();
        write(repo.path(), "package.json", r#"{"name":"root"}"#);
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "init"]);
        write(repo.path(), "src/a.txt", "changed\n");

        let report = run(repo.path(), &[], None).unwrap();
        match report {
            ChReport::Completed {
                failures, written, ..
            } => {
                assert_eq!(failures.len(), 1);
                assert!(written.is_empty());
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn run_rejects_empty_message_flag() {
        let repo = init_repo();
        let argv = vec!["--message".to_string(), "   ".to_string()];
        let err = run(repo.path(), &argv, None).unwrap_err();
        assert!(matches!(err, NError::Message(_)));
    }

    #[test]
    fn run_path_filters_to_single_workspace() {
        let repo = init_repo();
        write(
            repo.path(),
            "package.json",
            r#"{"workspaces":["packages/*"]}"#,
        );
        write(repo.path(), "packages/foo/package.json", "{}");
        write(repo.path(), "packages/bar/package.json", "{}");
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "init"]);
        write(repo.path(), "packages/foo/a.txt", "x\n");
        write(repo.path(), "packages/bar/b.txt", "y\n");

        let argv = vec![
            "--message".to_string(),
            "опис".to_string(),
            "--path".to_string(),
            "packages/foo".to_string(),
        ];
        let report = run(repo.path(), &argv, None).unwrap();
        match report {
            ChReport::Completed {
                written,
                skipped_by_path,
                ..
            } => {
                assert_eq!(written.len(), 1);
                assert_eq!(written[0].ws, "packages/foo");
                assert_eq!(skipped_by_path, vec!["packages/bar".to_string()]);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn run_path_with_no_matching_changes_errors() {
        let repo = init_repo();
        write(
            repo.path(),
            "package.json",
            r#"{"workspaces":["packages/*"]}"#,
        );
        write(repo.path(), "packages/foo/package.json", "{}");
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "init"]);
        write(repo.path(), "packages/foo/a.txt", "x\n");

        let argv = vec![
            "--message".to_string(),
            "опис".to_string(),
            "--path".to_string(),
            "packages/other".to_string(),
        ];
        let err = run(repo.path(), &argv, None).unwrap_err();
        assert!(matches!(err, NError::Message(_)));
    }
}
