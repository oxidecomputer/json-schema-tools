# json-schema-tools

A Cargo workspace of tools for rendering and interactively filling in JSON
Schemas (`schemars::schema::RootSchema`). The schemas can come from anywhere:
an OpenAPI spec, progenitor, or a `#[derive(JsonSchema)]` type.

## Crates

| Crate | Kind | Purpose |
| --- | --- | --- |
| [`schema-doc`](schema-doc/) | library | Renders a schema as a BNF-style grammar reference and generates placeholder JSON values. |
| [`schema-tui`](schema-tui/) | library | Interactive accordion TUI (ratatui) for building a JSON body from a schema. Uses `schema-doc` for placeholder values. |
| [`oxjson`](examples/oxjson/) | example binary | Runs both crates against a real OpenAPI spec, selecting an operation by its CLI command path. |

`schema-doc` has no TUI dependencies and `schema-tui` knows nothing about
OpenAPI; the OpenAPI glue lives entirely in `oxjson`. Because both the grammar
reference and the TUI share `schema-doc`'s traversal helpers, they interpret a
schema identically.

## Quick start

```bash
cargo test

# Grammar reference for an Oxide operation's request body:
cargo run -p oxjson -- disk create --json-body-schema

# Interactive builder, exports JSON on quit:
cargo run -p oxjson -- disk create --json-body-template > body.json

# The TUI on a plain JSON Schema file, no OpenAPI spec needed:
cargo run -p schema-tui --example tui -- schema-tui/tests/fixtures/disk_create.json
```

`oxjson` needs an OpenAPI spec; see its [README](examples/oxjson/README.md)
for how one is located. Each crate's README covers usage in detail.
