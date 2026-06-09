//! schema-doc: render and scaffold JSON Schemas (schemars `RootSchema`s).
//!
//! Two capabilities, both operating on any `schemars` schema with no
//! dependency on how it was produced (progenitor, an OpenAPI spec, a derived
//! `#[derive(JsonSchema)]` type, etc.):
//!
//! - [`render_body_schema`] — a human- and agent-friendly BNF-style grammar
//!   reference for a type, with each `$ref` emitted as its own named production.
//! - [`generate_value`] — placeholder JSON values for a schema node, so
//!   interactive front-ends (the `schema-tui` builder) can scaffold a body
//!   per-field without reimplementing schema traversal.
//!

use schemars::schema::{InstanceType, RootSchema, Schema, SchemaObject, SingleOrVec};
use std::collections::{BTreeMap, BTreeSet};

/// A schema definitions map (`definitions`), used to resolve `$ref`s.
pub type Defs = BTreeMap<String, Schema>;

/// Generate a placeholder JSON value for an arbitrary schema node, resolving
/// `$ref`s against `definitions`.
///
/// Objects include only their required properties; `anyOf`/`oneOf` select the
/// first viable variant; scalars become empty string / `0` / `false`; enums
/// take their first value.
///
pub fn generate_value(
    schema: &Schema,
    definitions: &Defs,
) -> serde_json::Value {
    match schema {
        Schema::Bool(_) => serde_json::Value::Null,
        Schema::Object(obj) => generate_value_object(obj, definitions),
    }
}

fn generate_value_object(
    schema: &SchemaObject,
    definitions: &Defs,
) -> serde_json::Value {
    if let Some(reference) = &schema.reference {
        if let Some(def_name) = reference.strip_prefix("#/definitions/") {
            if let Some(def_schema) = definitions.get(def_name) {
                return generate_value(def_schema, definitions);
            }
        }
        return serde_json::Value::Null;
    }

    if let Some(sub) = &schema.subschemas {
        if let Some(any_of) = &sub.any_of {
            for sub_schema in any_of {
                let value = generate_value(sub_schema, definitions);
                if !value.is_null() {
                    return value;
                }
            }
        }
        if let Some(one_of) = &sub.one_of {
            if let Some(first) = one_of.first() {
                return generate_value(first, definitions);
            }
        }
        if let Some(all_of) = &sub.all_of {
            if let Some(first) = all_of.first() {
                return generate_value(first, definitions);
            }
        }
    }

    if let Some(enum_values) = &schema.enum_values {
        if let Some(first) = enum_values.first() {
            return first.clone();
        }
    }

    let Some(instance_type) = &schema.instance_type else {
        return serde_json::Value::Null;
    };

    match instance_type {
        SingleOrVec::Single(t) => match **t {
            InstanceType::Null => serde_json::Value::Null,
            InstanceType::Boolean => serde_json::Value::Bool(false),
            InstanceType::Number | InstanceType::Integer => {
                serde_json::Value::Number(serde_json::Number::from(0))
            }
            InstanceType::String => serde_json::Value::String(String::new()),
            InstanceType::Array => match &schema.array {
                Some(items) => match &items.items {
                    Some(SingleOrVec::Single(item_schema)) => {
                        serde_json::Value::Array(vec![generate_value(item_schema, definitions)])
                    }
                    Some(SingleOrVec::Vec(item_schemas)) => serde_json::Value::Array(
                        item_schemas.iter().map(|s| generate_value(s, definitions)).collect(),
                    ),
                    None => serde_json::Value::Array(vec![]),
                },
                None => serde_json::Value::Array(vec![]),
            },
            InstanceType::Object => {
                let mut map = serde_json::Map::new();
                if let Some(object) = &schema.object {
                    for (prop_name, prop_schema) in &object.properties {
                        if !object.required.contains(prop_name) {
                            continue;
                        }
                        map.insert(prop_name.clone(), generate_value(prop_schema, definitions));
                    }
                }
                serde_json::Value::Object(map)
            }
        },
        SingleOrVec::Vec(_) => serde_json::Value::Null,
    }
}

// ----------------------------------------------------------------------------
// Body schema renderer (BNF-style grammar reference).
// ----------------------------------------------------------------------------

/// Render a request body schema as a structured grammar reference.
///
/// ANSI color is applied when stdout is a terminal and `NO_COLOR` is unset.
pub fn render_body_schema(root_schema: &RootSchema) -> String {
    let colorize = should_colorize();
    let style = Style::new(colorize);

    let top_name = root_title(root_schema);

    let usage_map = build_usage_map(root_schema, &top_name);

    let mut referenced: BTreeSet<String> = BTreeSet::new();
    let mut out = String::new();

    // Brief legend so first-time readers know what the conventions mean.
    out.push_str(&style.dim(
        "# [foo] = optional   <Type> = named ref   | = alternative",
    ));
    out.push('\n');
    out.push_str(&style.dim(
        "# (default: X) = default value   (tagged on `f`) = discriminator field",
    ));
    out.push_str("\n\n");

    out.push_str(&render_production(
        &top_name,
        &Schema::Object(root_schema.schema.clone()),
        true,
        &usage_map,
        &root_schema.definitions,
        &mut referenced,
        &style,
    ));

    let mut emitted: BTreeSet<String> = BTreeSet::new();
    while let Some(name) = referenced.iter().find(|n| !emitted.contains(*n)).cloned() {
        emitted.insert(name.clone());
        if let Some(def) = root_schema.definitions.get(&name) {
            out.push('\n');
            out.push_str(&render_production(
                &name,
                def,
                false,
                &usage_map,
                &root_schema.definitions,
                &mut referenced,
                &style,
            ));
        }
    }

    out
}

fn render_production(
    name: &str,
    schema: &Schema,
    is_root: bool,
    usage_map: &BTreeMap<String, BTreeSet<String>>,
    defs: &Defs,
    refs: &mut BTreeSet<String>,
    style: &Style,
) -> String {
    // Effective schema (unwrap single-allOf metadata wrappers).
    let effective: &SchemaObject = match schema {
        // Unwrap a single-element `allOf` object wrapper (a metadata carrier) to
        // its inner object; otherwise use `o` itself.
        Schema::Object(o) => match o.subschemas.as_ref().and_then(|s| s.all_of.as_deref()) {
            Some([Schema::Object(inner)]) => inner,
            _ => o,
        },
        // Non-object production (rare): emit on the header line as before.
        _ => {
            return format!(
                "{} {} {}\n",
                style.bold(name),
                style.dim("::="),
                render_schema(schema, defs, refs, 0, style),
            );
        }
    };

    let oneof_variants: Option<Vec<&Schema>> = non_null_variants(effective);

    // Header annotations.
    let mut annotations: Vec<String> = Vec::new();
    if is_root {
        annotations.push(style.dim("(root)"));
    } else if let Some(users) = usage_map.get(name) {
        let users_vec: Vec<&String> = users.iter().filter(|u| u.as_str() != name).collect();
        if !users_vec.is_empty() {
            let take = 3;
            let mut text = String::from("(used by: ");
            text.push_str(
                &users_vec
                    .iter()
                    .take(take)
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            if users_vec.len() > take {
                text.push_str(&format!(", +{} more", users_vec.len() - take));
            }
            text.push(')');
            annotations.push(style.dim(&text));
        }
    }

    let tag = oneof_variants.as_ref().and_then(|v| detect_tag(v));
    if let Some(t) = &tag {
        annotations.push(style.dim(&format!("(tagged on `{}`)", t)));
    }

    let mut header = format!("{} {}", style.bold(name), style.dim("::="));
    if !annotations.is_empty() {
        header.push_str("  ");
        header.push_str(&annotations.join("  "));
    }

    // Body.
    let body = match &oneof_variants {
        Some(variants) if variants.len() > 1 => {
            render_variants_production(variants, tag.as_deref(), defs, refs, style)
        }
        Some(variants) if variants.len() == 1 => render_schema(variants[0], defs, refs, 0, style),
        _ => render_schema(schema, defs, refs, 0, style),
    };

    format!("{}\n{}\n", header, body)
}

fn render_variants_production(
    variants: &[&Schema],
    tag: Option<&str>,
    defs: &Defs,
    refs: &mut BTreeSet<String>,
    style: &Style,
) -> String {
    // Try inline rendering first. If all variants fit, emit each on its own
    // line with the `|` separator (first variant indented; rest at
    // column 0 with leading `| `).
    let inline_budget = 76;
    let inline_attempts: Vec<Option<String>> = variants
        .iter()
        .map(|v| render_variant_inline(v, tag, defs, refs, style, inline_budget))
        .collect();

    let all_inline = inline_attempts.iter().all(|o| o.is_some());
    if all_inline {
        let rendered: Vec<String> = inline_attempts.into_iter().flatten().collect();
        let any_object = rendered.iter().any(|r| r.contains('{'));
        let single_line = rendered.join(&format!(" {} ", style.bold_yellow("|")));
        // Collapse purely-scalar unions to one line (e.g., `Uuid | String`).
        // Keep object unions in BNF style so each alternative reads as its own row.
        if !any_object && plain_len(&single_line) <= 80 {
            return single_line;
        }
        let mut out = String::new();
        for (i, v) in rendered.iter().enumerate() {
            if i == 0 {
                out.push_str("  ");
            } else {
                out.push('\n');
                out.push_str(&style.bold_yellow("|"));
                out.push(' ');
            }
            out.push_str(v);
        }
        return out;
    }

    // Block style fallback (each variant as a full object).
    let priority = tag.map(|t| vec![t.to_string()]).unwrap_or_default();
    let rendered: Vec<String> = variants
        .iter()
        .map(|v| render_schema_with_priority(v, &priority, defs, refs, 0, style))
        .collect();
    let sep = format!("\n{} ", style.bold_yellow("|"));
    rendered.join(&sep)
}

fn render_variant_inline(
    schema: &Schema,
    tag: Option<&str>,
    defs: &Defs,
    refs: &mut BTreeSet<String>,
    style: &Style,
    budget: usize,
) -> Option<String> {
    // Object variants get tag-first ordering and inline key/value rendering.
    if let Schema::Object(o) = schema {
        if let Some(ov) = &o.object {
            let priority: Vec<String> = tag.map(|t| vec![t.to_string()]).unwrap_or_default();
            let mut parts: Vec<String> = Vec::new();
            for (k, v, is_required) in order_properties(ov, &priority) {
                let (_, key) = render_key(k, is_required, style);
                let rendered = render_schema(v, defs, refs, 0, style);
                parts.push(format!("{} {}{}", key, rendered, default_suffix(v, " ", style)));
            }
            let inline = if parts.is_empty() {
                "{}".to_string()
            } else {
                format!("{{ {} }}", parts.join(", "))
            };
            if plain_len(&inline) <= budget && !inline.contains('\n') {
                return Some(inline);
            }
            return None;
        }
    }

    // Scalar / non-object variants: render normally and check the budget.
    let rendered = render_schema(schema, defs, refs, 0, style);
    if plain_len(&rendered) <= budget && !rendered.contains('\n') {
        Some(rendered)
    } else {
        None
    }
}

fn render_schema_with_priority(
    schema: &Schema,
    priority: &[String],
    defs: &Defs,
    refs: &mut BTreeSet<String>,
    depth: usize,
    style: &Style,
) -> String {
    match schema {
        Schema::Object(o) if !priority.is_empty() => {
            render_object(o, priority, defs, refs, depth, style)
        }
        _ => render_schema(schema, defs, refs, depth, style),
    }
}

/// `key:` for required, dimmed `[key]:` for optional — returned as
/// (plain, styled) so callers can measure alignment on the plain form.
fn render_key(k: &str, is_required: bool, style: &Style) -> (String, String) {
    let plain = if is_required {
        format!("{}:", k)
    } else {
        format!("[{}]:", k)
    };
    let styled = if is_required {
        plain.clone()
    } else {
        style.dim(&plain)
    };
    (plain, styled)
}

/// The dimmed `(default: …)` annotation for a property, or empty. `sep` is
/// the whitespace between the rendered type and the annotation.
fn default_suffix(v: &Schema, sep: &str, style: &Style) -> String {
    match v {
        Schema::Object(o) => default_annotation(o)
            .map(|d| style.dim(&format!("{}(default: {})", sep, d)))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn order_properties<'a>(
    ov: &'a schemars::schema::ObjectValidation,
    priority: &[String],
) -> Vec<(&'a String, &'a Schema, bool)> {
    let mut out: Vec<(&String, &Schema, bool)> = Vec::new();
    let is_required = |k: &String| ov.required.contains(k);

    // Priority fields first, in order.
    for p in priority {
        if let Some((k, v)) = ov.properties.iter().find(|(k, _)| *k == p) {
            out.push((k, v, is_required(k)));
        }
    }
    let used: BTreeSet<&String> = out.iter().map(|t| t.0).collect();

    // Then the rest in one pass — required before optional, each alphabetical
    // via BTreeMap iteration: required go straight out, optional are collected
    // and appended after.
    let mut optional: Vec<(&String, &Schema, bool)> = Vec::new();
    for (k, v) in &ov.properties {
        if used.contains(k) {
            continue;
        }
        if is_required(k) {
            out.push((k, v, true));
        } else {
            optional.push((k, v, false));
        }
    }
    out.extend(optional);
    out
}

fn render_object(
    o: &SchemaObject,
    priority: &[String],
    defs: &Defs,
    refs: &mut BTreeSet<String>,
    depth: usize,
    style: &Style,
) -> String {
    let ov = match &o.object {
        Some(ov) => ov,
        None => return "{}".to_string(),
    };
    if ov.properties.is_empty() {
        return "{}".to_string();
    }

    let indent_outer = "  ".repeat(depth);
    let indent_inner = "  ".repeat(depth + 1);

    // Align all colons. Optional keys are 2 chars wider for the brackets.
    let key_width = ov
        .properties
        .keys()
        .map(|k| {
            if ov.required.contains(k) {
                k.len()
            } else {
                k.len() + 2
            }
        })
        .max()
        .unwrap_or(0);

    let mut lines = Vec::new();
    for (k, v, is_required) in order_properties(ov, priority) {
        let rendered = render_schema(v, defs, refs, depth + 1, style);
        let (key_plain, key_styled) = render_key(k, is_required, style);
        let pad = " ".repeat((key_width + 1).saturating_sub(key_plain.len()));
        lines.push(format!(
            "{}{}{}  {}{}",
            indent_inner,
            key_styled,
            pad,
            rendered,
            default_suffix(v, "   ", style)
        ));
    }

    format!("{{\n{}\n{}}}", lines.join("\n"), indent_outer)
}

fn build_usage_map(
    root: &RootSchema,
    top_name: &str,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut map: BTreeMap<String, BTreeSet<String>> =
        BTreeMap::new();
    walk_for_refs(&Schema::Object(root.schema.clone()), top_name, &mut map);
    for (def_name, def_schema) in &root.definitions {
        walk_for_refs(def_schema, def_name, &mut map);
    }
    map
}

fn walk_for_refs(
    schema: &Schema,
    parent: &str,
    map: &mut BTreeMap<String, BTreeSet<String>>,
) {
    let Schema::Object(o) = schema else { return };
    if let Some(r) = &o.reference {
        let name = ref_name(r);
        map.entry(name).or_default().insert(parent.to_string());
        return;
    }
    if let Some(sub) = &o.subschemas {
        for items in [&sub.one_of, &sub.any_of, &sub.all_of].into_iter().flatten() {
            for s in items {
                walk_for_refs(s, parent, map);
            }
        }
    }
    if let Some(ov) = &o.object {
        for v in ov.properties.values() {
            walk_for_refs(v, parent, map);
        }
    }
    if let Some(av) = &o.array {
        match &av.items {
            Some(SingleOrVec::Single(s)) => walk_for_refs(s, parent, map),
            Some(SingleOrVec::Vec(v)) => {
                for s in v {
                    walk_for_refs(s, parent, map);
                }
            }
            None => {}
        }
    }
}

/// Detect the discriminator (tag) field of a tagged union: the single field
/// that appears, as a single-value string enum, in every variant. Returns
/// `None` if the variants don't share exactly one such field.
pub fn detect_tag(variants: &[&Schema]) -> Option<String> {
    // For each variant, collect fields that are single-value string enums.
    // The tag is a field present (with that shape) in every variant.
    let candidate_sets: Vec<BTreeSet<String>> = variants
        .iter()
        .map(|v| {
            let Schema::Object(o) = v else {
                return BTreeSet::new();
            };
            let Some(ov) = &o.object else {
                return BTreeSet::new();
            };
            ov.properties
                .iter()
                .filter_map(|(name, schema)| {
                    let Schema::Object(s) = schema else { return None };
                    let values = s.enum_values.as_ref()?;
                    if values.len() == 1 && values[0].is_string() {
                        Some(name.clone())
                    } else {
                        None
                    }
                })
                .collect()
        })
        .collect();

    if candidate_sets.is_empty() || candidate_sets.iter().any(|s| s.is_empty()) {
        return None;
    }

    let mut intersection = candidate_sets[0].clone();
    for s in &candidate_sets[1..] {
        intersection = intersection.intersection(s).cloned().collect();
    }

    if intersection.len() == 1 {
        intersection.into_iter().next()
    } else {
        None
    }
}

fn should_colorize() -> bool {
    use std::io::IsTerminal as _;
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if std::env::var_os("CLICOLOR_FORCE").is_some() {
        return true;
    }
    std::io::stdout().is_terminal()
}

struct Style {
    enabled: bool,
}

impl Style {
    fn new(enabled: bool) -> Self {
        Self { enabled }
    }
    fn wrap(&self, code: &str, s: &str) -> String {
        if self.enabled {
            format!("\x1b[{}m{}\x1b[0m", code, s)
        } else {
            s.to_string()
        }
    }
    fn dim(&self, s: &str) -> String {
        self.wrap("2", s)
    }
    fn bold(&self, s: &str) -> String {
        self.wrap("1", s)
    }
    fn cyan(&self, s: &str) -> String {
        self.wrap("36", s)
    }
    fn green(&self, s: &str) -> String {
        self.wrap("32", s)
    }
    fn bold_yellow(&self, s: &str) -> String {
        self.wrap("1;33", s)
    }
}

fn render_schema(
    s: &Schema,
    defs: &Defs,
    refs: &mut BTreeSet<String>,
    depth: usize,
    style: &Style,
) -> String {
    match s {
        Schema::Bool(true) => "any".to_string(),
        Schema::Bool(false) => "never".to_string(),
        Schema::Object(o) => render_schema_object(o, defs, refs, depth, style),
    }
}

fn render_schema_object(
    o: &SchemaObject,
    defs: &Defs,
    refs: &mut BTreeSet<String>,
    depth: usize,
    style: &Style,
) -> String {
    if let Some(r) = &o.reference {
        let name = ref_name(r);
        refs.insert(name.clone());
        return style.cyan(&format!("<{}>", name));
    }

    // allOf wrapping a single ref is just a description carrier; unwrap.
    if let Some(sub) = &o.subschemas {
        if let Some(all) = &sub.all_of {
            if all.len() == 1 {
                return render_schema(&all[0], defs, refs, depth, style);
            }
        }
    }

    // oneOf/anyOf: drop null alternatives (optionality is shown by brackets).
    if let Some(sub) = &o.subschemas {
        if let Some(variants) = sub.one_of.as_ref().or(sub.any_of.as_ref()) {
            let non_null: Vec<&Schema> = variants.iter().filter(|v| !is_null_schema(v)).collect();
            if non_null.len() == 1 {
                return render_schema(non_null[0], defs, refs, depth, style);
            }
            let rendered: Vec<String> = non_null
                .iter()
                .map(|v| render_schema(v, defs, refs, depth, style))
                .collect();
            return join_variants(&rendered, depth, style);
        }
    }

    if let Some(values) = &o.enum_values {
        if !values.is_empty() {
            let rendered: Vec<String> = values
                .iter()
                .map(|v| match v {
                    serde_json::Value::String(s) => style.green(&format!("\"{}\"", s)),
                    other => other.to_string(),
                })
                .collect();
            let sep = format!(" {} ", style.bold_yellow("|"));
            return rendered.join(&sep);
        }
    }

    match instance_type(&o.instance_type) {
        Some(InstanceType::String) => render_string(o),
        Some(InstanceType::Integer) => render_integer(o),
        Some(InstanceType::Number) => "Number".to_string(),
        Some(InstanceType::Boolean) => "bool".to_string(),
        Some(InstanceType::Null) => "null".to_string(),
        Some(InstanceType::Array) => render_array(o, defs, refs, depth, style),
        Some(InstanceType::Object) | None => render_object(o, &[], defs, refs, depth, style),
    }
}

/// True if `s` is the `null` schema (its instance type is `null`). Used to
/// filter the nullable arm out of `oneOf`/`anyOf` unions.
pub fn is_null_schema(s: &Schema) -> bool {
    match s {
        Schema::Object(o) => matches!(instance_type(&o.instance_type), Some(InstanceType::Null)),
        _ => false,
    }
}

/// The `oneOf`/`anyOf` arms of a schema object, with `null` arms removed.
/// Returns `None` when there's no such union. schemars uses `anyOf` for
/// `Option<T>` and `oneOf` for tagged unions.
pub fn non_null_variants(o: &SchemaObject) -> Option<Vec<&Schema>> {
    let sub = o.subschemas.as_ref()?;
    let variants = sub.one_of.as_ref().or(sub.any_of.as_ref())?;
    Some(variants.iter().filter(|v| !is_null_schema(v)).collect())
}

/// True when `o` is a union with more than one real arm.
pub fn is_multi_variant(o: &SchemaObject) -> bool {
    non_null_variants(o).is_some_and(|v| v.len() > 1)
}

/// The display name for a root schema: its `title` in `snake_case`, falling
/// back to `"request"`.
pub fn root_title(root: &RootSchema) -> String {
    root.schema
        .metadata
        .as_ref()
        .and_then(|m| m.title.clone())
        .map(|t| snake_case(&t))
        .unwrap_or_else(|| "request".to_string())
}

/// Display name for a string schema's `format` — a JSON Schema / OpenAPI
/// vocabulary value, e.g. `"uuid"` → `Uuid`, `"date-time"` → `DateTime`.
/// Unknown formats render as `String (<format>)`; no format renders as `String`.
/// Shared so the grammar reference and the interactive builder name string
/// types identically.
pub fn string_format_name(format: Option<&str>) -> String {
    match format {
        Some("uuid") => "Uuid".to_string(),
        Some("date-time") => "DateTime".to_string(),
        Some("ip") => "IpAddr".to_string(),
        Some("ipv4") => "Ipv4Addr".to_string(),
        Some("ipv6") => "Ipv6Addr".to_string(),
        Some("byte") => "Base64".to_string(),
        Some(other) => format!("String ({})", other),
        None => "String".to_string(),
    }
}

fn render_string(o: &SchemaObject) -> String {
    string_format_name(o.format.as_deref())
}

fn render_integer(o: &SchemaObject) -> String {
    o.format.clone().unwrap_or_else(|| "Integer".to_string())
}

fn render_array(
    o: &SchemaObject,
    defs: &Defs,
    refs: &mut BTreeSet<String>,
    depth: usize,
    style: &Style,
) -> String {
    let item = match &o.array {
        Some(av) => match &av.items {
            Some(SingleOrVec::Single(s)) => render_schema(s, defs, refs, depth, style),
            Some(SingleOrVec::Vec(v)) if !v.is_empty() => {
                render_schema(&v[0], defs, refs, depth, style)
            }
            _ => "any".to_string(),
        },
        None => "any".to_string(),
    };
    format!("[{}, ...]", item)
}

fn join_variants(variants: &[String], depth: usize, style: &Style) -> String {
    let sep_inline = format!(" {} ", style.bold_yellow("|"));
    let inline = variants.join(&sep_inline);
    let budget = 80usize.saturating_sub(2 * depth);
    if !inline.contains('\n') && plain_len(&inline) <= budget {
        return inline;
    }
    let indent = "  ".repeat(depth);
    let sep = format!("\n{}{} ", indent, style.bold_yellow("|"));
    let mut out = String::new();
    out.push_str(&variants[0]);
    for v in &variants[1..] {
        out.push_str(&sep);
        out.push_str(v);
    }
    out
}

fn plain_len(s: &str) -> usize {
    let mut len = 0;
    let mut in_esc = false;
    for c in s.chars() {
        if in_esc {
            if c.is_ascii_alphabetic() {
                in_esc = false;
            }
        } else if c == '\x1b' {
            in_esc = true;
        } else {
            len += 1;
        }
    }
    len
}

fn default_annotation(o: &SchemaObject) -> Option<String> {
    let d = o.metadata.as_ref()?.default.as_ref()?;
    Some(match d {
        serde_json::Value::String(s) => format!("\"{}\"", s),
        other => other.to_string(),
    })
}

/// Collapse a schema's instance type to a single non-null `InstanceType`,
/// picking the first non-null entry when the schema lists several.
pub fn instance_type(t: &Option<SingleOrVec<InstanceType>>) -> Option<InstanceType> {
    match t {
        Some(SingleOrVec::Single(t)) => Some(**t),
        Some(SingleOrVec::Vec(v)) => v.iter().find(|t| !matches!(t, InstanceType::Null)).copied(),
        None => None,
    }
}

/// The trailing segment of a `$ref` pointer, e.g. `#/definitions/Foo` -> `Foo`.
pub fn ref_name(r: &str) -> String {
    r.rsplit('/').next().unwrap_or(r).to_string()
}

/// Convert a `PascalCase`/`camelCase` type name to `snake_case`.
pub fn snake_case(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(v: serde_json::Value) -> SchemaObject {
        serde_json::from_value(v).expect("valid schema object")
    }

    fn schema(v: serde_json::Value) -> Schema {
        serde_json::from_value(v).expect("valid schema")
    }

    #[test]
    fn non_null_variants_keeps_real_arms() {
        let o = object(json!({
            "oneOf": [
                { "type": "object", "properties": { "type": { "enum": ["local"] } } },
                { "type": "object", "properties": { "type": { "enum": ["remote"] } } }
            ]
        }));
        let arms = non_null_variants(&o).expect("has a union");
        assert_eq!(arms.len(), 2);
        assert!(is_multi_variant(&o));
    }

    // schemars encodes `Option<T>` as `anyOf: [T, null]`. The `null` arm is
    // dropped, leaving a single real arm — that's a nullable wrapper, NOT a
    // multi-variant picker.
    #[test]
    fn non_null_variants_drops_null_arm() {
        let o = object(json!({
            "anyOf": [{ "$ref": "#/definitions/Thing" }, { "type": "null" }]
        }));
        let arms = non_null_variants(&o).expect("has a union");
        assert_eq!(arms.len(), 1);
        assert!(!is_multi_variant(&o), "one real arm is not a picker");
    }

    // A plain type has no union at all.
    #[test]
    fn non_null_variants_none_without_union() {
        let o = object(json!({ "type": "string" }));
        assert!(non_null_variants(&o).is_none());
        assert!(!is_multi_variant(&o));
    }

    #[test]
    fn root_title_snake_cases_title_else_request() {
        let titled: RootSchema =
            serde_json::from_value(json!({ "title": "DiskCreate", "type": "object" })).unwrap();
        assert_eq!(root_title(&titled), "disk_create");

        let untitled: RootSchema = serde_json::from_value(json!({ "type": "object" })).unwrap();
        assert_eq!(root_title(&untitled), "request");
    }

    #[test]
    fn is_null_schema_only_for_null_type() {
        assert!(is_null_schema(&schema(json!({ "type": "null" }))));
        assert!(!is_null_schema(&schema(json!({ "type": "string" }))));
    }

    // The discriminator of a tagged union is the property that is a single-value
    // enum across every arm.
    #[test]
    fn detect_tag_finds_discriminator() {
        let o = object(json!({
            "oneOf": [
                { "type": "object", "required": ["type"],
                  "properties": { "type": { "enum": ["local"] } } },
                { "type": "object", "required": ["type"],
                  "properties": { "type": { "enum": ["remote"] }, "host": { "type": "string" } } }
            ]
        }));
        let arms = non_null_variants(&o).unwrap();
        assert_eq!(detect_tag(&arms).as_deref(), Some("type"));
    }

    // `generate_value` resolves a single schema node to the placeholder the
    // interactive builder shows per-field. This — leaf nodes and `$ref`s, not
    // whole objects — is the only way `schema-tui` calls it.
    #[test]
    fn generate_value_resolves_leaf_placeholders() {
        let defs = BTreeMap::new();
        let val = |s: serde_json::Value| generate_value(&serde_json::from_value(s).unwrap(), &defs);

        assert_eq!(val(json!({ "type": "string" })), json!(""));
        assert_eq!(val(json!({ "type": "integer" })), json!(0));
        assert_eq!(val(json!({ "type": "boolean" })), json!(false));
        // An enum resolves to its first value (the discriminator placeholder).
        assert_eq!(val(json!({ "enum": ["first", "second"] })), json!("first"));
        // A scalar array resolves to a one-element list of the item placeholder.
        assert_eq!(
            val(json!({ "type": "array", "items": { "type": "string" } })),
            json!([""])
        );
    }

    // `$ref`s resolve against `definitions` before producing a value.
    #[test]
    fn generate_value_resolves_refs() {
        let mut defs = BTreeMap::new();
        defs.insert("Name".to_string(), serde_json::from_value(json!({ "type": "string" })).unwrap());

        let schema: Schema = serde_json::from_value(json!({ "$ref": "#/definitions/Name" })).unwrap();
        assert_eq!(generate_value(&schema, &defs), json!(""));
    }

    #[test]
    fn ref_name_and_snake_case() {
        assert_eq!(ref_name("#/definitions/NameOrId"), "NameOrId");
        assert_eq!(ref_name("#/components/schemas/DiskCreate"), "DiskCreate");
        assert_eq!(snake_case("NameOrId"), "name_or_id");
    }
}
