# schema-doc

Render and scaffold JSON Schemas (`schemars::schema::RootSchema`). Works with any schemars schema regardless of where it came from (progenitor, an OpenAPI spec, a `#[derive(JsonSchema)]` type).

Two main capabilities:

- **`render_body_schema`**: renders a schema as a compact, BNF-style grammar reference. Each `$ref` becomes its own named production, tagged unions are annotated with their discriminator field, optional fields are bracketed, and defaults are shown inline. Output is colorized when stdout is a terminal (respects `NO_COLOR` and `CLICOLOR_FORCE`).
- **`generate_value`**: produces a placeholder JSON value for a schema node, resolving `$ref`s against a definitions map. Objects include only required properties, unions pick the first viable variant, scalars get empty/zero values, and enums use their first value.

The crate also exports the schema traversal helpers it is built on (`non_null_variants`, `transparent_inner`, `detect_tag`, `scalar_type_name`, `ref_name`, and others) so other tools can interpret schemas the same way. The `schema-tui` crate uses these to drive its interactive builder.

## Usage

```rust
use schemars::schema::RootSchema;

let root: RootSchema = serde_json::from_str(&schema_json)?;

// Print a grammar reference for the whole schema.
println!("{}", schema_doc::render_body_schema(&root));

// Build a placeholder JSON body for the root type.
let body = schema_doc::generate_value(
    &schemars::schema::Schema::Object(root.schema.clone()),
    &root.definitions,
);
println!("{}", serde_json::to_string_pretty(&body)?);
```

Example grammar output:

```
disk_create ::=  (root)
{
  description:  String
  name:         <Name>
  size:         <ByteCount>
  [disk_source]: <DiskSource>
}

DiskSource ::=  (tagged on `type`)
  { type: "blank", block_size: <BlockSize> }
| { type: "snapshot", snapshot_id: Uuid }
```
