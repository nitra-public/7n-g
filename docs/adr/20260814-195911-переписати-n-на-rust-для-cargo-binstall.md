---
type: ADR
title: "Переписати @7n/n на Rust для дистрибуції через cargo-binstall"
---

# Переписати @7n/n на Rust для дистрибуції через cargo-binstall

**Status:** Accepted
**Date:** 2026-08-14

## Context and Problem Statement

`@7n/n` — CLI-утиліта (Bun/Node.js, ~2500 рядків) з командами `getw`/`pull`/`push`/`ch`, які
переносять git-дельти між worktree/гілками з багаторівневим авторезолвом конфліктів
(git apply → git merge-file --diff3 → Mergiraf → LLM-агент). Зараз важко шелиться на
`git`, `zsh`, `fzf`, `mergiraf`-бінарник і CLI LLM-агентів (`pi -p` / `claude -p` /
`cursor-agent -p`), дистрибутується через npm (`npx @7n/n`).

Мотивація переходу — саме бажання писати на Rust (не побічний ефект гонитви за
швидкістю встановлення), з прицілом отримати дистрибуцію одною командою
(`cargo binstall n`) на різних машинах без компіляції на кожній.

Обмеження сесії:
- Готовність на full-rewrite з нуля (без поетапної гібридності).
- Репозиторій переїжджає на `git.7n.ai/7n/n`, CI — Forgejo Actions на власних runners
  (не GitHub).
- В екосистемі вже є готовий Rust-крейт `llm-lib` (nitra/7n-rules) з ACP-шаром
  (`agent-client-protocol` crate, `acp.rs`) для виклику Codex/Cursor/Claude —
  не писати LLM-агентну частину з нуля.
- Не винаходити власний self-update — покладатись на механізм самого `cargo-binstall`.
- Не робити окремий `n-core` crate — потрібна одна повна crate-реалізація, яку можна
  викликати і як CLI, і підключати як library в інші Rust-проєкти.
- WASM-таргет не потрібен.

## Considered Options

- **Архітектура крейта**: один crate `n` (`lib.rs` + `src/bin/n.rs`, feature-flags за
  зразком `llm-lib`) — CLI і library в одному пакеті.
- **Git/merge-рушій**: `gitoxide` для операцій, що покриваються нативно (diff,
  merge-base, читання рефів); shell-`git` лишається для нетривіальних кейсів
  (`git stash create`, worktree) до окремої перевірки покриття API.
- **LLM-агентний шар**: ACP (`agent-client-protocol` crate) замість CLI-spawn,
  перевикористання/залежність від наявного `llm-lib`.
- **Дистрибуція/CI**: Forgejo Actions + Forgejo Releases як джерело артефактів для
  `cargo-binstall` (кастомний pkg-url), без npm-shim перехідного періоду.
- **Homebrew tap**: паралельний канал дистрибуції — окрема Forgejo Actions-джоба після
  релізу оновлює формулу в [git.7n.ai/7n/homebrew](https://git.7n.ai/7n/homebrew)
  (checksums з release-артефактів → перезапис `Formula/n.rb` → commit+push через PAT),
  за готовим патерном `update-homebrew-tap` з `mt-rust/.github/workflows/release-mt.yml`
  (там tap — `nitra/homebrew-7n` на GitHub; для `n` той самий підхід, інший host і repo:
  `7n/homebrew` на Forgejo).
- **Інтерактивний UX**: нативний TUI fuzzy-picker (`skim`/`nucleo`) замість зовнішньої
  залежності від `fzf`.

## Decision Outcome

Chosen option: повний rewrite `@7n/n` на Rust у складі одного crate `n` (lib+bin),
з gitoxide поетапно (не суцільна заміна git-викликів одразу), ACP+`llm-lib` для
LLM-агентної частини, Forgejo Actions/Releases для CI та дистрибуції, плюс автопублікація
у tap `7n/homebrew` (Forgejo) за патерном `mt-rust`, без npm-shim — тому що: (1)
мотивація — сам Rust, а не проміжні компроміси; (2) `llm-lib`/ACP уже готові й
перевірені, тож найризикованіша частина (LLM-агенти) де-ризикована наперед; (3) єдиний
crate спрощує і CLI-, і library-використання без зайвої workspace-структури; (4)
Homebrew tap — вже перевірений в екосистемі механізм (`mt`), не новий винахід.

### Consequences

- Good, because дистрибуція стає `cargo binstall n` — без npm/Node/Bun рантайму,
  крос-платформно (включно з Windows нативно, без zsh).
- Good, because LLM-агентна частина не пишеться з нуля — `llm-lib`/ACP вже існує і
  перевірена в іншому проєкті екосистеми.
- Good, because Homebrew-дистрибуція теж не винаходиться заново — той самий tap і той
  самий CI-патерн (build matrix → GitHub/Forgejo Release → update-tap job), що вже
  працює для `mt`.
- Bad, because найризикованіший відкритий момент сесії — чи `cargo-binstall` коректно
  резолвить артефакти з Forgejo-хостингу (не GitHub) без додаткового налаштування
  pkg-url; це варто перевірити до старту реалізації, інакше під питанням сама мотивація
  переходу.
- Bad, because `gitoxide` не гарантує 1:1 паритет з `git`-CLI на всі використовувані
  операції (`git stash create` для pre-flight бекапу, worktree-менеджмент) — частина
  git-логіки лишиться на shell-виклики до окремого дослідження покриття.

## More Information

Усі ідеї сесії (сирий список SCAMPER, 51 позиція):

**Substitute:** 1. gitoxide замість child_process git · 2. без zsh-рантайму · 3. нативний
TUI-picker замість fzf · 4. owo-colors/colored · 5. cargo test + insta замість vitest ·
6. clap замість ручного parsing · 7. tokio::process для LLM-агентів (згодом замінено на
ACP, #46) · 8. cargo + cargo-binstall + crates.io замість npm.

**Combine:** 9. спільне ядро як окрема lib-crate `n-core` (знято рішенням користувача,
див. #51) · 10. cargo-dist для build+release · 11. cargo workspace співіснує з рештою
bun-монорепо · 12. gitoxide + власний merge-engine без shell git.

**Adapt:** 13. `[package.metadata.binstall]` з першого дня · 14. перенести ADR/specs-workflow
без змін · 15. розширити n-lint/n-doc-files на Rust (lang-rust плагін) · 16. README →
rustdoc.

**Modify/Magnify:** 17. власний `n self-update` (знято рішенням користувача, див. #50) ·
18. статичний musl-бінарник · 19. `similar` crate для diff3-тіру · 20. миттєвий cold start.

**Put to other use:** 21. n-core як бібліотека для інших тулів (злито з #51) · 22. GitHub
Action на скомпільованому бінарнику · 23. WASM-таргет (знято рішенням користувача — не
потрібен).

**Eliminate:** 24. без npm-залежності · 25. без zsh · 26. без fzf-бінарника · 27. без
subprocess LLM CLI — прямий HTTP (замінено рішенням користувача на ACP, #46) · 28.
спрощення bun.lock-спецкейсу.

**Reverse/Rearrange:** 29. curl\|sh інсталер через cargo binstall · 30. один бінарник з
subcommands (clap) · 31. опційні shell-hooks як plugin-система.

**Дистрибуція/тулінг:** 32. GH Actions matrix (замінено на Forgejo, #48) · 33.
sigstore/cosign підписування релізів · 34. npm-shim перехідного періоду (відхилено
рішенням користувача) · 35. перевірка доступності імені `n` на crates.io · 36.
`cargo install n --locked` як fallback · 37. Homebrew tap (прийнято в рішення — див.
"Дистрибуція/CI" вище, 7n/homebrew на Forgejo за патерном mt-rust) · 38.
duct/Command-builder ·
39. serde+toml для конфігів · 40. tracing crate замість ручних print · 41. Mergiraf як
library dependency замість спавну бінарника · 42. глобальний `--dry-run` через clap ·
43. workspace-структура n-core/n-cli/n-git (відхилено рішенням користувача, див. #51) ·
44. assert_cmd+insta+proptest для тестів · 45. feature-flags для опційності Mergiraf.

**Додано в ході сесії (уточнення користувача):** 46. ACP (`agent-client-protocol` crate)
замість spawn CLI-агентів · 47. перевикористання наявного `llm-lib` (nitra/7n-rules) ·
48. Forgejo Actions замість GH Actions · 49. Forgejo Releases як джерело артефактів для
binstall pkg-url · 50. без власного self-update, покладаємось на механізм cargo-binstall ·
51. один crate `n` (lib+bin) без окремого `n-core`.

Відкладені кластери: E (заміна fzf на нативний TUI) і G (стандартний Rust-тулінг:
clap/serde/tracing/тести) — не предмет дискусії, увійдуть у деталізацію реалізації без
окремого рішення.

Відкриті питання:
- Чи `cargo-binstall` коректно резолвить артефакти з Forgejo-хостингу без додаткового
  pkg-url-налаштування — потребує технічної перевірки до старту реалізації.
- Покриття `gitoxide` для `git stash create` і worktree-операцій — які саме частини
  git-логіки лишаються на shell-виклики.
- Доступність імені `n` на crates.io.
