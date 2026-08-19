//! Ручна перевірка [`n7n_g::tui_picker::TuiPicker`] у реальному терміналі — не
//! автотест (пікер потребує TTY), запускати вручну: `cargo run --example
//! getw_picker_demo`. Створює кілька фейкових [`WorktreeCandidate`] і показує
//! вибір/скасування.

use std::path::PathBuf;

use n7n_g::getw::{WorktreeCandidate, WorktreePicker};
use n7n_g::tui_picker::TuiPicker;

fn main() {
    let candidates = vec![
        WorktreeCandidate {
            name: "feature-alpha".into(),
            path: PathBuf::from("/tmp/demo/.worktrees/feature-alpha"),
            branch: "feature-alpha".into(),
            task: Some("Портувати getw picker на нативний TUI".into()),
            created: Some("2026-08-18 10:15".into()),
            modified: Some("2026-08-19 09:42".into()),
        },
        WorktreeCandidate {
            name: "fix-n-ch-path-scope".into(),
            path: PathBuf::from("/tmp/demo/.worktrees/fix-n-ch-path-scope"),
            branch: "fix/n-ch-path-scope".into(),
            task: None,
            created: Some("2026-08-14 19:59".into()),
            modified: Some("2026-08-15 08:03".into()),
        },
        WorktreeCandidate {
            name: "gix-worktree-list".into(),
            path: PathBuf::from("/tmp/demo/.worktrees/gix-worktree-list"),
            branch: "feat/gix-worktree-list".into(),
            task: Some("Порт `git worktree list` на gix".into()),
            created: Some("2026-08-10 12:00".into()),
            modified: Some("2026-08-12 16:30".into()),
        },
    ];

    match TuiPicker.pick(&candidates) {
        Ok(Some(c)) => println!("Обрано: {} ({})", c.name, c.branch),
        Ok(None) => println!("Скасовано."),
        Err(e) => println!("Помилка (можливо, не TTY): {e}"),
    }
}
