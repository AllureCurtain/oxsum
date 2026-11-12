# Contributing

oxsum is issue-driven. Open or pick an issue before writing code. One pull request does one thing. Discussion stays on the issue. `TODO.md` is only an index of open issues: add or remove a row, and do not paste a status log.

## Before you change code

- Read [docs/development.md](docs/development.md) for the toolchain, commands, and how tests are scoped.
- Use the documentation list in [README.md](README.md) to find the page that owns the behavior you are changing.
- Product behavior, architecture, and decisions are updated in the same pull request as the code. The rules are in [AGENTS.md](AGENTS.md).
- Do not commit `.env` or any real credential. New environment variables go in `.env.example` in the same change.
- Documentation, code comments, and commit messages are English. `README.zh-CN.md` is the only standing exception.

## Branches and commits

Branch names match `^(feat|fix|docs|refactor|test|chore|build|ci|perf)/[a-z0-9]+(-[a-z0-9]+)*$`. The description states the outcome. It does not start with an issue number.

Format Rust with `scripts/fmt.sh`, not bare `cargo fmt`. The check is `scripts/fmt-check.sh`.

## Reports

- Bugs and features use the issue templates.
- Vulnerabilities follow [SECURITY.md](SECURITY.md). Do not put secrets or exploits in a public issue.
- Operator support is [SUPPORT.md](SUPPORT.md).
