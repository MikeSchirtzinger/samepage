# Contributing

Open an issue before a large change, so we can agree on the shape first.

A pull request is ready when:

- CI passes. It runs formatting, clippy for native and wasm32, the tests, the
  WASM equivalence checks, the full recipe build and `cargo deny`. The commands
  are in `.github/workflows/ci.yml` and run the same way locally.
- A change people can see in the page was checked in a real browser against
  `apps/samepage/recipes/full.sh`, and the pull request says what you looked at.
- A change users notice has a line under Unreleased in `CHANGELOG.md`.

By contributing you agree that your work is released under the MIT license in
`LICENSE`.
