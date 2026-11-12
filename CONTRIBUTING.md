# Contributing to jawnscanner

Thanks for taking an interest. jawnscanner is a small personal project, and contributions are welcome.

## Open an issue first

**Please open an issue before you open a pull request.** This applies to bug fixes, features, and refactors alike.

I built this for my own use, so some changes will be out of scope for reasons you have no way to know in advance. A short conversation up front saves you from writing code that gets turned down. Describe the problem you're trying to solve, not just the solution you have in mind.

Once we've agreed on the shape of a change in the issue, open the pull request and link it.

## Development setup

See [Development](README.md#development) in the README to get the project building locally.

## Before you push

Run the full gate locally. CI runs the same checks, and it's faster to fix them on your machine than to iterate through a red build.

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

Notes on the gate:

- Clippy runs with `-D warnings`. A warning fails the build.

## Pull request expectations

- **Link the issue** it resolves (`Closes #123`).
- **One logical change per PR.** A bug fix and a refactor in the same PR are hard to review and hard to revert.
- **Use conventional-commit style** (`feat:`, `fix:`, `chore:`, `docs:`) for the PR title. PRs are squash-merged, so the title becomes the commit message on `main`.
- **Update the docs** when you change behavior, so the README never disagrees with the code.
- **Don't commit secrets.** No API keys, tokens, `.env` files, or data files. Check `git diff --cached` before you commit.

## Reporting a security issue

Please **don't** post vulnerability details in a public issue. Open an issue that says only that you have a security report, and I'll arrange a private channel for the details.

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE) that covers this project.
