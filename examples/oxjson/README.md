# oxjson

Example binary that wires the two library crates together over a real OpenAPI
spec. It demonstrates what `oxide --json-body-schema` and
`--json-body-template` could become.

- `--json-body-schema` prints a grammar reference for an operation's request
  body, using [`schema-doc`](../../schema-doc).
- `--json-body-template` opens an interactive builder for that body, using
  [`schema-tui`](../../schema-tui).

Exactly one of the two flags is required.

## Run

From the workspace root:

```bash
cargo run -p oxjson -- disk create --json-body-schema
cargo run -p oxjson -- disk create --json-body-template > body.json
```

The command words are joined with `_` to form an `operationId` (`disk create`
becomes `disk_create`) and looked up in the spec. The spec is located in this
order:

1. `--spec <path>`
2. `$OXIDE_JSON`
3. the first existing of `./oxide.json`, `../oxide.json`,
   `../oxide.rs/oxide.json`, `../../oxide.rs/oxide.json`,
   `../../../oxide.rs/oxide.json`

## Template mode

The TUI draws on `/dev/tty` (stderr if unavailable) so stdout stays clean for
the JSON; redirect stdout to capture the body you built. Keys and visual
conventions are documented in the [`schema-tui` README](../../schema-tui/README.md).

The exported JSON contains every required field, every optional field you
included, and the selected variant of every `oneOf`, all with placeholder
values from `schema_doc::generate_value`. Excluded optional fields and
unselected variants are omitted.

End-to-end with the Oxide CLI:

```bash
cargo run -p oxjson -- disk create --json-body-template > /tmp/body.json
oxide disk create --project myproj --json-body /tmp/body.json
```

To try the builder on a plain JSON Schema file without an OpenAPI spec, use
the `schema-tui` example instead:

```bash
cargo run -p schema-tui --example tui -- schema-tui/tests/fixtures/disk_create.json
```

## OpenAPI to schemars

OpenAPI 3.0 refs use `#/components/schemas/X`; schemars uses
`#/definitions/X`. `oxjson` rewrites the refs and stitches `components.schemas`
into the `RootSchema`'s `definitions` map with plain `serde_json::Value`
traversal, no external OpenAPI crate. This glue is the only OpenAPI-aware code
in the workspace; the library crates are schema-generic.

## Status

Demo-grade. Not implemented: per-field value editing, search, undo,
jump-to-definition, and fuzzy command matching (the command path must match an
exact `operationId`).
