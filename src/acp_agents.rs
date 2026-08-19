//! Реалізація трьох LLM-seam'ів ([`crate::merge::ConflictResolver`],
//! [`crate::ch::MessageGenerator`], [`crate::push::CommitMessageGenerator`]) через
//! `llm_lib` (`n7n-llm-lib` на crates.io, той самий крейт, що й в екосистемі
//! `7n-rules` — ADR 20260814-195911, ідея #46/#47).
//!
//! **0.3**: власноруч написаний каскад «локальна модель → ACP» замінено на
//! крейтовий композитор [`llm_lib::cascade::one_shot_cascade`] (спека
//! `llm-lib/docs/specs/2026-08-17-n7n-harness-local-models.md`, §3.14 —
//! `n7n-g` як робочий приклад класу 1, «промпт → відповідь», без канонічного
//! детектора: `n7n-harness` сюди НЕ підключається). Логіка та сама, лише
//! винесена в крейт: `RungAttempt`/trace-рядки (`~/.n-llm-lib/llm-trace-*.jsonl`,
//! `kind: "oneShot"`) пишуться автоматично, ліза `per-item` й прогноз
//! вартості перед стартом — теж.
//!
//! **Три різні комбінації рунгів/egress, за оригінальним призначенням кожного
//! seam'у**:
//! - [`merge::ConflictResolver`] — у JS-оригіналі конфлікти йшли ЛИШЕ в
//!   локальну модель (приватно, без cloud-підписки: повний вміст файлів із
//!   конфліктами). Тут — рівно один рунг ([`Rung::local`]) з
//!   [`EgressPolicy::LocalOnly`] (§2, рішення Ц): жодного фолбеку на ACP.
//!   Недоступний сервер чи зайнята ліза дають НАЗВАНУ помилку
//!   ([`llm_lib::cascade::CascadeError`]), а не тиху ескалацію в
//!   ACP-агентів — приватність цього seam'у важливіша за доступність.
//! - [`ch::MessageGenerator`] — у JS-оригіналі теж локальна модель, але
//!   контекст (diff, без повних файлів) менш чутливий, тож тут лишається
//!   фолбек «краще ACP, ніж мовчки нічого не зробити» —
//!   [`Rung::local`] + ACP-каскад (`Cursor → Codex → Pi`) з
//!   [`EgressPolicy::AllowAcp`].
//! - [`push::CommitMessageGenerator`] — у JS-оригіналі використовував саме
//!   cloud CLI-агенти (`pi`/`claude`/`cursor-agent`). Тут — лише ACP-каскад,
//!   без local-рунга: комерційна commit-message генерація не мала
//!   локального шляху в оригіналі, і немає підстав його додавати.
//!
//! Жоден із трьох рунгів не торкається метрованих хмарних тирів
//! (`Tier::Cloud{Min,Avg,Max}`) — `g` їх не конфігурує; `AllowAcp` і
//! `AllowCloud` тут поводились би однаково, `AllowAcp` обраний як точніший
//! сигнал наміру (§2: «ACP — особиста підписка, не метрований ключ»).
//!
//! **Sync/async міст**: три трейти цього crate — синхронні, а
//! `llm_lib::cascade::one_shot_cascade` — `async fn`. [`AcpAgentAdapter`]
//! тримає власний `tokio`-рантайм (`current_thread`, лінива ініціалізація
//! per-call) і блокує на ньому — прийнятно для CLI, де кожна LLM-операція й
//! так послідовна й одноразова.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use llm_lib::acp::AcpAgentKind;
use llm_lib::budget::{self, EgressPolicy};
use llm_lib::calibration::{self, ModelFingerprint};
use llm_lib::cascade::{self, LocalForecastInput, Rung};
use llm_lib::local_cloud::{default_local_openai_provider, LocalCloud};
use llm_lib::local_lease::{cascade_lease_fn, LeaseMeta};

use crate::{ch, merge, push};

/// Порядок спроб ACP-агентів — той самий намір, що й `pi → claude → cursor-agent`
/// у JS-оригіналі (особиста підписка спершу, без явного вибору моделі/тіру).
/// Goose видалений як ACP-kind рішенням Ч спеки `2026-08-17-n7n-harness-local-models.md`
/// — заміни немає, `g` його й раніше не використовував.
const ACP_CASCADE: &[AcpAgentKind] = &[AcpAgentKind::Cursor, AcpAgentKind::Codex, AcpAgentKind::Pi];

/// Стеля виходу локального рунга: `g` не має ходів чи tool-викликів (клас 1,
/// §3.14), відповіді короткі (підсумок мерджу / рядок changelog) — той самий
/// дефолт, що `FixPolicy::default().output_ceiling` у `n7n-harness` (клас 3),
/// узятий як розумний орієнтир, не запозичена структура.
const LOCAL_OUTPUT_CEILING: u64 = 1024;

pub struct AcpAgentAdapter {
    /// Робочий каталог для ACP-сесії, коли викликач не передає власний
    /// (наразі лише [`push::CommitMessageGenerator`], чий трейт без `cwd`-параметра).
    default_cwd: PathBuf,
    local: LocalCloud,
    /// Ідентифікатор команди для trace (`one_shot_cascade(..., caller)`) і
    /// [`LeaseMeta::run_id`] — напр. `"g pull"`, `"g ch"` (§3.8).
    caller: String,
}

impl AcpAgentAdapter {
    pub fn new(default_cwd: impl Into<PathBuf>, caller: impl Into<String>) -> Self {
        // Реальні N_LOCAL_MODEL у цій екосистемі бачені і з префіксом `omlx/...`
        // (спадок omlx-серверів), і з `local-openai/...` (дефолтний провайдер
        // tiers::is_local_model, TurboFieldfare й аналогічні OpenAI-сумісні
        // сервери) — реєструємо обидва ключі на той самий provider-конфіг, щоб
        // LocalCloud::one_shot резолвив provider незалежно від того, який
        // префікс фактично в env. `is_local_model` (крейт 0.3) на цю мапу не
        // впливає — вона суто для `LocalCloud`, не для класифікації тиру.
        let provider = default_local_openai_provider();
        let mut providers = std::collections::HashMap::new();
        providers.insert("omlx".to_string(), provider.clone());
        providers.insert("local-openai".to_string(), provider);
        Self {
            default_cwd: default_cwd.into(),
            local: LocalCloud::new(providers),
            caller: caller.into(),
        }
    }

    fn block_on<F: std::future::Future>(&self, fut: F) -> std::io::Result<F::Output> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(std::io::Error::other)
            .map(|rt| rt.block_on(fut))
    }

    /// Прогноз вартості локального рунга (рішення Т, §3.14) — `None`, якщо
    /// capability сервера ([`budget::local_capability`]) чи `N_LOCAL_MODEL`
    /// не задані: це факти середовища, вгадувати їх не можна, тож без них
    /// `one_shot_cascade` просто пропускає прогноз і пробує рунг напряму.
    fn local_forecast(&self, prompt: &str) -> Option<LocalForecastInput> {
        let capability = budget::local_capability()?;
        let model_spec = llm_lib::tiers::local_model()?;
        let (provider, model_id) = llm_lib::tiers::parse_model_spec(&model_spec).ok()?;
        let fingerprint = ModelFingerprint::new(
            provider,
            model_id,
            capability.context,
            calibration::UNKNOWN_OWNED_BY,
        );
        Some(LocalForecastInput {
            prompt_tokens: budget::bytes_to_tokens_estimate(prompt.len() as u64),
            output_ceiling: LOCAL_OUTPUT_CEILING,
            capability,
            rates: calibration::resolve_rates(&fingerprint),
        })
    }

    fn local_rung(&self, prompt: &str) -> Rung {
        Rung::local(self.local.clone(), None, prompt.to_string())
    }

    fn acp_rungs(&self, cwd: &Path, prompt: &str) -> Vec<Rung> {
        ACP_CASCADE
            .iter()
            .map(|kind| Rung::acp(*kind, prompt.to_string(), cwd.to_path_buf()))
            .collect()
    }

    /// Спільний виклик [`llm_lib::cascade::one_shot_cascade`]: будує лізу
    /// `per-item` ([`cascade_lease_fn`], §2 рішення П), блокує на ній через
    /// власний рантайм і дропає guard одразу після завершення — саме це і є
    /// `per-item` для класу 1 (§3.14: один виклик `one_shot_cascade` = один item).
    fn run_cascade(
        &self,
        rungs: &[Rung],
        egress: EgressPolicy,
        local_forecast: Option<LocalForecastInput>,
    ) -> std::io::Result<String> {
        let held = Arc::new(Mutex::new(None));
        let lease = cascade_lease_fn(
            LeaseMeta {
                run_id: self.caller.clone(),
                chain_id: None,
            },
            Arc::clone(&held),
        );
        let outcome = self.block_on(cascade::one_shot_cascade(
            rungs,
            egress,
            local_forecast,
            Some(lease),
            &self.caller,
        ))?;
        drop(held);
        outcome
            .map(|o| o.text)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    /// Лише локальний рунг, [`EgressPolicy::LocalOnly`] — [`merge::ConflictResolver`]
    /// (приватність важливіша за доступність, доккоментар модуля).
    fn local_only(&self, prompt: &str) -> std::io::Result<String> {
        let rungs = [self.local_rung(prompt)];
        let forecast = self.local_forecast(prompt);
        self.run_cascade(&rungs, EgressPolicy::LocalOnly, forecast)
    }

    /// Локальний рунг першим, ACP-каскад — фолбек, [`EgressPolicy::AllowAcp`]
    /// — [`ch::MessageGenerator`] (доккоментар модуля). «Ліза не взята» чи
    /// сервер недоступний іде тим самим каналом, що будь-яка інша
    /// [`llm_lib::cascade::RungAttemptOutcome::Unavailable`]/`Failed` — каскад
    /// сам переходить до наступного рунга.
    fn local_then_acp(&self, cwd: &Path, prompt: &str) -> std::io::Result<String> {
        let mut rungs = vec![self.local_rung(prompt)];
        rungs.extend(self.acp_rungs(cwd, prompt));
        let forecast = self.local_forecast(prompt);
        self.run_cascade(&rungs, EgressPolicy::AllowAcp, forecast)
    }

    /// Лише ACP-каскад, без локального рунга, [`EgressPolicy::AllowAcp`] —
    /// [`push::CommitMessageGenerator`] (доккоментар модуля: комерційна
    /// commit-message генерація локального шляху в оригіналі не мала).
    fn acp_only(&self, cwd: &Path, prompt: &str) -> std::io::Result<String> {
        let rungs = self.acp_rungs(cwd, prompt);
        self.run_cascade(&rungs, EgressPolicy::AllowAcp, None)
    }
}

impl merge::ConflictResolver for AcpAgentAdapter {
    fn resolve(&self, _cwd: &Path, files: &[String]) -> std::io::Result<String> {
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
        self.local_only(&prompt)
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
        let raw = self.local_then_acp(repo_root, &prompt)?;
        Ok(ch::clean_generated_message(&raw))
    }
}

impl push::CommitMessageGenerator for AcpAgentAdapter {
    fn generate(&self, context: &str) -> std::io::Result<String> {
        let prompt = push::commit_message_prompt(context);
        self.acp_only(&self.default_cwd, &prompt)
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
        let adapter = AcpAgentAdapter::new(&cwd, "test");
        let out = adapter
            .acp_only(&cwd, "Скажи рівно одне слово: тест")
            .expect("хоча б один ACP-агент має відповісти");
        assert!(!out.trim().is_empty());
        println!("ACP response: {out:?}");
    }

    /// Ручна перевірка: потребує запущеного локального OpenAI-сумісного сервера
    /// й `N_LOCAL_MODEL=omlx/<model-id>` (чи `local-openai/<model-id>`) в env.
    /// `#[ignore]` з тієї самої причини. Реально пройдено з
    /// [TurboFieldfare](https://github.com/drumih/turbo-fieldfare) (`--port 8080`,
    /// модель `gemma-4-26b-a4b-it`):
    /// `N_LOCAL_OPENAI_BASE_URL=http://127.0.0.1:8080/v1/
    /// N_LOCAL_MODEL=omlx/gemma-4-26b-a4b-it cargo test --features agents
    /// acp_agents::tests::local_only_returns_text_when_configured -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn local_only_returns_text_when_configured() {
        let cwd = std::env::current_dir().unwrap();
        let adapter = AcpAgentAdapter::new(&cwd, "test");
        let out = adapter.local_only("Скажи рівно одне слово: тест");
        println!("local response: {out:?}");
        assert!(out.is_ok());
    }

    /// Реальний сценарій цієї машини: `N_LOCAL_MODEL=omlx/...` заданий, але
    /// локальний сервер не запущений (`127.0.0.1:8000` connection refused).
    /// Дві різні гарантії в одному тесті:
    /// - [`AcpAgentAdapter::local_only`] (ConflictResolver, `LocalOnly`) має
    ///   впасти НАЗВАНОЮ помилкою — жодного тихого фолбеку на ACP
    ///   (доккоментар модуля, рішення Ц);
    /// - [`AcpAgentAdapter::local_then_acp`] (ch::MessageGenerator, `AllowAcp`)
    ///   мусить мовчки провалити локальний рунг і впасти на ACP-каскад.
    #[test]
    #[ignore]
    fn falls_back_to_acp_when_local_configured_but_unreachable() {
        let cwd = std::env::current_dir().unwrap();
        let adapter = AcpAgentAdapter::new(&cwd, "test");
        assert!(
            adapter.local_only("Скажи рівно одне слово: тест").is_err(),
            "LocalOnly має впасти названою помилкою, коли сервер не запущений"
        );
        let out = adapter
            .local_then_acp(&cwd, "Скажи рівно одне слово: тест")
            .expect("AllowAcp має впасти на ACP-каскад і отримати відповідь");
        assert!(!out.trim().is_empty());
        println!("fallback response: {out:?}");
    }
}
