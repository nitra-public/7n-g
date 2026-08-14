# n

Rust-переписання `@7n/n` (Bun/JS CLI з монорепо `7n`) — git-дельта інструмент
(`getw`/`pull`/`push`/`ch`), дистрибутований через `cargo binstall` замість npm.

Рішення й обґрунтування: [`docs/adr/20260814-195911-переписати-n-на-rust-для-cargo-binstall.md`](docs/adr/20260814-195911-переписати-n-на-rust-для-cargo-binstall.md).

## Статус

Скелет crate — CLI-каркас (`clap`) з підкомандами `getw`/`pull`/`push`/`ch`, кожна
поки повертає `NotPorted` (логіка мерджу з JS-оригіналу ще не портована). Дивись
`src/*.rs` — кожен модуль посилається на відповідний `npm/src/*.js`-файл монорепо
`7n` як джерело портування.

## Встановлення

```bash
cargo binstall n7n-git   # ставить бінарник `n`
# або
brew install 7n/homebrew/n
```

## Розробка

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
```
