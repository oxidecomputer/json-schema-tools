//! oxjson: a demo machine that shows what `--json-body-template` and
//! `--json-body-schema` could be if they were turbo-charged.
//!
//! - `--json-body-schema` prints the BNF-style grammar reference for the
//!   chosen operation's request body (via `schema_doc::render_body_schema`).
//!
//! - `--json-body-template` opens the interactive accordion TUI
//!   (`schema_tui`). Pick the `oneOf` variants you want, expand the optional
//!   fields you care about, then `q` to emit a tailored JSON body.
//!
//! Usage:
//!     oxjson disk create --json-body-schema
//!     oxjson disk create --json-body-template > body.json
//!     oxjson instance create --json-body-template > /tmp/instance.json

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use schemars::schema::RootSchema;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(
    name = "oxjson",
    about = "Demo: schema reference and interactive body builder over an OpenAPI spec.",
    long_about = "A demo that shows what '--json-body-template' and '--json-body-schema' \
                  could be if they were extended.\n\n\
                  --json-body-schema prints the BNF-style grammar reference for the \
                  selected operation's request body (same renderer as oxide ships).\n\n\
                  --json-body-template opens an interactive TUI. Pick oneOf variants, \
                  toggle optional fields, then 'q' to emit JSON on stdout."
)]
struct Args {
    /// OpenAPI spec file. Defaults to $OXIDE_JSON or a nearby oxide.json.
    #[arg(long, value_name = "FILE")]
    spec: Option<PathBuf>,

    /// Print the schema grammar reference for the operation and exit.
    #[arg(long, conflicts_with = "json_body_template")]
    json_body_schema: bool,

    /// Open the interactive body-builder TUI on the operation.
    #[arg(long)]
    json_body_template: bool,

    /// CLI command path, e.g. "disk create" or "instance create".
    #[arg(value_name = "COMMAND")]
    command: Vec<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if !args.json_body_schema && !args.json_body_template {
        bail!("specify one of --json-body-schema or --json-body-template");
    }

    let spec_path = match args.spec {
        Some(p) => p,
        None => locate_oxide_json().ok_or_else(|| {
            anyhow!(
                "could not find an OpenAPI spec. Pass --spec <path>, set OXIDE_JSON, \
                 or run from a directory near oxide.rs."
            )
        })?,
    };
    let openapi = load_openapi(&spec_path)
        .with_context(|| format!("loading OpenAPI spec at {}", spec_path.display()))?;

    if args.command.is_empty() {
        bail!("a COMMAND path is required (e.g. `oxjson disk create --json-body-template`)");
    }
    let (op_id, schema) = schema_from_openapi(&openapi, &args.command)?;

    if args.json_body_schema {
        // Identical output to `oxide … --json-body-schema`. The renderer
        // auto-detects TTY for colors; pipe to a file for plain text.
        print!("{}", schema_doc::render_body_schema(&schema));
        return Ok(());
    }

    // --json-body-template: interactive TUI builder.
    let title = format!("{}    [op: {}]", args.command.join(" "), op_id);
    let outcome = schema_tui::run_tui(schema, title)?;
    schema_tui::print_outcome(outcome)
}

// ---------------------------------------------------------------------------
// OpenAPI → schemars glue. This is oxjson-specific (pulling a request-body
// schema out of an OpenAPI 3.0 operation); the TUI itself (`schema_tui_core`)
// is schema-generic and knows nothing about OpenAPI.
// ---------------------------------------------------------------------------

/// Load an OpenAPI 3.0 spec (e.g. oxide.json) as a raw JSON Value.
fn load_openapi(path: &Path) -> Result<Value> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&content).context("parsing OpenAPI JSON")
}

/// Walk well-known locations to find oxide.json. Honors the `OXIDE_JSON` env
/// var first, then probes a few sensible relative paths.
fn locate_oxide_json() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("OXIDE_JSON") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    for candidate in [
        "./oxide.json",
        "../oxide.json",
        "../oxide.rs/oxide.json",
        "../../oxide.rs/oxide.json",
        "../../../oxide.rs/oxide.json",
    ] {
        let p = PathBuf::from(candidate);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Given an OpenAPI spec and a CLI-style command path (e.g.
/// `["disk", "create"]`), find the matching operation and return its
/// request-body schema rewritten as a schemars `RootSchema`.
fn schema_from_openapi(openapi: &Value, command_path: &[String]) -> Result<(String, RootSchema)> {
    let op_id = command_path.join("_").replace('-', "_");
    let op = find_operation(openapi, &op_id)
        .ok_or_else(|| anyhow!("no operation with operationId '{}' in spec", op_id))?;
    let body = extract_body_schema(op, openapi)
        .ok_or_else(|| anyhow!("operation '{}' has no JSON request body", op_id))?;
    Ok((op_id, body))
}

fn find_operation<'a>(openapi: &'a Value, op_id: &str) -> Option<&'a Value> {
    let paths = openapi.get("paths")?.as_object()?;
    for (_path, path_obj) in paths {
        let Some(path_obj) = path_obj.as_object() else {
            continue;
        };
        for (method, op) in path_obj {
            if !matches!(method.as_str(), "get" | "post" | "put" | "delete" | "patch") {
                continue;
            }
            if op.get("operationId").and_then(|v| v.as_str()) == Some(op_id) {
                return Some(op);
            }
        }
    }
    None
}

fn extract_body_schema(op: &Value, openapi: &Value) -> Option<RootSchema> {
    let body_schema = op
        .get("requestBody")?
        .get("content")?
        .get("application/json")?
        .get("schema")?;

    let components_schemas = openapi
        .get("components")
        .and_then(|c| c.get("schemas"))
        .cloned()
        .unwrap_or(Value::Object(Map::new()));

    let body_rewritten = rewrite_refs(body_schema.clone());
    let defs_rewritten = rewrite_refs(components_schemas);

    // Build a RootSchema by merging the body schema fields with a top-level
    // `definitions` map from components.schemas.
    let mut root = Map::new();
    if let Value::Object(body_obj) = body_rewritten {
        for (k, v) in body_obj {
            root.insert(k, v);
        }
    }
    if let Value::Object(defs_obj) = defs_rewritten {
        root.insert("definitions".to_string(), Value::Object(defs_obj));
    }
    serde_json::from_value(Value::Object(root)).ok()
}

/// Recursively rewrite `$ref` values from OpenAPI 3.0's
/// `#/components/schemas/X` to schemars' `#/definitions/X`.
fn rewrite_refs(v: Value) -> Value {
    match v {
        Value::Object(mut map) => {
            // Recurse into each value in place (preserves key order; no
            // remove/insert churn).
            for val in map.values_mut() {
                *val = rewrite_refs(std::mem::take(val));
            }
            if let Some(Value::String(ref_str)) = map.get_mut("$ref") {
                if let Some(rest) = ref_str.strip_prefix("#/components/schemas/") {
                    *ref_str = format!("#/definitions/{}", rest);
                }
            }
            Value::Object(map)
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(rewrite_refs).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // OpenAPI 3.0 refs (`#/components/schemas/X`) become schemars refs
    // (`#/definitions/X`) — recursively, through objects and arrays — while
    // plain strings that merely look like a ref path are left untouched.
    #[test]
    fn rewrite_refs_translates_component_refs() {
        let out = rewrite_refs(json!({
            "$ref": "#/components/schemas/Disk",
            "nested": { "items": { "$ref": "#/components/schemas/Name" } },
            "list": [ { "$ref": "#/components/schemas/Foo" } ],
            "not_a_ref": "#/components/schemas/StringValue"
        }));

        assert_eq!(out["$ref"], "#/definitions/Disk");
        assert_eq!(out["nested"]["items"]["$ref"], "#/definitions/Name");
        assert_eq!(out["list"][0]["$ref"], "#/definitions/Foo");
        assert_eq!(out["not_a_ref"], "#/components/schemas/StringValue");
    }

    // The command path is looked up as an `operationId`, and the resulting
    // `RootSchema` carries the body plus a `definitions` map stitched from
    // `components.schemas` (with refs rewritten so they resolve).
    #[test]
    fn schema_from_openapi_pulls_body_and_defs() {
        let spec = json!({
            "paths": {
                "/disks": {
                    "post": {
                        "operationId": "disk_create",
                        "requestBody": { "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/DiskCreate" }
                        }}}
                    }
                }
            },
            "components": { "schemas": {
                "DiskCreate": { "type": "object", "required": ["name"],
                    "properties": { "name": { "$ref": "#/components/schemas/Name" } } },
                "Name": { "type": "string" }
            }}
        });

        let cmd = vec!["disk".to_string(), "create".to_string()];
        let (op_id, root) = schema_from_openapi(&spec, &cmd).expect("operation found");

        assert_eq!(op_id, "disk_create");
        assert!(root.definitions.contains_key("DiskCreate"));
        assert!(root.definitions.contains_key("Name"));
        assert_eq!(
            root.schema.reference.as_deref(),
            Some("#/definitions/DiskCreate"),
            "body ref rewritten to schemars form"
        );
    }

    #[test]
    fn schema_from_openapi_errors_on_unknown_operation() {
        let spec = json!({ "paths": {} });
        let cmd = vec!["nope".to_string()];
        assert!(schema_from_openapi(&spec, &cmd).is_err());
    }

    // An operation with no JSON request body is an error, not an empty schema.
    #[test]
    fn schema_from_openapi_errors_when_no_body() {
        let spec = json!({
            "paths": { "/things": { "get": { "operationId": "thing_list" } } }
        });
        let cmd = vec!["thing".to_string(), "list".to_string()];
        assert!(schema_from_openapi(&spec, &cmd).is_err());
    }
}
