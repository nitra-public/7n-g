//! `n7n_g` — бібліотечне ядро CLI `g` (скорочено від git). Кожен модуль відповідає
//! одній команді JS-оригіналу (`npm/src/*.js` у монорепо `7n`) — див. ADR
//! 20260814-195911 у `docs/adr/`.

#[cfg(feature = "agents")]
pub mod acp_agents;
pub mod ch;
pub mod diff_context;
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

    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, NError>;

/// Еквівалент `git rev-parse --is-inside-work-tree`: `true` лише якщо `cwd`
/// лежить у не-bare репозиторії з робочим деревом (а не всередині `.git/`
/// чи в bare-репозиторії).
pub fn is_inside_work_tree(cwd: &std::path::Path) -> bool {
    gix::discover(cwd)
        .map(|repo| repo.work_dir().is_some())
        .unwrap_or(false)
}

/// gix-хелпери, що замінюють читальні (без мутації робочого дерева/мережі)
/// підкоманди `git` — `rev-parse --verify`, `merge-base`, `merge-base
/// --is-ancestor`, `show <rev>:<path>`, `cat-file -e <rev>:<path>`,
/// `branch --show-current`.
pub mod gix_util {
    use std::path::Path;

    /// Еквівалент `git rev-parse --verify --quiet <spec>`: повний hex sha
    /// об'єкта, або `None`, якщо `spec` не резолвиться (не помилка — саме так
    /// поводиться `--quiet`).
    pub fn rev_parse(cwd: &Path, spec: &str) -> Option<String> {
        let repo = gix::discover(cwd).ok()?;
        repo.rev_parse_single(spec)
            .ok()
            .map(|id| id.to_hex().to_string())
    }

    /// Еквівалент `git merge-base <one> <two>`: hex sha спільного предка, або
    /// `None`, якщо історії не мають спільного предка (не помилка).
    pub fn merge_base(cwd: &Path, one: &str, two: &str) -> Option<String> {
        let repo = gix::discover(cwd).ok()?;
        let one = repo.rev_parse_single(one).ok()?;
        let two = repo.rev_parse_single(two).ok()?;
        repo.merge_base(one.detach(), two.detach())
            .ok()
            .map(|id| id.to_hex().to_string())
    }

    /// Еквівалент `git merge-base --is-ancestor <ancestor> <descendant>`.
    pub fn is_ancestor(cwd: &Path, ancestor: &str, descendant: &str) -> bool {
        let Some(repo) = gix::discover(cwd).ok() else {
            return false;
        };
        let Some(ancestor_id) = repo.rev_parse_single(ancestor).ok() else {
            return false;
        };
        let Some(descendant_id) = repo.rev_parse_single(descendant).ok() else {
            return false;
        };
        let ancestor_id = ancestor_id.detach();
        repo.merge_base(ancestor_id, descendant_id.detach())
            .map(|base| base.detach() == ancestor_id)
            .unwrap_or(false)
    }

    /// Еквівалент `git branch --show-current`: `None` для detached HEAD або
    /// щойно ініціалізованого репозиторію без комітів.
    pub fn current_branch(cwd: &Path) -> Option<String> {
        let repo = gix::discover(cwd).ok()?;
        let name = repo.head_name().ok().flatten()?;
        let branch = name.as_bstr().strip_prefix(b"refs/heads/")?.to_vec();
        String::from_utf8(branch).ok()
    }

    /// Еквівалент `git show <rev>:<rel>`: вміст blob на `rev`, або `None`,
    /// якщо шлях не існує на цьому ref (новостворений/видалений файл).
    pub fn read_blob_at(cwd: &Path, rev: &str, rel: &str) -> Option<Vec<u8>> {
        let repo = gix::discover(cwd).ok()?;
        let id = repo.rev_parse_single(rev).ok()?;
        let commit = id.object().ok()?.try_into_commit().ok()?;
        let tree = commit.tree().ok()?;
        let entry = tree.lookup_entry_by_path(rel).ok().flatten()?;
        if entry.mode().is_tree() {
            return None;
        }
        let data = entry.object().ok()?.data.clone();
        Some(data)
    }

    /// Еквівалент `git cat-file -e <rev>:<rel>`.
    pub fn path_exists_at(cwd: &Path, rev: &str, rel: &str) -> bool {
        read_blob_at(cwd, rev, rel).is_some()
    }

    /// Еквівалент `git diff --quiet <a> <b>` (лише булевий факт наявності
    /// різниці між деревами двох ревізій, без побудови самого патчу).
    /// `false` (немає різниці), якщо будь-яку з ревізій не вдалося
    /// зарезолвити — узгоджено з тим, що виклики цієї функції завжди йдуть
    /// після успішного `rev_parse`/`merge_base` на тих самих ревізіях.
    pub fn trees_differ(cwd: &Path, a: &str, b: &str) -> bool {
        use gix::object::tree::diff::Action;

        let Some(repo) = gix::discover(cwd).ok() else {
            return false;
        };
        let Some(tree_a) = repo
            .rev_parse_single(a)
            .ok()
            .and_then(|id| id.object().ok())
            .and_then(|o| o.try_into_commit().ok())
            .and_then(|c| c.tree().ok())
        else {
            return false;
        };
        let Some(tree_b) = repo
            .rev_parse_single(b)
            .ok()
            .and_then(|id| id.object().ok())
            .and_then(|o| o.try_into_commit().ok())
            .and_then(|c| c.tree().ok())
        else {
            return false;
        };
        let mut differs = false;
        let Ok(mut platform) = tree_a.changes() else {
            return false;
        };
        let _ = platform.for_each_to_obtain_tree(&tree_b, |_change| {
            differs = true;
            Ok::<_, std::convert::Infallible>(Action::Cancel)
        });
        differs
    }
}
