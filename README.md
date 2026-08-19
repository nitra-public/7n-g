# g

Rust-переписання `@7n/n` (Bun/JS CLI з монорепо `7n`) — git-дельта інструмент
(`getw`/`pull`/`push`/`ch`), дистрибутований через `cargo binstall` замість npm.
Бінарник — `g` (скорочено від git); package/crate — `n7n-g`; модульний шлях —
`n7n_g::...` (`[lib] name = "n7n_g"`); репозиторій —
[git.7n.ai/7n/g](https://git.7n.ai/7n/g).

Тонкі read-only `gix`-хелпери (`show_toplevel`, `check-ignore`, `diff --name-status`
двох дерев), спільні з [`n7n-llm-lib`](https://git.7n.ai/7n/llm-lib)/`n7n-harness`,
винесені звідси в окремий крейт `n7n-gix-util` (опублікований на crates.io) — `g`
лишається єдиним споживачем із CLI/TUI-залежностями (`clap`/`crossterm`).

Рішення й обґрунтування: [`docs/adr/20260814-195911-переписати-n-на-rust-для-cargo-binstall.md`](docs/adr/20260814-195911-переписати-n-на-rust-для-cargo-binstall.md).

## Статус

`getw`/`push`/`ch` поки повертають `NotPorted` (логіка з JS-оригіналу ще не
портована). `pull` — портовано й протестовано (fast-forward + reverse-delta через
спільне merge-ядро). Дивись `src/*.rs` — кожен модуль посилається на відповідний
`npm/src/*.js`-файл монорепо `7n` як джерело портування.

## Що всередині

- **`getw`** — перенесення дельти з worktree у поточну гілку.
- **`pull`** — накочування дельти `origin/<гілка>` у поточне робоче дерево
  (fast-forward, за потреби — reverse-delta через спільне merge-ядро).
- **`push`** — сквош локальних змін в один коміт і push.
- **`ch`** — тонка обгортка над `nitra-cursor change`.

## Встановлення

```bash
cargo binstall n7n-g   # ставить бінарник `g`
# або
brew install 7n/homebrew/g
```

## Приклад

```bash
g pull            # накотити дельту origin/<поточна гілка>
g pull main       # накотити дельту конкретної гілки
```

## MSRV

Формально не зафіксований (`rust-version` у `Cargo.toml` відсутній) — edition 2021,
тож мінімум мовою Rust 1.56. CI компілює на поточному stable.

## Розробка

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
```
