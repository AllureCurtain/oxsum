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

## Issue labels

`.github/workflows/label-issues.yml` runs when an issue is opened, edited, or reopened. It only adds labels. A label someone applied by hand stays, even if the title later stops matching.

The title is matched at the start, case-insensitive, with an optional scope such as `feat(web):`.

| Title prefix | Label added |
| --- | --- |
| `fix:` or `bug:` | `bug` |
| `docs:` | `documentation` |
| `feat:` | `enhancement` |
| `Security:` | `security` |
| `test:` or `chore:` | none by themselves |

`test:` and `chore:` have no matching label in this repository, so they are left for a person. `ci:`, `build:`, `refactor:`, and `perf:` are also left alone.

Any title that contains an acceptance id such as `N21`, a reference to `#725`, or the words `operator launch` / `operator-launch` also gets `operator-launch`. That stacks with the prefix label, so `docs: pilots (N19)` receives both `documentation` and `operator-launch`.

Pull requests are not labeled by this workflow.

## Reports

- Bugs and features use the issue templates.
- Vulnerabilities follow [SECURITY.md](SECURITY.md). Do not put secrets or exploits in a public issue.
- Operator support is [SUPPORT.md](SUPPORT.md).
