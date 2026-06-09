//! Build a JSON request body interactively from any JSON Schema file.
//!
//! Run:
//!     cargo run --example tui -- tests/fixtures/disk_create.json
//!     cargo run --example tui -- path/to/schema.json
//!     cargo run --example tui -- -            # read the schema from stdin
//!
//! On quit (`q`) the body you shaped is printed to stdout; cancel (`Esc`)
//! prints nothing. This is the generic counterpart to the `oxjson` binary,
//! which sources its schema from an OpenAPI operation instead of a file.

use std::path::Path;

fn main() -> anyhow::Result<()> {
    let arg = std::env::args().nth(1).unwrap_or_else(|| "-".to_string());
    let schema = schema_tui::load_schema_from_file(Path::new(&arg))?;
    let outcome = schema_tui::run_tui(schema, arg)?;
    schema_tui::print_outcome(outcome)
}
