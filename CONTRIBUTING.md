# Contributing

## Workflow

Direct pushes to `main` are blocked. Branch from `main`, open a pull request,
and merge when checks pass.

Before opening a PR:

```bash
cargo fmt
cargo clippy --all-targets
cargo test
```

New behavior should come with a test. Bug fixes should include a regression
test that fails without the fix.

## Keep the library crates generic

`schema-doc` and `schema-tui` operate on plain `schemars` schemas and must not
contain anything Oxide-specific. Do not add:

- Dependencies on progenitor, oxide.rs, or other Oxide crates
- Hardcoded field names, type names, or operation IDs from the Oxide API
- OpenAPI parsing or any assumption about where a schema came from

If a change only makes sense for the Oxide spec, it belongs in
`examples/oxjson`, which holds all of the OpenAPI and Oxide-specific glue. If
a schema from the Oxide spec renders poorly, fix the general case: handle the
JSON Schema construct, not the specific type.

## Style

- Generic schema traversal helpers live in `schema-doc` and are shared by both
  crates; extend those rather than duplicating traversal logic
- Keep READMEs and doc comments brief and factual
- Keep commit messages to a short subject line
