//! Rust-порт `diff-context.js` (105 рядків, монорепо `7n`) — спільне джерело шумних
//! шляхів і лімітів обрізання diff-контексту для LLM (changelog-опис у `ch.rs`,
//! згодом — commit-меседж у `push.rs`, щоб набори не розходились).

/// Шумні glob-и (без префікса pathspec) — їхній ВМІСТ виключаємо з diff-контексту;
/// самі файли лишаються в коміті, їхні імена агент усе одно бачить у name-status.
pub const NOISE_GLOBS: &[&str] = &[
    "docs/**",
    "**/docs/**",
    "**/CHANGELOG.md",
    "**/.changes/**",
    // `.n-cursor/**`/`**/.n-cursor/**` прибрано (llm-lib 0.3, спека
    // `2026-08-17-n7n-harness-local-models.md` §3.14, п.6): корінь
    // траєкторії тепер `~/.n-llm-lib` (рішення З) — домашній каталог, який у
    // diff у принципі не потрапляє, тож ці два рядки були мертвим кодом.
    // `**/*.jsonl` ЛИШАЄТЬСЯ — виправданий сам собою (JSONL це майже завжди
    // дамп даних, шум для моделі незалежно від походження).
    "**/*.jsonl",
    "*.lock",
    "**/package-lock.json",
    "**/pnpm-lock.yaml",
    "**/yarn.lock",
    "**/*.snap",
    "**/__snapshots__/**",
    "**/*.min.js",
    "**/*.map",
    "**/*.d.ts",
    "dist/**",
    "build/**",
    "coverage/**",
];

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_lines: usize,
    pub max_line_len: usize,
    pub max_bytes: usize,
    pub max_file_bytes: usize,
}

pub const DEFAULT_LIMITS: Limits = Limits {
    max_lines: 1500,
    max_line_len: 500,
    max_bytes: 262_144,
    max_file_bytes: 16_384,
};

/// git-pathspec'и виключення (`:(exclude)<glob>`).
pub fn exclude_pathspecs(globs: &[&str]) -> Vec<String> {
    globs.iter().map(|g| format!(":(exclude){g}")).collect()
}

/// Обрізає текст до `max_bytes` у UTF-8, не лишаючи обрубаного multibyte-символа в кінці.
pub fn clamp_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Трирівневе обрізання: рядки (`max_lines`) → довжина рядка (`max_line_len`,
/// у символах — JS-оригінал рахує в UTF-16 code units, різниця лише для astral-символів)
/// → тверда байтова стеля (`max_bytes`). Додає позначки про обрізане.
pub fn truncate_context(text: &str, limits: &Limits) -> String {
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    let omitted = lines.len().saturating_sub(limits.max_lines);
    if omitted > 0 {
        lines.truncate(limits.max_lines);
    }
    for line in &mut lines {
        if line.chars().count() > limits.max_line_len {
            *line = line.chars().take(limits.max_line_len).collect();
        }
    }
    let mut out = lines.join("\n");
    let bytes_clipped = out.len() > limits.max_bytes;
    if bytes_clipped {
        out = clamp_bytes(&out, limits.max_bytes);
    }
    let mut notes = Vec::new();
    if omitted > 0 {
        notes.push(format!(
            "… (обрізано: {omitted} рядків понад ліміт {})",
            limits.max_lines
        ));
    }
    if bytes_clipped {
        notes.push(format!(
            "… (обрізано за байтами до ~{}; рядки — до {} симв.)",
            limits.max_bytes, limits.max_line_len
        ));
    }
    if notes.is_empty() {
        out
    } else {
        format!("{out}\n{}", notes.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_bytes_keeps_short_text() {
        assert_eq!(clamp_bytes("hello", 100), "hello");
    }

    #[test]
    fn clamp_bytes_cuts_at_char_boundary() {
        let text = "日本語"; // 3 chars, 3 bytes each in UTF-8
        let clamped = clamp_bytes(text, 4);
        assert!(clamped.len() <= 4);
        assert!(clamped.is_char_boundary(clamped.len()));
        assert_eq!(clamped, "日");
    }

    #[test]
    fn truncate_context_notes_omitted_lines() {
        let text = (0..10)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let limits = Limits {
            max_lines: 3,
            ..DEFAULT_LIMITS
        };
        let out = truncate_context(&text, &limits);
        assert!(out.starts_with("0\n1\n2"));
        assert!(out.contains("обрізано: 7 рядків"));
    }

    #[test]
    fn truncate_context_clips_long_lines() {
        let text = "a".repeat(10);
        let limits = Limits {
            max_line_len: 3,
            ..DEFAULT_LIMITS
        };
        let out = truncate_context(&text, &limits);
        assert!(out.starts_with("aaa"));
        assert!(!out.starts_with("aaaa"));
    }
}
