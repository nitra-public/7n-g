//! Нативний TUI fuzzy-picker для [`crate::getw::WorktreePicker`] — реалізація
//! seam-у, описаного в doc-коментарі `getw.rs` (ADR 20260814-195911): замінює
//! `fzf`-бінарник, нічого зовнішнього не шелить.
//!
//! Малює список кандидатів у поточному терміналі (`crossterm` — raw mode +
//! alternate screen, крос-платформно: Linux/macOS/Windows), живий fuzzy-фільтр
//! по введенню (`nucleo-matcher` — той самий движок, що в helix/telescope.nvim).
//! Навігація: `↑`/`↓`, `Ctrl-P`/`Ctrl-N` (аналог vim-навігації `j`/`k`, без
//! конфлікту з набором тексту фільтра — на відміну від прямих `j`/`k`, які
//! користувач і так вводить у рядок пошуку). `Enter` — вибір, `Esc`/`Ctrl-C` —
//! скасування (`Ok(None)`, контракт трейту).
//!
//! Поза реальним TTY (stdin/stdout не термінал — напр. CI) — `pick` повертає
//! `Err`, а не панікує чи блокується на `event::read()`.

use std::io::{self, Write};

use crossterm::cursor::MoveTo;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::tty::IsTty;
use crossterm::{execute, queue, style};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

use crate::getw::{WorktreeCandidate, WorktreePicker};

pub struct TuiPicker;

impl WorktreePicker for TuiPicker {
    fn pick<'a>(
        &self,
        candidates: &'a [WorktreeCandidate],
    ) -> io::Result<Option<&'a WorktreeCandidate>> {
        if candidates.is_empty() {
            return Ok(None);
        }
        if !io::stdin().is_tty() || !io::stdout().is_tty() {
            return Err(io::Error::other(
                "TuiPicker потребує реального термінала (TTY) — stdin/stdout не termінал",
            ));
        }

        let _guard = RawScreenGuard::enter()?;
        let mut stdout = io::stdout();
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut query = String::new();
        let mut selected: usize = 0;

        loop {
            let filtered = filter(candidates, &query, &mut matcher);
            if selected >= filtered.len() {
                selected = filtered.len().saturating_sub(1);
            }
            let (cols, rows) = crossterm::terminal::size()?;
            render(
                &mut stdout,
                &query,
                &filtered,
                candidates,
                selected,
                cols,
                rows,
            )?;

            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None);
                }
                KeyCode::Enter => {
                    return Ok(filtered.get(selected).map(|&idx| &candidates[idx]));
                }
                KeyCode::Up => selected = selected.saturating_sub(1),
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    selected = selected.saturating_sub(1)
                }
                KeyCode::Down if selected + 1 < filtered.len() => selected += 1,
                KeyCode::Char('n')
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && selected + 1 < filtered.len() =>
                {
                    selected += 1
                }
                KeyCode::Backspace => {
                    query.pop();
                    selected = 0;
                }
                KeyCode::Char(c)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    query.push(c);
                    selected = 0;
                }
                _ => {}
            }
        }
    }
}

/// RAII-обгортка над raw mode + alternate screen: незалежно від того, як `pick`
/// завершиться (вибір/скасування/помилка читання подій), термінал повертається
/// у звичайний режим при виході зі скоупу.
struct RawScreenGuard;

impl RawScreenGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for RawScreenGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// Індекси кандидатів (у `candidates`), що пройшли fuzzy-фільтр по `query`,
/// відсортовані за релевантністю (найкращий збіг — перший). Порожній `query` —
/// усі кандидати в початковому порядку.
fn filter(candidates: &[WorktreeCandidate], query: &str, matcher: &mut Matcher) -> Vec<usize> {
    if query.is_empty() {
        return (0..candidates.len()).collect();
    }

    struct Hay<'a> {
        idx: usize,
        text: &'a str,
    }
    impl AsRef<str> for Hay<'_> {
        fn as_ref(&self) -> &str {
            self.text
        }
    }

    let haystacks: Vec<String> = candidates
        .iter()
        .map(|c| {
            format!(
                "{} {} {}",
                c.name,
                c.branch,
                c.task.as_deref().unwrap_or("")
            )
        })
        .collect();
    let items: Vec<Hay> = haystacks
        .iter()
        .enumerate()
        .map(|(idx, text)| Hay { idx, text })
        .collect();

    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut matches = pattern.match_list(items, matcher);
    matches.sort_by_key(|(_hay, score)| std::cmp::Reverse(*score));
    matches.into_iter().map(|(hay, _score)| hay.idx).collect()
}

#[allow(clippy::too_many_arguments)]
fn render(
    stdout: &mut io::Stdout,
    query: &str,
    filtered: &[usize],
    candidates: &[WorktreeCandidate],
    selected: usize,
    cols: u16,
    rows: u16,
) -> io::Result<()> {
    let width = cols.max(20) as usize;

    queue!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
    queue!(stdout, style::Print(format!("> {query}\r\n")))?;
    queue!(
        stdout,
        style::Print(format!(
            "Знайдено {}/{} — ↑↓/Ctrl-P/Ctrl-N навігація, Enter вибір, Esc/Ctrl-C скасування\r\n",
            filtered.len(),
            candidates.len()
        ))
    )?;
    if filtered.is_empty() {
        queue!(stdout, style::Print("  (нічого не знайдено)\r\n"))?;
    }

    let visible_rows = rows.saturating_sub(2).max(1) as usize;
    let start = if selected >= visible_rows {
        selected + 1 - visible_rows
    } else {
        0
    };
    for (row_i, &idx) in filtered.iter().enumerate().skip(start).take(visible_rows) {
        let c = &candidates[idx];
        let marker = if row_i == selected { "➤ " } else { "  " };
        let mut line = format!("{marker}{}", c.name);
        if !c.branch.is_empty() && c.branch != c.name {
            line.push_str(&format!("  [{}]", c.branch));
        }
        if let Some(task) = &c.task {
            line.push_str(&format!("  — {task}"));
        }
        let mut meta = Vec::new();
        if let Some(created) = &c.created {
            meta.push(format!("створено {created}"));
        }
        if let Some(modified) = &c.modified {
            meta.push(format!("змінено {modified}"));
        }
        if !meta.is_empty() {
            line.push_str(&format!("  ({})", meta.join(", ")));
        }
        queue!(
            stdout,
            style::Print(format!("{}\r\n", truncate(&line, width)))
        )?;
    }

    queue!(stdout, MoveTo(2 + query.chars().count() as u16, 0))?;
    stdout.flush()
}

fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}
