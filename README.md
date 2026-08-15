# g

Rust-переписання `@7n/n` (Bun/JS CLI з монорепо `7n`) — git-дельта інструмент
(`getw`/`pull`/`push`/`ch`), дистрибутований через `cargo binstall` замість npm.
Бінарник — `g` (скорочено від git); package/crate — `n7n-g`; репозиторій —
[git.7n.ai/7n/g](https://git.7n.ai/7n/g).

Рішення й обґрунтування: [`docs/adr/20260814-195911-переписати-n-на-rust-для-cargo-binstall.md`](docs/adr/20260814-195911-переписати-n-на-rust-для-cargo-binstall.md).

## Статус

`getw`/`push`/`ch` поки повертають `NotPorted` (логіка з JS-оригіналу ще не
портована). `pull` — портовано й протестовано (fast-forward + reverse-delta через
спільне merge-ядро). Дивись `src/*.rs` — кожен модуль посилається на відповідний
`npm/src/*.js`-файл монорепо `7n` як джерело портування.

## Встановлення

```bash
cargo binstall n7n-g   # ставить бінарник `g`
# або
brew install 7n/homebrew/g
```

## Розробка

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
```
