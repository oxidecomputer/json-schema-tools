# oxjson — example harness

`oxjson` is an example binary (a workspace member under `examples/`) that wires
the two library crates in this workspace together over a real OpenAPI spec:

- **[`schema-doc`](../../schema-doc)** — renders a request body's **grammar
  reference** (`--json-body-schema`).
- **[`schema-tui`](../../schema-tui)** — an interactive accordion **builder**
  for a request body (`--json-body-template`).

It's the end-to-end demonstrator of what `oxide --json-body-template` /
`--json-body-schema` could become: pick an operation by its CLI command path,
then either read its body grammar or build a tailored body visually.

## Run

From the workspace root (`json-schema-tools/`):

```bash
# Grammar reference for an operation's request body:
cargo run -p oxjson -- disk create --json-body-schema

# Interactive builder — shape a body, `q` to emit it on stdout:
cargo run -p oxjson -- disk create --json-body-template > body.json
cargo run -p oxjson -- instance create --json-body-template > instance.json
```

Exactly one of `--json-body-schema` or `--json-body-template` is required.

`oxjson <command…>` joins the words with `_` (and maps `-` → `_`) to form an
`operationId` and looks it up in the spec — the same path you'd type after
`oxide` itself: `oxide disk create` → `oxjson disk create …`.

### Spec resolution

The OpenAPI spec is located in this order:

1. `--spec <path>`
2. `$OXIDE_JSON`
3. the first existing of `./oxide.json`, `../oxide.json`,
   `../oxide.rs/oxide.json`, `../../oxide.rs/oxide.json`,
   `../../../oxide.rs/oxide.json`

## The two modes

| Flag | Crate | Output |
| --- | --- | --- |
| `--json-body-schema` | `schema_doc::render_body_schema` | BNF-style grammar reference to stdout. Same renderer `oxide` ships; auto-colors on a TTY, plain when piped. |
| `--json-body-template` | `schema_tui::run_tui` | Interactive TUI. Pick `oneOf` variants, toggle optional fields, then `q` to emit JSON on stdout (`Esc` cancels, no output). |

The TUI draws on `/dev/tty` (falling back to stderr) so stdout stays clean for
the resulting JSON — redirect stdout to capture the body you built.

### Keys (template mode)

These belong to `schema-tui`; `oxjson` just hands it the operation's schema.

| Key | Action |
| --- | --- |
| `↑` `↓` `k` `j` | Move cursor |
| `Enter` or `→` | Expand a ref / pick a `oneOf` variant |
| `space` | Toggle whether an optional field is included in the output |
| `←` | Collapse the current node, or step up |
| `g` / `G` | Jump to top / bottom |
| `PgUp` `PgDn` | Page navigation |
| `q` | **Quit and export the JSON to stdout** |
| `Esc` / Ctrl-C | Cancel (no output) |

### Visual conventions

| Symbol | Meaning |
| --- | --- |
| `▶` `▼` | Expandable, collapsed / expanded |
| `+` | Optional field, currently included in output |
| (nothing) | Optional field, currently excluded |
| `●` (green) | Selected `oneOf` variant (this one contributes to JSON) |
| `○` | Unselected `oneOf` variant |
| **bold** | Required field |
| dim | Optional or excluded |
| cyan | `<TypeRef>` — expandable named type |
| green | `"literal"` — discriminator value |

## What's in the exported JSON

When you quit with `q`, the printed JSON contains:

- **Every required field**, with a placeholder value (schema defaults when
  available, otherwise empty-string / `0` / `false` / first enum value).
- **Every optional field you included** (`+` marker), with the same kind of
  placeholder.
- **The selected variant** of every `oneOf`, including nested ones you drilled
  into — and including the optional ancestors along the path.

Optional fields you didn't include and unselected variant branches are omitted.
Placeholder values come from `schema_doc::generate_value`, so the interactive
builder's default export agrees field-for-field with the non-interactive
template.

## End-to-end Oxide demo

```bash
cargo run -p oxjson -- disk create --json-body-template > /tmp/body.json
# in the TUI: drill into disk_backend → pick "distributed" → drill into
# disk_source → pick "image" → q
oxide disk create --project myproj --json-body /tmp/body.json
```

Same end result — a tailored body — but built visually instead of guessed.

## How `oxjson` translates OpenAPI → schemars

`oxide.json` is OpenAPI 3.0; its schemas use `#/components/schemas/X` for refs,
while `schemars` uses `#/definitions/X`. `oxjson` recursively rewrites the refs
and stitches `components.schemas` into the `RootSchema`'s `definitions` map. No
external OpenAPI crate — just `serde_json::Value` traversal, small and
self-contained. This glue is the only OpenAPI-aware code; the library crates are
schema-generic and know nothing about OpenAPI.

## Try the builder without a spec

`schema-tui` ships a file-based example that runs the same TUI on any JSON
Schema file — no OpenAPI spec needed:

```bash
cd ../../schema-tui
cargo run --example tui -- tests/fixtures/disk_create.json
```

## Workspace layout

```
json-schema-tools/
├── Cargo.toml                       # workspace: schema-doc, schema-tui, examples/oxjson
├── schema-doc/                      # lib: grammar renderer + value/template generator
├── schema-tui/                      # lib: interactive accordion builder
│   ├── examples/tui.rs              #   file-based driver (`cargo run --example tui`)
│   └── tests/fixtures/
│       └── disk_create.json         #   sample schema / test fixture
└── examples/
    └── oxjson/                      # this crate — the demo binary
        └── src/main.rs              #   OpenAPI → schemars glue + arg parsing
```

## Status

Demo-grade. The builder handles nested `oneOf`s, optional fields, scalar/array
placeholders, and OpenAPI 3.0 → schemars conversion for the Oxide spec. Not yet:

- Per-field placeholder editing inside the TUI
- Search / filter
- Side-by-side rendered grammar pane
- Multi-step undo
- Jump-to-definition for `<TypeRef>`s
- Fuzzy command-path matching (today `oxjson disk create` must match an exact
  `operationId`)

## Where this might land

The natural endpoint is for `oxide --json-body-template` (and any other
progenitor-generated CLI) to launch this when stdout is a TTY, and keep the
current minimal-template behavior when piped non-interactively. That's a
separate, larger change; this crate is the demonstrator.
