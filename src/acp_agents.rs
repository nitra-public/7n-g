//! Реалізація трьох LLM-seam'ів ([`crate::merge::ConflictResolver`],
//! [`crate::ch::MessageGenerator`], [`crate::push::CommitMessageGenerator`]) через
//! ACP (Agent Client Protocol) — `llm_lib::acp::one_shot_acp`, той самий крейт, що й
//! в екосистемі `7n-rules` (ADR 20260814-195911, ідея #46/#47).
//!
//! Кожен виклик каскадом пробує `Cursor` → `Codex` → `Pi` (перший, що відповість
//! непорожнім текстом, — переможець; ACP спавнить уже залогінений локально CLI
//! агента особистою підпискою, без API-ключів у цьому крейті). Немає жодного
//! вбудованого retry понад цей каскад — fail-fast, як і решта `llm-lib`.
//!
//! **Sync/async міст**: три трейти цього crate — синхронні (узгоджено з рештою
//! `n7n_g`, яка не має async-рантайму в основному потоці), а `one_shot_acp` —
//! `async fn`. [`AcpAgentAdapter`] тримає власний `tokio`-рантайм
//! (`current_thread`, лінива ініціалізація per-call) і блокує на ньому — прийнятно
//! для CLI, де кожна LLM-операція й так послідовна й одноразова.

use std::path::{Path, PathBuf};

use llm_lib::acp::{one_shot_acp, AcpAgentKind};

use crate::{ch, merge, push};

/// Порядок спроб ACP-агентів — той самий намір, що й `pi → claude → cursor-agent`
/// у JS-оригіналі (особиста підписка спершу, без явного вибору моделі/тіру).
const CASCADE: &[AcpAgentKind] = &[AcpAgentKind::Cursor, AcpAgentKind::Codex, AcpAgentKind::Pi];

pub struct AcpAgentAdapter {
    /// Робочий каталог для ACP-сесії, коли викликач не передає власний
    /// (наразі лише [`push::CommitMessageGenerator`], чий трейт без `cwd`-параметра).
    default_cwd: PathBuf,
}

impl AcpAgentAdapter {
    pub fn new(default_cwd: impl Into<PathBuf>) -> Self {
        Self {
            default_cwd: default_cwd.into(),
        }
    }

    fn one_shot(&self, cwd: &Path, prompt: &str) -> std::io::Result<String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(std::io::Error::other)?;

        rt.block_on(async {
            let mut last_err: Option<String> = None;
            for kind in CASCADE {
                match one_shot_acp(*kind, prompt, cwd).await {
                    Ok(text) if !text.trim().is_empty() => return Ok(text),
                    Ok(_) => last_err = Some(format!("{kind:?}: порожня відповідь")),
                    Err(e) => last_err = Some(format!("{kind:?}: {e}")),
                }
            }
            Err(std::io::Error::other(last_err.unwrap_or_else(|| {
                "жоден ACP-агент (Cursor/Codex/Pi) не відповів".to_string()
            })))
        })
    }
}

impl merge::ConflictResolver for AcpAgentAdapter {
    fn resolve(&self, cwd: &Path, files: &[String]) -> std::io::Result<String> {
        let file_list = files
            .iter()
            .map(|f| format!("- {f}"))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = format!(
            "У робочому дереві є файли з git conflict-маркерами (<<<<<<<, |||||||, =======, >>>>>>>) \
після невдалого 3-way merge:\n{file_list}\n\n\
Відредагуй КОЖЕН із цих файлів на диску: прибери маркери, поєднай зміни обох сторін \
логічно й коректно (зрозумій намір кожної сторони, не видаляй жодну довільно). Нічого \
не комітуй і не запускай git-команди. Після завершення виведи короткий (кілька рядків \
на файл) підсумок: що було з кожної сторони і як примирено."
        );
        self.one_shot(cwd, &prompt)
    }
}

impl ch::MessageGenerator for AcpAgentAdapter {
    fn generate(&self, ws: &str, files: &[String], repo_root: &Path) -> std::io::Result<String> {
        let context = ch::workspace_diff_context(repo_root, ws, files);
        let messages = ch::build_gen_messages(ws, &context);
        let prompt = messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let raw = self.one_shot(repo_root, &prompt)?;
        Ok(ch::clean_generated_message(&raw))
    }
}

impl push::CommitMessageGenerator for AcpAgentAdapter {
    fn generate(&self, context: &str) -> std::io::Result<String> {
        let prompt = push::commit_message_prompt(context);
        self.one_shot(&self.default_cwd, &prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ручна smoke-перевірка з реальним ACP-агентом (`cursor-agent`/`codex`/`pi` мають
    /// бути залогінені особистою підпискою локально) — `#[ignore]`, бо потребує
    /// мережі й інтерактивного логіну, недоступного в CI/sandboxed-середовищах.
    /// Запуск: `cargo test --features agents acp_agents::tests -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn one_shot_returns_nonempty_text() {
        let cwd = std::env::current_dir().unwrap();
        let adapter = AcpAgentAdapter::new(&cwd);
        let out = adapter
            .one_shot(&cwd, "Скажи рівно одне слово: тест")
            .expect("хоча б один ACP-агент має відповісти");
        assert!(!out.trim().is_empty());
        println!("ACP response: {out:?}");
    }
}
