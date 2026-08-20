//! `g push [branch]` — Rust-порт `push()` з `npm/src/push.js` (531 рядок, найбільший
//! command-файл JS-оригіналу, монорепо `7n`).
//!
//! Бере ВСІ локальні коміти (`origin/<branch>..HEAD`) + усі зміни робочого дерева
//! (`git add -A` — staged/unstaged/untracked), сквошить в ОДИН коміт на вершині
//! `origin/<branch>` (`git reset --soft <base>` — тож push до наявної гілки завжди
//! fast-forward), формує commit-меседж і пушить одним комітом. За дивергенції
//! (origin має коміти, яких немає локально) спершу автоматично підтягує дельту тим
//! самим ядром, що й `pull` ([`crate::merge::delta_merge`]).
//!
//! **Пріоритет меседжу**: якщо серед застейджених файлів є change-файли
//! (`.changes/*.md`) — меседж збирається ДЕТЕРМІНОВАНО, без LLM
//! ([`build_message_from_changes`]): frontmatter `section` → emoji/type, `scope` —
//! воркспейс із найбільшою кількістю change-файлів, `summary` — тіло найвагомішого
//! (за `bump`) change-файлу. Лише за відсутності change-файлів (або
//! `N7COMMIT_FORCE_LLM=1`) меседж генерує LLM через injectable
//! [`CommitMessageGenerator`].
//!
//! **Відмінності від zsh-оригіналу**:
//! - LLM-агентний ланцюжок (`pi -p` → `claude -p` → `cursor-agent -p`, спавн CLI) —
//!   замінено на [`CommitMessageGenerator`]-seam (ACP/`llm-lib`, ADR 20260814-195911
//!   ідея #46); без підключеного генератора й без change-файлів `run` повертає
//!   помилку замість спроби всіх трьох CLI по черзі.
//! - `N7COMMIT_DEBUG`-таймлайн (посекундна діагностика етапів у stderr) не
//!   портовано — суто діагностичний UX, не впливає на функціональну коректність;
//!   `tracing`-інструментація в CLI-шарі може дати те саме пізніше.
//! - Env-кнопки (`N7COMMIT_*`) читаються напряму в цьому модулі (як
//!   `N7MERGE_NO_MERGIRAF` у `merge.rs`) — та сама відповідальність, що й в
//!   оригіналі, не винесена окремо в CLI-шар.

use std::path::Path;
use std::process::{Command, Output};

use crate::diff_context::{self, DEFAULT_LIMITS};
use crate::merge::{ConflictResolver, DeltaMergeOpts, DeltaMergeOutcome, delta_merge};
use crate::{NError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageSource {
    /// Зібрано детерміновано зі change-файлів, без LLM.
    Changes,
    /// Згенеровано через [`CommitMessageGenerator`].
    Llm,
}

#[derive(Debug)]
pub enum PushOutcome {
    /// Немає змін відносно `base` — пушити нічого.
    NothingToPush { base: String },
    Done {
        branch: String,
        base: String,
        subject: String,
        /// Name-status рядки, без `docs/adr/**` (ті — лише кількістю в `adr_count`).
        file_names: Vec<String>,
        adr_count: usize,
        /// `true`, якщо гілки на origin ще не було (`git push -u`).
        pushed_new_branch: bool,
        auto_pulled: Option<Box<DeltaMergeOutcome>>,
        message_source: MessageSource,
    },
}

/// Tier для генерації commit-меседжу через LLM — викликається лише коли немає
/// придатних change-файлів (або `N7COMMIT_FORCE_LLM=1`). Заміна ACP/`llm-lib` для
/// CLI-ланцюжка `pi`/`claude`/`cursor-agent` з JS-оригіналу.
pub trait CommitMessageGenerator {
    fn generate(&self, context: &str) -> std::io::Result<String>;
}

/// Rust-порт промпту `_n7push_gen_message` з JS-оригіналу — Gitmoji + Monorepo
/// (Conventional Commits зі scope), українською. Публічна — reuse у
/// [`CommitMessageGenerator`]-реалізаціях (напр. `acp_agents::AcpAgentAdapter`).
pub fn commit_message_prompt(context: &str) -> String {
    format!(
        "Згенеруй Git commit-меседж українською у стилі Gitmoji + Monorepo (Conventional Commits зі scope).\n\
\n\
Формат:\n\
  <emoji> <type>(<scope>): <короткий підсумок>\n\
\n\
  - <пункт тіла: що саме змінено і навіщо>\n\
  - <ще пункт за потреби, 1-5 загалом>\n\
\n\
Правила:\n\
- Мова — українська; технічні ідентифікатори, шляхи, команди та API-назви лишай англійською.\n\
- <emoji> — доречний Gitmoji (✨ нова фіча, 🐛 фікс, ♻️ рефактор, 📝 докси, ✅ тести, 🔧 конфіг, ⬆️ оновлення залежностей, 🚀 деплой/реліз тощо).\n\
- <type> — feat|fix|refactor|docs|test|chore|build за змістом змін.\n\
- <scope> — назва workspace/каталогу, де основні зміни. Якщо їх кілька — обери головний.\n\
- Subject (перший рядок) ≤ 72 символи, без крапки в кінці.\n\
- Якщо в контексті є секція «Change-файли» — будуй меседж НАСАМПЕРЕД на їхньому описі (вони вже фіксують суть і секцію); diff там відсутній і не потрібен. Якщо change-файлів немає — визначай суть із diff.\n\
- Виведи ЛИШЕ сам меседж: subject, далі порожній рядок, далі тіло. БЕЗ преамбул, БЕЗ code fence, БЕЗ лапок навколо.\n\
\n\
Контекст змін:\n\
{context}"
    )
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

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).map(|v| v == "1").unwrap_or(false)
}

/// `g push [branch]`.
pub fn run(
    cwd: &Path,
    branch: Option<&str>,
    conflict_resolver: Option<&dyn ConflictResolver>,
    message_generator: Option<&dyn CommitMessageGenerator>,
) -> Result<PushOutcome> {
    if !crate::is_inside_work_tree(cwd) {
        return Err(NError::Message("Ви не в Git репозиторії.".into()));
    }

    let branch = match branch {
        Some(b) if !b.is_empty() => b.to_string(),
        _ => {
            let Some(current) = crate::gix_util::current_branch(cwd) else {
                return Err(NError::Message(
                    "Не вдалося визначити гілку (detached HEAD?). Вкажи явно: g push <branch>"
                        .into(),
                ));
            };
            current
        }
    };

    // Best-effort — на відміну від pull, push не зупиняється, якщо fetch не вдався
    // (JS-оригінал теж ігнорує exit code тут).
    let _ = run_git(cwd, &["fetch", "origin", &branch]);

    let remote_ref = format!("origin/{branch}");
    let remote_exists = crate::gix_util::rev_parse(cwd, &remote_ref).is_some();

    let mut auto_pulled = None;
    let (base, base_is_remote_branch) = if remote_exists {
        let is_ancestor = crate::gix_util::is_ancestor(cwd, &remote_ref, "HEAD");
        if !is_ancestor {
            let merge = delta_merge(
                DeltaMergeOpts {
                    cwd,
                    ours: "HEAD",
                    src: &remote_ref,
                    ours_label: None,
                    src_label: None,
                },
                conflict_resolver,
            )?;
            if !merge.is_clean() {
                return Err(NError::Message(
                    "Автопідтягування лишило конфлікти — розв'яжи вручну (git diff), потім повтори g push."
                        .into(),
                ));
            }
            auto_pulled = Some(Box::new(merge));
        }
        (remote_ref.clone(), true)
    } else {
        let default_ref = crate::gix_util::default_remote_branch(cwd, "origin");
        let mut base = None;
        if let Some(dref) = &default_ref
            && !dref.is_empty()
            && crate::gix_util::rev_parse(cwd, dref).is_some()
        {
            base = crate::gix_util::merge_base(cwd, "HEAD", dref).filter(|s| !s.is_empty());
        }
        let base = match base {
            Some(b) => b,
            None => crate::gix_util::root_commit(cwd)
                .ok_or_else(|| NError::Message("Не вдалося визначити кореневий коміт.".into()))?,
        };
        (base, false)
    };

    run_git(cwd, &["add", "-A"])?;

    let no_changes = !crate::gix_util::index_differs_from_tree(cwd, &base);
    if no_changes {
        return Ok(PushOutcome::NothingToPush { base });
    }

    run_git(cwd, &["reset", "--soft", &base])?;

    let staged_names = git_ok(cwd, &["diff", "--cached", "--name-only", &base, "--"])?;
    let changes_list: Vec<String> = staged_names
        .lines()
        .filter(|l| l.contains(".changes/"))
        .map(str::to_string)
        .collect();

    let force_llm = env_flag("N7COMMIT_FORCE_LLM");

    let (message, message_source) = if !changes_list.is_empty() && !force_llm {
        match build_message_from_changes(cwd, &changes_list) {
            Some(m) => (m, MessageSource::Changes),
            None => generate_llm_message(cwd, &base, &changes_list, message_generator)?,
        }
    } else {
        generate_llm_message(cwd, &base, &changes_list, message_generator)?
    };

    let subject = message
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_string();
    if subject.is_empty() {
        return Err(NError::Message(
            "Порожній commit-меседж — нічого не закомічено.".into(),
        ));
    }

    let names = git_ok(cwd, &["diff", "--cached", "--name-status", &base, "--"])?;
    let (file_names, adr_count) = split_adr(&names);

    let mut msg_file = tempfile::NamedTempFile::new()?;
    std::io::Write::write_all(&mut msg_file, message.as_bytes())?;
    let commit = Command::new("git")
        .args(["commit", "--no-verify", "-F"])
        .arg(msg_file.path())
        .current_dir(cwd)
        .output()
        .map_err(NError::Io)?;
    if !commit.status.success() {
        return Err(NError::Message("git commit не вдався.".into()));
    }

    let pushed_new_branch = !base_is_remote_branch;
    let push_out = if base_is_remote_branch {
        run_git(cwd, &["push", "origin", &branch])?
    } else {
        run_git(cwd, &["push", "-u", "origin", &branch])?
    };
    if !push_out.status.success() {
        let hint = if base_is_remote_branch {
            format!(
                "git push не вдався (можливо, origin/{branch} знову оновився — зроби g push ще раз)."
            )
        } else {
            "git push не вдався.".to_string()
        };
        return Err(NError::Message(hint));
    }

    Ok(PushOutcome::Done {
        branch,
        base,
        subject,
        file_names,
        adr_count,
        pushed_new_branch,
        auto_pulled,
        message_source,
    })
}

fn generate_llm_message(
    cwd: &Path,
    base: &str,
    changes_list: &[String],
    generator: Option<&dyn CommitMessageGenerator>,
) -> Result<(String, MessageSource)> {
    let context = build_diff_context(cwd, base, changes_list);
    let Some(generator) = generator else {
        return Err(NError::Message(
            "Немає change-файлів і не підключено генератор commit-меседжу (CommitMessageGenerator) — ACP/llm-lib ще не підключено.".into(),
        ));
    };
    let message = generator
        .generate(&context)
        .map_err(|e| NError::Message(format!("Не вдалося згенерувати commit-меседж: {e}")))?;
    Ok((message, MessageSource::Llm))
}

/// `true`, якщо `docs/` зустрічається як сегмент шляху (на початку рядка чи
/// одразу після пробілу/табу/`/`) — еквівалент JS-regex `(^|[[:space:]/])docs/`.
fn contains_docs_segment(line: &str) -> bool {
    for (pos, _) in line.match_indices("docs/") {
        if pos == 0 {
            return true;
        }
        match line.as_bytes().get(pos - 1) {
            Some(b' ') | Some(b'\t') | Some(b'/') => return true,
            _ => {}
        }
    }
    false
}

/// Розділяє `git diff --name-status` на не-ADR рядки і кількість `docs/adr/**`.
fn split_adr(names: &str) -> (Vec<String>, usize) {
    let mut kept = Vec::new();
    let mut adr = 0;
    for line in names.lines() {
        if line.is_empty() {
            continue;
        }
        if line.contains("docs/adr/") {
            adr += 1;
        } else {
            kept.push(line.to_string());
        }
    }
    (kept, adr)
}

/// Diff-контекст для LLM-генератора: перелік файлів (docs/ згорнуто до кількості) +
/// або вміст change-файлів (якщо є — першоджерело наміру), або per-file обрізаний
/// diff (рівномірний байт-бюджет на файл, щоб один великий файл не з'їв стелю).
fn build_diff_context(cwd: &Path, base: &str, changes_list: &[String]) -> String {
    let names_full = git_text(cwd, &["diff", "--cached", "--name-status", base, "--"]);
    let docs_n = names_full
        .lines()
        .filter(|l| contains_docs_segment(l))
        .count();

    let mut out = String::from("# Змінені файли (scope; docs/ згорнуто до кількості):\n");
    for line in names_full.lines() {
        if !contains_docs_segment(line) {
            out.push_str(line);
            out.push('\n');
        }
    }
    if docs_n > 0 {
        out.push_str(&format!(
            "# (+ загально змінено {docs_n} файл(ів) у docs/ директоріях)\n"
        ));
    }
    out.push('\n');

    if !changes_list.is_empty() {
        out.push_str("# Change-файли (.changes/) — ПЕРШОДЖЕРЕЛО наміру коміту; будуй меседж насамперед на них\n");
        out.push_str("# (frontmatter section ≈ type/emoji: Added→feat/✨, Fixed→fix/🐛, Changed→refactor/♻️, Removed→🔥):\n");
        for cf in changes_list {
            out.push_str(&format!("\n## {cf}\n"));
            out.push_str(&change_file_content(cwd, cf));
        }
        return out;
    }

    out.push_str(
        "# Change-файлів немає — визнач суть із diff (вміст шумних шляхів виключено, обрізано):\n",
    );

    let max_lines = env_usize("N7COMMIT_MAX_DIFF_LINES", DEFAULT_LIMITS.max_lines);
    let max_line_len = env_usize("N7COMMIT_MAX_LINE", DEFAULT_LIMITS.max_line_len);
    let max_bytes = env_usize("N7COMMIT_MAX_DIFF_BYTES", DEFAULT_LIMITS.max_bytes);
    let max_file_bytes = env_usize("N7COMMIT_MAX_FILE_BYTES", DEFAULT_LIMITS.max_file_bytes);

    let mut noise: Vec<String> = Vec::new();
    if !env_flag("N7COMMIT_NO_DEFAULT_EXCLUDE") {
        noise.extend(diff_context::exclude_pathspecs(diff_context::NOISE_GLOBS));
    }
    if let Ok(extra) = std::env::var("N7COMMIT_EXCLUDE") {
        for g in extra.split_whitespace() {
            noise.push(format!(":(exclude){g}"));
        }
    }

    let mut name_args: Vec<String> = vec![
        "diff".into(),
        "--cached".into(),
        "--name-only".into(),
        base.into(),
        "--".into(),
        ".".into(),
    ];
    name_args.extend(noise.iter().cloned());
    let refs: Vec<&str> = name_args.iter().map(String::as_str).collect();
    let changed: Vec<String> = git_text(cwd, &refs)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();

    let mut acc = String::new();
    let mut trunc_n = 0usize;
    let mut omit_n = 0usize;
    for (i, f) in changed.iter().enumerate() {
        let used = acc.len();
        if used >= max_bytes {
            omit_n = changed.len() - i;
            break;
        }
        let remain = max_bytes - used;
        let cap = max_file_bytes.min(remain);
        let raw = git_text(cwd, &["diff", "--cached", base, "--", f]);
        let limited = limit_lines_and_columns(&raw, max_lines, max_line_len);
        let capped = diff_context::clamp_bytes(&limited, cap);
        let was_truncated = capped.len() < limited.len();
        acc.push_str(&capped);
        acc.push('\n');
        if was_truncated {
            acc.push_str(&format!("# … (вміст {f} обрізано до ~{cap} б)\n"));
            trunc_n += 1;
        }
    }
    out.push_str(&acc);
    if trunc_n > 0 {
        out.push_str(&format!(
            "\n# … вміст {trunc_n} файл(ів) обрізано (per-file ~{max_file_bytes} б, env N7COMMIT_MAX_FILE_BYTES; рядки {max_lines}/N7COMMIT_MAX_DIFF_LINES, довжина {max_line_len} симв./N7COMMIT_MAX_LINE).\n"
        ));
    }
    if omit_n > 0 {
        out.push_str(&format!(
            "\n# … {omit_n} файл(ів) пропущено — вичерпано глобальну стелю ~{max_bytes} б (env N7COMMIT_MAX_DIFF_BYTES).\n"
        ));
    }
    out
}

fn limit_lines_and_columns(text: &str, max_lines: usize, max_line_len: usize) -> String {
    let lines: Vec<String> = text
        .lines()
        .take(max_lines)
        .map(|l| {
            if l.chars().count() > max_line_len {
                l.chars().take(max_line_len).collect()
            } else {
                l.to_string()
            }
        })
        .collect();
    lines.join("\n")
}

fn change_file_content(cwd: &Path, cf: &str) -> String {
    let staged = git_text(cwd, &["show", &format!(":{cf}")]);
    if !staged.is_empty() {
        return staged;
    }
    std::fs::read_to_string(cwd.join(cf)).unwrap_or_default()
}

fn split_frontmatter(content: &str) -> (String, String) {
    let lines: Vec<&str> = content.lines().collect();
    let dash_positions: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim_end() == "---")
        .map(|(i, _)| i)
        .collect();
    if dash_positions.len() < 2 {
        return (String::new(), content.to_string());
    }
    let fm = lines[dash_positions[0] + 1..dash_positions[1]].join("\n");
    let mut body_lines = &lines[dash_positions[1] + 1..];
    while let Some(first) = body_lines.first() {
        if first.trim().is_empty() {
            body_lines = &body_lines[1..];
        } else {
            break;
        }
    }
    (fm, body_lines.join("\n"))
}

fn extract_field(frontmatter: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    frontmatter
        .lines()
        .find_map(|l| l.strip_prefix(&prefix).map(|v| v.trim().to_string()))
}

fn emoji_for(section: &str) -> &'static str {
    match section {
        "Added" => "✨",
        "Changed" => "♻️",
        "Fixed" => "🐛",
        "Removed" => "🔥",
        _ => "📝",
    }
}

fn type_for(section: &str) -> &'static str {
    match section {
        "Added" => "feat",
        "Changed" => "refactor",
        "Fixed" => "fix",
        "Removed" => "chore",
        _ => "chore",
    }
}

fn bump_rank(bump: &str) -> i32 {
    match bump {
        "major" => 3,
        "minor" => 2,
        "patch" => 1,
        _ => 0,
    }
}

fn section_rank(section: &str) -> i32 {
    match section {
        "Added" => 4,
        "Fixed" => 3,
        "Changed" => 2,
        "Removed" => 1,
        _ => 0,
    }
}

/// Rust-порт `_n7push_build_message_from_changes`: детерміновано (без LLM) збирає
/// commit-меседж зі застейджених change-файлів. `None`, якщо жоден придатний
/// change-файл не знайдено (тоді викликач фолбечить на LLM).
fn build_message_from_changes(cwd: &Path, changes_list: &[String]) -> Option<String> {
    let mut bullets: Vec<String> = Vec::new();
    let mut ws_count: Vec<(String, usize)> = Vec::new();
    let mut head_score = -1i32;
    let mut head_section = String::new();
    let mut head_summary = String::new();

    for cf in changes_list {
        let content = change_file_content(cwd, cf);
        if content.is_empty() {
            continue;
        }
        let (frontmatter, body) = split_frontmatter(&content);
        let section =
            extract_field(&frontmatter, "section").unwrap_or_else(|| "Changed".to_string());
        let bump = extract_field(&frontmatter, "bump").unwrap_or_default();
        let oneline = {
            let joined = body.split_whitespace().collect::<Vec<_>>().join(" ");
            if joined.is_empty() {
                cf.clone()
            } else {
                joined
            }
        };
        bullets.push(format!("- {oneline}"));

        let ws = match cf.split_once("/.changes/") {
            Some((prefix, _)) => prefix.to_string(),
            None => ".".to_string(),
        };
        match ws_count.iter_mut().find(|(w, _)| *w == ws) {
            Some(entry) => entry.1 += 1,
            None => ws_count.push((ws, 1)),
        }

        let score = bump_rank(&bump) * 10 + section_rank(&section);
        if score > head_score {
            head_score = score;
            head_section = section;
            head_summary = oneline;
        }
    }

    if bullets.is_empty() {
        return None;
    }
    if head_section.is_empty() {
        head_section = "Changed".to_string();
    }
    if head_summary.is_empty() {
        head_summary = bullets[0]
            .strip_prefix("- ")
            .unwrap_or(&bullets[0])
            .to_string();
    }

    let mut scope = String::new();
    let mut best = -1i64;
    for (w, c) in &ws_count {
        if (*c as i64) > best {
            best = *c as i64;
            scope = w.clone();
        }
    }

    let emoji = emoji_for(&head_section);
    let type_ = type_for(&head_section);
    let mut subj = if !scope.is_empty() && scope != "." {
        format!("{emoji} {type_}({scope}): {head_summary}")
    } else {
        format!("{emoji} {type_}: {head_summary}")
    };
    if subj.chars().count() > 72 {
        let truncated: String = subj.chars().take(71).collect();
        subj = format!("{truncated}…");
    }

    let mut out = subj;
    out.push_str("\n\n");
    out.push_str(&bullets.join("\n"));
    Some(out)
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

    // ── build_message_from_changes ────────────────────────────────────────

    #[test]
    fn build_message_single_change_file() {
        let repo = init_repo();
        write(
            &repo.path().join("pkg"),
            ".changes/260101-0000.md",
            "---\nbump: minor\nsection: Fixed\n---\nВиправлено помилку типізації\n",
        );
        let cf = "pkg/.changes/260101-0000.md".to_string();
        let msg = build_message_from_changes(repo.path(), &[cf]).unwrap();
        assert!(msg.starts_with("🐛 fix(pkg): Виправлено помилку типізації"));
        assert!(msg.contains("- Виправлено помилку типізації"));
    }

    #[test]
    fn build_message_picks_highest_bump_as_head() {
        let repo = init_repo();
        write(
            repo.path(),
            "pkg/.changes/a.md",
            "---\nbump: patch\nsection: Fixed\n---\nДрібний фікс\n",
        );
        write(
            repo.path(),
            "pkg/.changes/b.md",
            "---\nbump: major\nsection: Added\n---\nВелика нова фіча\n",
        );
        let msg = build_message_from_changes(
            repo.path(),
            &[
                "pkg/.changes/a.md".to_string(),
                "pkg/.changes/b.md".to_string(),
            ],
        )
        .unwrap();
        assert!(msg.starts_with("✨ feat(pkg): Велика нова фіча"));
        assert!(msg.contains("- Дрібний фікс"));
        assert!(msg.contains("- Велика нова фіча"));
    }

    #[test]
    fn build_message_scope_by_majority_workspace() {
        let repo = init_repo();
        write(
            repo.path(),
            "pkg-a/.changes/a.md",
            "---\nbump: minor\nsection: Changed\n---\nX\n",
        );
        write(
            repo.path(),
            "pkg-a/.changes/b.md",
            "---\nbump: minor\nsection: Changed\n---\nY\n",
        );
        write(
            repo.path(),
            "pkg-b/.changes/c.md",
            "---\nbump: minor\nsection: Changed\n---\nZ\n",
        );
        let msg = build_message_from_changes(
            repo.path(),
            &[
                "pkg-a/.changes/a.md".to_string(),
                "pkg-a/.changes/b.md".to_string(),
                "pkg-b/.changes/c.md".to_string(),
            ],
        )
        .unwrap();
        assert!(msg.starts_with("♻️ refactor(pkg-a):"));
    }

    #[test]
    fn build_message_root_change_no_scope_in_subject() {
        let repo = init_repo();
        write(
            repo.path(),
            ".changes/root.md",
            "---\nbump: minor\nsection: Changed\n---\nКореневий опис\n",
        );
        let msg =
            build_message_from_changes(repo.path(), &[".changes/root.md".to_string()]).unwrap();
        assert!(msg.starts_with("♻️ refactor: Кореневий опис"));
        assert!(!msg.starts_with("♻️ refactor("));
    }

    #[test]
    fn build_message_returns_none_when_no_valid_files() {
        let repo = init_repo();
        assert!(
            build_message_from_changes(repo.path(), &["missing/.changes/x.md".to_string()])
                .is_none()
        );
    }

    #[test]
    fn build_message_truncates_long_subject() {
        let repo = init_repo();
        let long = "а".repeat(100);
        write(
            repo.path(),
            "pkg/.changes/a.md",
            &format!("---\nbump: minor\nsection: Changed\n---\n{long}\n"),
        );
        let msg =
            build_message_from_changes(repo.path(), &["pkg/.changes/a.md".to_string()]).unwrap();
        let subject = msg.lines().next().unwrap();
        assert!(subject.ends_with('…'));
        assert!(subject.chars().count() <= 72);
    }

    // ── split_adr / contains_docs_segment ─────────────────────────────────

    #[test]
    fn split_adr_collapses_adr_files() {
        let names = "M\tsrc/a.rs\nA\tdocs/adr/260101-x.md\nA\tdocs/adr/260102-y.md\n";
        let (kept, adr) = split_adr(names);
        assert_eq!(kept, vec!["M\tsrc/a.rs".to_string()]);
        assert_eq!(adr, 2);
    }

    #[test]
    fn contains_docs_segment_matches_various_positions() {
        assert!(contains_docs_segment("docs/index.md"));
        assert!(contains_docs_segment("M\tdocs/index.md"));
        assert!(contains_docs_segment("pkg/docs/index.md"));
        assert!(!contains_docs_segment("mydocs/index.md"));
    }

    // ── run() end-to-end ───────────────────────────────────────────────────

    fn init_upstream_and_clone() -> (tempfile::TempDir, tempfile::TempDir) {
        let upstream = init_repo();
        // upstream — не bare-репо з checked-out гілкою; за замовчуванням git відмовляє push у
        // поточну гілку. Тести тут пушать назад в upstream, тож дозволяємо (робоче дерево
        // upstream після push лишається застарілим — тестам воно не потрібне).
        git(
            upstream.path(),
            &["config", "receive.denyCurrentBranch", "ignore"],
        );
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
    fn nothing_to_push_when_no_local_changes() {
        let (_upstream, local) = init_upstream_and_clone();
        let outcome = run(local.path(), None, None, None).unwrap();
        assert!(matches!(outcome, PushOutcome::NothingToPush { .. }));
    }

    #[test]
    fn pushes_with_message_from_change_file() {
        let (upstream, local) = init_upstream_and_clone();
        write(local.path(), "src/feature.txt", "new feature\n");
        write(
            local.path(),
            ".changes/260101-0000.md",
            "---\nbump: minor\nsection: Added\n---\nДодано нову фічу\n",
        );

        let outcome = run(local.path(), None, None, None).unwrap();
        match outcome {
            PushOutcome::Done {
                subject,
                message_source,
                pushed_new_branch,
                ..
            } => {
                assert!(subject.contains("Додано нову фічу"));
                assert_eq!(message_source, MessageSource::Changes);
                assert!(!pushed_new_branch, "гілка main уже існує на origin");
            }
            other => panic!("expected Done, got {other:?}"),
        }

        // Перевіряємо, що на upstream реально дійшло.
        let upstream_log = git_ok(upstream.path(), &["log", "-1", "--format=%s"]).unwrap();
        assert!(upstream_log.contains("Додано нову фічу"));
    }

    struct StubGenerator(&'static str);
    impl CommitMessageGenerator for StubGenerator {
        fn generate(&self, _context: &str) -> std::io::Result<String> {
            Ok(self.0.to_string())
        }
    }

    #[test]
    fn falls_back_to_llm_generator_without_change_files() {
        let (_upstream, local) = init_upstream_and_clone();
        write(local.path(), "src/plain.txt", "no change file here\n");

        let generator = StubGenerator("✨ feat(src): щось нове\n\n- деталь");
        let outcome = run(local.path(), None, None, Some(&generator)).unwrap();
        match outcome {
            PushOutcome::Done {
                subject,
                message_source,
                ..
            } => {
                assert_eq!(subject, "✨ feat(src): щось нове");
                assert_eq!(message_source, MessageSource::Llm);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn errors_without_generator_and_without_change_files() {
        let (_upstream, local) = init_upstream_and_clone();
        write(local.path(), "src/plain.txt", "no change file here\n");
        let err = run(local.path(), None, None, None).unwrap_err();
        assert!(matches!(err, NError::Message(_)));
    }

    #[test]
    fn first_push_of_new_branch_uses_push_u() {
        let (_upstream, local) = init_upstream_and_clone();
        git(local.path(), &["checkout", "-q", "-b", "feature"]);
        write(
            local.path(),
            ".changes/260101-0000.md",
            "---\nbump: patch\nsection: Fixed\n---\nНова гілка\n",
        );

        let outcome = run(local.path(), Some("feature"), None, None).unwrap();
        match outcome {
            PushOutcome::Done {
                pushed_new_branch, ..
            } => assert!(pushed_new_branch),
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn diverged_origin_auto_pulls_before_squash() {
        let (upstream, local) = init_upstream_and_clone();

        write(upstream.path(), "a.txt", "hello\nfrom upstream\n");
        git(upstream.path(), &["add", "-A"]);
        git(
            upstream.path(),
            &["commit", "-q", "-m", "upstream advances"],
        );

        write(local.path(), "c.txt", "local work\n");
        write(
            local.path(),
            ".changes/260101-0000.md",
            "---\nbump: patch\nsection: Fixed\n---\nЛокальний фікс\n",
        );

        let outcome = run(local.path(), None, None, None).unwrap();
        match outcome {
            PushOutcome::Done { auto_pulled, .. } => {
                assert!(auto_pulled.is_some());
                assert!(auto_pulled.unwrap().is_clean());
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(local.path().join("a.txt")).unwrap(),
            "hello\nfrom upstream\n"
        );
        assert_eq!(
            std::fs::read_to_string(local.path().join("c.txt")).unwrap(),
            "local work\n"
        );
    }
}
