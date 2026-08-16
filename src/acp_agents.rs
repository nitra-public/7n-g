//! Реалізація трьох LLM-seam'ів ([`crate::merge::ConflictResolver`],
//! [`crate::ch::MessageGenerator`], [`crate::push::CommitMessageGenerator`]) через
//! `llm_lib` (`n7n-llm-lib` на crates.io, той самий крейт, що й в екосистемі
//! `7n-rules` — ADR 20260814-195911, ідея #46/#47).
//!
//! **Два різні шляхи, за оригінальним призначенням кожного seam'у**:
//! - [`merge::ConflictResolver`] і [`ch::MessageGenerator`] — у JS-оригіналі
//!   (`omlx-resolve.mjs`/`omlx.mjs`) використовували ЛОКАЛЬНУ модель (приватно,
//!   без cloud-підписки). Тут — `llm_lib::local_cloud::LocalCloud` (прямий
//!   OpenAI-сумісний HTTP-виклик до omlx, дефолт `127.0.0.1:8000`), з фолбеком
//!   на ACP-каскад, якщо `N_LOCAL_*_MODEL` не сконфігуровано чи omlx недоступний
//!   (краще спробувати cloud, ніж мовчки нічого не зробити).
//! - [`push::CommitMessageGenerator`] — у JS-оригіналі використовував саме
//!   cloud CLI-агенти (`pi`/`claude`/`cursor-agent`). Тут — лише ACP-каскад
//!   (`Cursor` → `Codex` → `Pi`, перший непорожній результат перемагає), без
//!   local-кроку: комерційна commit-message генерація не мала локального шляху
//!   в оригіналі, і немає підстав його додавати.
//!
//! **Sync/async міст**: три трейти цього crate — синхронні, а виклики `llm_lib`
//! (`one_shot_acp`, `LocalCloud::one_shot`) — `async fn`. [`AcpAgentAdapter`]
//! тримає власний `tokio`-рантайм (`current_thread`, лінива ініціалізація
//! per-call) і блокує на ньому — прийнятно для CLI, де кожна LLM-операція й так
//! послідовна й одноразова.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use llm_lib::acp::{one_shot_acp, AcpAgentKind};
use llm_lib::local_cloud::{default_local_openai_provider, LocalCloud};
use llm_lib::tiers::Tier;

use crate::{ch, merge, push};

/// Порядок спроб ACP-агентів — той самий намір, що й `pi → claude → cursor-agent`
/// у JS-оригіналі (особиста підписка спершу, без явного вибору моделі/тіру).
const ACP_CASCADE: &[AcpAgentKind] = &[AcpAgentKind::Cursor, AcpAgentKind::Codex, AcpAgentKind::Pi];

pub struct AcpAgentAdapter {
    /// Робочий каталог для ACP-сесії, коли викликач не передає власний
    /// (наразі лише [`push::CommitMessageGenerator`], чий трейт без `cwd`-параметра).
    default_cwd: PathBuf,
    local: LocalCloud,
}

impl AcpAgentAdapter {
    pub fn new(default_cwd: impl Into<PathBuf>) -> Self {
        // Реальні N_LOCAL_*_MODEL у цій екосистемі використовують префікс `omlx/...`
        // (не `local-openai/...`, який лише позначає "локальний" у tiers::is_local_model
        // за замовчуванням) — реєструємо обидва ключі на той самий provider-конфіг,
        // щоб LocalCloud::one_shot резолвив provider незалежно від того, який
        // префікс фактично в env.
        let provider = default_local_openai_provider();
        let mut providers = HashMap::new();
        providers.insert("omlx".to_string(), provider.clone());
        providers.insert("local-openai".to_string(), provider);
        Self {
            default_cwd: default_cwd.into(),
            local: LocalCloud::new(providers),
        }
    }

    fn block_on<F: std::future::Future>(&self, fut: F) -> std::io::Result<F::Output> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(std::io::Error::other)
            .map(|rt| rt.block_on(fut))
    }

    /// `local_min → local_avg → local_max` (`N_LOCAL_*_MODEL`) через omlx —
    /// `None`, якщо жодного не сконфігуровано чи виклик не вдався (тоді
    /// викликач фолбечить на [`Self::acp_cascade`]).
    fn local_omlx(&self, prompt: &str) -> Option<String> {
        self.block_on(self.local.one_shot(Tier::Min, None, prompt))
            .ok()?
            .ok()
            .filter(|t| !t.trim().is_empty())
    }

    fn acp_cascade(&self, cwd: &Path, prompt: &str) -> std::io::Result<String> {
        self.block_on(async {
            let mut last_err: Option<String> = None;
            for kind in ACP_CASCADE {
                match one_shot_acp(*kind, prompt, cwd).await {
                    Ok(text) if !text.trim().is_empty() => return Ok(text),
                    Ok(_) => last_err = Some(format!("{kind:?}: порожня відповідь")),
                    Err(e) => last_err = Some(format!("{kind:?}: {e}")),
                }
            }
            Err(std::io::Error::other(last_err.unwrap_or_else(|| {
                "жоден ACP-агент (Cursor/Codex/Pi) не відповів".to_string()
            })))
        })?
    }

    /// Локальний omlx першим (приватно/дешево), ACP-каскад — фолбек.
    fn one_shot_local_first(&self, cwd: &Path, prompt: &str) -> std::io::Result<String> {
        if let Some(text) = self.local_omlx(prompt) {
            return Ok(text);
        }
        self.acp_cascade(cwd, prompt)
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
        self.one_shot_local_first(cwd, &prompt)
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
        let raw = self.one_shot_local_first(repo_root, &prompt)?;
        Ok(ch::clean_generated_message(&raw))
    }
}

impl push::CommitMessageGenerator for AcpAgentAdapter {
    fn generate(&self, context: &str) -> std::io::Result<String> {
        let prompt = push::commit_message_prompt(context);
        self.acp_cascade(&self.default_cwd, &prompt)
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
    fn acp_cascade_returns_nonempty_text() {
        let cwd = std::env::current_dir().unwrap();
        let adapter = AcpAgentAdapter::new(&cwd);
        let out = adapter
            .acp_cascade(&cwd, "Скажи рівно одне слово: тест")
            .expect("хоча б один ACP-агент має відповісти");
        assert!(!out.trim().is_empty());
        println!("ACP response: {out:?}");
    }

    /// Ручна перевірка: потребує запущеного omlx-сервера (`http://127.0.0.1:8000`)
    /// і `N_LOCAL_MIN_MODEL` в env. `#[ignore]` з тієї самої причини.
    #[test]
    #[ignore]
    fn local_omlx_returns_text_when_configured() {
        let cwd = std::env::current_dir().unwrap();
        let adapter = AcpAgentAdapter::new(&cwd);
        let out = adapter.local_omlx("Скажи рівно одне слово: тест");
        println!("omlx response: {out:?}");
        assert!(out.is_some());
    }

    /// Реальний сценарій цієї машини: `N_LOCAL_MIN_MODEL=omlx/...` заданий, але omlx
    /// сервер не запущений (`127.0.0.1:8000` connection refused) — `one_shot_local_first`
    /// має мовчки провалити local-крок і впасти на ACP-каскад, а не повернути помилку.
    #[test]
    #[ignore]
    fn falls_back_to_acp_when_omlx_configured_but_unreachable() {
        let cwd = std::env::current_dir().unwrap();
        let adapter = AcpAgentAdapter::new(&cwd);
        assert!(
            adapter.local_omlx("Скажи рівно одне слово: тест").is_none(),
            "omlx сервер не має бути досяжний у цьому прогоні"
        );
        let out = adapter
            .one_shot_local_first(&cwd, "Скажи рівно одне слово: тест")
            .expect("має впасти на ACP-каскад і отримати відповідь");
        assert!(!out.trim().is_empty());
        println!("fallback response: {out:?}");
    }
}
