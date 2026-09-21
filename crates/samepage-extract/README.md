# samepage-extract

Finds every execution lane a codebase actually has, straight from source, so
a diagram of that codebase can't quietly draw one entry point when the code
has two. The failure this crate exists to prevent: an agent, or a person,
draws a single box and a single arrow for a service that also binds a
second socket, spawns a sidecar process, or starts a background task that
outlives the request that triggered it. Every lane this crate reports comes
with a file, a line, and that line's own text, never a description someone
gave it out of band.

First pass: Rust and JavaScript/TypeScript, using gitignore-aware walking
plus regex and a small amount of brace-depth analysis. No `tree-sitter`, no
real parser, no dynamic analysis.

## Usage

```sh
cargo run -p samepage-extract -- /path/to/project
```

Prints the [`Report`](src/lib.rs) as pretty JSON on stdout: every `Lane`
found, how many files were scanned, what was skipped and why, and which
languages were recognized.

## What a lane kind means

- **Binary (E1).** A program the build actually produces: a Cargo package
  with an explicit `[[bin]]` target, an implicit default binary via
  `src/main.rs`, or a `src/bin/*.rs` file; on the JS side, `package.json`'s
  `bin`, `main`, or `scripts.start`. Manifest-declared, so this is the
  highest-confidence lane kind — it does not depend on reading the file's
  actual code.
- **Listener (E2).** A socket bind: `TcpListener::bind`, `UdpSocket::bind`,
  `axum::Server::bind`, a bare `serve(` call, a `.bind(` on something
  listener- or socket-named; on the JS side, `.listen(`, `createServer(`,
  `Bun.serve(`, `Deno.serve(`. This is the lane a single-entry-point diagram
  most often misses: a second listener nobody mentioned.
- **Spawn (E3).** A child process: `Command::new`, `tokio::process::Command`,
  `std::process::Command`, `.spawn()` on a `Command`; on the JS side,
  `child_process`, `spawn(`, `exec(`, `execFile(`, `fork(`, `Bun.spawn(`,
  `Worker(`. A bare import of `std::process::Command` or `child_process`
  counts on its own, the same way it would for a reviewer scanning the file
  by eye.
- **Outbound (E4).** A network call this codebase makes to somewhere else:
  an HTTP client constructed or called on that line (`reqwest::Client`,
  `Client::builder()`, `Client::new().get(`), a raw `TcpStream::connect`,
  `ureq::`, `hyper::Client`; on the JS side, `fetch(` to a non-relative URL
  or a variable, `WebSocket(`, `http.request(`, `axios`. Evidence is the line
  itself. A bare `.get(` or `.post(` is never a lane, since a map lookup or
  an axum route registration looks the same, and a type mention such as
  `reqwest::Url` in a signature is not a lane either.

## What static extraction misses

This is a source-text scan, not a running system. It cannot see:

- A process spawned from a value read out of a config file, an environment
  variable, or a database row at runtime, rather than a literal in the
  source.
- Anything reached through reflection, dynamic dispatch behind a trait
  object with no textual call site, or a plugin loaded by name at runtime.
- Code generated at build time (a build script, a macro that expands into a
  `Command::new(...)` the source never spells out, codegen from a schema).
- Any of the above changing which lane is real between builds or
  environments — the scan reports what the source says, not what a given
  deployment will do.

## Tests

```sh
cargo test -p samepage-extract
```

Includes a fixture tree under `fixtures/` (a two-member Rust workspace plus
a JS package) with an exact expected lane set, a named regression test for
the sidecar-the-diagram-never-drew failure, a check that comment and
`#[cfg(test)]` matches never become lanes, and a self-scan of this
repository that asserts the scanner finds the `same-page-room` binary, the
`TcpListener::bind` in `crates/ag-ui-surface/src/lib.rs`, and the process
spawns in `crates/ag-ui-surface/src/auth.rs` and
`crates/ag-ui-surface/src/turn_loop/pi.rs`.
