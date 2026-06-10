# schema-tui

An interactive accordion TUI (ratatui + crossterm) for building JSON request bodies from a JSON Schema. You browse a `schemars` schema as a collapsible tree, expand the optional fields you want, pick `oneOf`/`anyOf` variants, and quit. The body you shaped is returned as a `serde_json::Value`.

Placeholder values come from the [`schema-doc`](../schema-doc) crate, so the interactive builder and the non-interactive template generator agree field for field.

The TUI renders to `/dev/tty` (falling back to stderr), keeping stdout free for the JSON output, and it restores the terminal on panic.

## Usage

```rust
use schema_tui::{load_schema_from_file, print_outcome, run_tui, Outcome};

let schema = load_schema_from_file(std::path::Path::new("schema.json"))?; // "-" reads stdin
let outcome = run_tui(schema, "disk create".to_string())?;

match outcome {
    Outcome::Export(json) => {
        // The body the user built, as serde_json::Value.
        println!("{}", serde_json::to_string_pretty(&json)?);
    }
    Outcome::Cancel => {}
}
```

`print_outcome` is a convenience that writes the exported body as pretty JSON to stdout (and nothing on cancel).

## Keys

| Key | Action |
| --- | --- |
| `Up`/`Down` or `j`/`k` | Move selection (`PageUp`/`PageDown` jump by 10) |
| `g` / `G` | Jump to top / bottom |
| `Enter` or `Right` | Expand a node or pick a variant |
| `Space` | Include or exclude an optional field |
| `Left` | Collapse, or move to the parent node |
| `q` or `Esc` | Quit and export the built body |
| `Ctrl-C` | Cancel without exporting |
