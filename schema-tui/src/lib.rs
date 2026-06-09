//! schema-tui-core: an interactive accordion **builder** for JSON request
//! bodies. Browse a `schemars` schema as a collapsible tree, expand the
//! optional fields you want, pick `oneOf` variants, then quit — the body you
//! shaped is returned as JSON. Placeholder values come from [`schema_doc`], so
//! the builder and the non-interactive template agree field-for-field.

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use schema_doc::{
    Defs, detect_tag, display_value, generate_value, instance_type, is_multi_variant, item_schema,
    non_null_variants, order_properties, ref_name, root_title, scalar_type_name, transparent_inner,
};
use schemars::schema::{InstanceType, RootSchema, Schema, SchemaObject};
use serde_json::{Map, Value};
use std::fs::File;
use std::io::{self, Read, Stderr};
use std::path::Path;

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Load a JSON Schema from a file path. `path == "-"` reads from stdin.
pub fn load_schema_from_file(path: &Path) -> Result<RootSchema> {
    let content = if path.as_os_str() == "-" {
        let mut s = String::new();
        io::stdin().read_to_string(&mut s).context("reading stdin")?;
        s
    } else {
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
    };
    serde_json::from_str(&content).context("parsing schema JSON")
}

/// Outcome of an interactive session.
pub enum Outcome {
    Export(Value),
    Cancel,
}

/// Drive the TUI to completion. On success, returns either the JSON the user
/// built (Export) or Cancel.
pub fn run_tui(schema: RootSchema, title: String) -> Result<Outcome> {
    let mut terminal = setup_terminal().context("setting up terminal")?;
    let result = run(&mut terminal, schema, title);
    restore_terminal(&mut terminal)?;
    result
}

/// Print the outcome: Export → pretty JSON to stdout, Cancel → nothing.
pub fn print_outcome(outcome: Outcome) -> Result<()> {
    if let Outcome::Export(json) = outcome {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        serde_json::to_writer_pretty(&mut out, &json)?;
        println!();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal: terminal setup + the rest of the TUI plumbing.
// ---------------------------------------------------------------------------

type TerminalT = Terminal<CrosstermBackend<TtyOut>>;

/// Output sink for the TUI — try /dev/tty so stdout is free for JSON,
/// otherwise fall back to stderr.
enum TtyOut {
    Tty(File),
    Err(Stderr),
}

impl io::Write for TtyOut {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            TtyOut::Tty(f) => f.write(buf),
            TtyOut::Err(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            TtyOut::Tty(f) => f.flush(),
            TtyOut::Err(s) => s.flush(),
        }
    }
}

fn tty_out() -> TtyOut {
    std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .map(TtyOut::Tty)
        .unwrap_or_else(|_| TtyOut::Err(io::stderr()))
}

fn setup_terminal() -> Result<TerminalT> {
    enable_raw_mode()?;
    // A panic unwinds past `restore_terminal`; restore from the hook so the
    // user's shell isn't left raw on the alternate screen.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(tty_out(), LeaveAlternateScreen);
        prev(info);
    }));
    let mut out = tty_out();
    execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(terminal: &mut TerminalT) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Tree model
// -----------------------------------------------------------------------------
//
// The tree mirrors the schema's structure. Each node carries enough info to:
//   - render its row in the TUI
//   - know whether to include itself in the final JSON
//   - resolve scalar leaves to placeholder values
//
// `kind` distinguishes:
//   - Object: an object (root, named ref, or anonymous nested) — children are
//     property nodes
//   - OneOf: a oneOf at this position — children are variant nodes; one is
//     `selected_variant`
//   - Variant: an object child of a OneOf (the picked variant contributes its
//     children to the output)
//   - Array: a list whose single child is a representative element the user can
//     drill into; the output is `[element]`
//   - Leaf: a scalar / enum / scalar-array field with no further drill-down
//
// `included` controls whether this node contributes to JSON output. Required
// nodes start included and can't be turned off. Optional nodes start
// excluded and the user includes them by pressing Enter / space.

#[derive(Clone, Debug)]
enum NodeKind {
    Object,
    OneOf,
    Variant,
    Array,
    Leaf,
}

#[derive(Clone, Debug)]
struct Node {
    label: String,
    type_desc: String,
    is_required: bool,
    included: bool,
    default: Option<Value>,
    /// Placeholder JSON for a `Leaf` node, produced by
    /// `progenitor_client::generate_value`. A schema `default` (when present)
    /// still takes precedence in `placeholder_value`.
    leaf_value: Option<Value>,
    annotations: Vec<String>,
    expandable: bool,
    expanded: bool,
    children: Vec<Node>,
    populated: bool,
    kind: NodeKind,
    selected_variant: usize, // only meaningful when kind == OneOf
    schema_snapshot: Option<Schema>,
}

impl Node {
    fn placeholder_value(&self) -> Value {
        // An array emits `[element]` from its representative child. This wins
        // over any schema `default` (e.g. `[]`): if the user included/drilled
        // the array, they want a populated element, not the empty default.
        if let NodeKind::Array = self.kind {
            return Value::Array(
                self.children
                    .first()
                    .map(|e| vec![e.placeholder_value()])
                    .unwrap_or_default(),
            );
        }
        // A concrete schema default (or a scalar `oneOf` variant's value) always
        // wins; otherwise leaves use the progenitor-generated placeholder and
        // containers are assembled from their included children.
        if let Some(d) = &self.default {
            return d.clone();
        }
        match &self.kind {
            NodeKind::Leaf => self.leaf_value.clone().unwrap_or(Value::Null),
            // A scalar variant (e.g. a `name | uuid` arm of `NameOrId`) carries
            // its placeholder in `leaf_value` and emits that — not an empty `{}`.
            // Tagged-object variants have no `leaf_value` and assemble normally.
            NodeKind::Variant => self
                .leaf_value
                .clone()
                .unwrap_or_else(|| self.object_value()),
            NodeKind::Object => self.object_value(),
            NodeKind::OneOf => self
                .children
                .get(self.selected_variant)
                .map(|c| c.placeholder_value())
                .unwrap_or(Value::Null),
            NodeKind::Array => unreachable!("handled above"),
        }
    }

    /// Build an object value by walking children that are `included`.
    fn object_value(&self) -> Value {
        let mut map = Map::new();
        for child in &self.children {
            if !child.included {
                continue;
            }
            map.insert(child.label.clone(), child.placeholder_value());
        }
        Value::Object(map)
    }
}

fn build_root(schema: &RootSchema) -> Node {
    let top_name = root_title(schema);

    let mut root = Node {
        label: top_name,
        type_desc: "(root)".to_string(),
        is_required: true,
        included: true,
        default: None,
        leaf_value: None,
        annotations: Vec::new(),
        expandable: true,
        expanded: true,
        children: Vec::new(),
        populated: false,
        kind: NodeKind::Object,
        selected_variant: 0,
        schema_snapshot: Some(Schema::Object(schema.schema.clone())),
    };
    populate(&mut root, &schema.definitions, 0);
    root
}

const MAX_POPULATE_DEPTH: usize = 64;

fn populate(node: &mut Node, defs: &Defs, depth: usize) {
    if node.populated || depth > MAX_POPULATE_DEPTH {
        node.populated = true;
        return;
    }
    let Some(schema) = node.schema_snapshot.take() else {
        node.populated = true;
        return;
    };
    let Some(o) = resolve_to_object(&schema, defs) else {
        node.populated = true;
        return;
    };

    // oneOf / anyOf with multiple non-null variants → variant children.
    if let Some(non_null) = non_null_variants(&o) {
        if non_null.len() > 1 {
            node.kind = NodeKind::OneOf;
            if let Some(tag) = detect_tag(&non_null) {
                node.annotations.push(format!("(tagged on `{}`)", tag));
            }
            for variant in non_null.iter() {
                // An arm is built like any other child, then specialized:
                //   - it renders/selects as a `Variant` (unless its body is
                //     itself a oneOf, which stays drillable as `OneOf`);
                //   - a drillable arm hides the redundant `object` type label;
                //   - a single-value enum arm (e.g. `"never"`) shows its value.
                let mut child = child_node(variant_label(variant, defs), variant, true, defs, depth);
                if !matches!(child.kind, NodeKind::OneOf) {
                    child.kind = NodeKind::Variant;
                }
                if child.expandable {
                    child.type_desc.clear();
                }
                child.default = scalar_variant_value(variant);
                node.children.push(child);
            }
            node.populated = true;
            return;
        } else if non_null.len() == 1 {
            // Single variant; treat as a transparent wrapper.
            let single = non_null[0].clone();
            if let Schema::Object(inner) = single {
                populate_from_object(node, &inner, defs, depth);
                return;
            }
        }
    }

    // array → one representative element child the user drills into. The
    // element is always "in" (`is_required`), so it populates eagerly.
    if let Some(item) = item_schema(&o) {
        node.children
            .push(child_node("[item]".to_string(), item, true, defs, depth));
        node.populated = true;
        return;
    }

    populate_from_object(node, &o, defs, depth);
}

fn populate_from_object(
    node: &mut Node,
    o: &SchemaObject,
    defs: &Defs,
    depth: usize,
) {
    let Some(ov) = &o.object else {
        node.populated = true;
        return;
    };
    for (k, v, is_required) in order_properties(ov, &[]) {
        node.children.push(child_node(k.clone(), v, is_required, defs, depth));
    }
    node.populated = true;
}

/// Construct a child node from a schema: classify it, wire up its placeholder
/// value, and — for a required, expandable child — populate it eagerly so its
/// own required descendants are present in the export. This is the single
/// shared constructor behind every child in the tree: object properties, array
/// elements, and `oneOf` arms (the arm site applies a couple of arm-specific
/// tweaks on top).
fn child_node(
    label: String,
    schema: &Schema,
    is_required: bool,
    defs: &Defs,
    depth: usize,
) -> Node {
    let o = match schema {
        Schema::Object(o) => o.clone(),
        _ => SchemaObject::default(),
    };
    let (kind, expandable, leaf_value) = classify(&o, defs);
    let mut node = Node {
        label,
        type_desc: describe_type(&o),
        is_required,
        included: is_required,
        default: default_value(&o),
        leaf_value,
        annotations: Vec::new(),
        expandable,
        expanded: false,
        children: Vec::new(),
        populated: !expandable,
        kind,
        selected_variant: 0,
        schema_snapshot: if expandable { Some(schema.clone()) } else { None },
    };
    if expandable && is_required {
        populate(&mut node, defs, depth + 1);
    }
    node
}

/// Decide how a property schema is presented: an expandable `OneOf`/`Object`,
/// or a `Leaf`. For leaves we delegate the placeholder value to
/// `progenitor_client::generate_value` (same logic that backs
/// `--json-body-template`'s non-interactive output), so arrays, enums, and
/// scalars all resolve identically without a parallel implementation here.
fn classify(o: &SchemaObject, defs: &Defs) -> (NodeKind, bool, Option<Value>) {
    let leaf = |o: &SchemaObject| {
        (
            NodeKind::Leaf,
            false,
            Some(generate_value(&Schema::Object(o.clone()), defs)),
        )
    };

    let Some(resolved) = resolve_to_object(&Schema::Object(o.clone()), defs) else {
        return leaf(o);
    };
    // oneOf with >1 non-null variant → expandable variant picker.
    if is_multi_variant(&resolved) {
        return (NodeKind::OneOf, true, None);
    }
    // object → expandable.
    if resolved.object.is_some() {
        return (NodeKind::Object, true, None);
    }
    // array of complex elements (object / multi-variant oneOf) → drillable; the
    // user customizes a representative element. Arrays of scalars stay leaves.
    if let Some(item) = item_schema(&resolved) {
        if let Some(item_resolved) = resolve_to_object(item, defs) {
            if item_resolved.object.is_some() || is_multi_variant(&item_resolved) {
                return (NodeKind::Array, true, None);
            }
        }
    }
    // everything else (enum, scalar, scalar array) is a leaf with a generated value.
    leaf(o)
}

fn default_value(o: &SchemaObject) -> Option<Value> {
    let d = o.metadata.as_ref()?.default.clone()?;
    // `default: null` in OpenAPI typically means "no concrete default" on a
    // nullable field. Don't treat it as a real value — fall through to
    // type-based placeholders instead.
    if matches!(d, Value::Null) {
        return None;
    }
    Some(d)
}

fn resolve_to_object(schema: &Schema, defs: &Defs) -> Option<SchemaObject> {
    let Schema::Object(mut o) = schema.clone() else {
        return None;
    };
    for _ in 0..32 {
        if let Some(r) = &o.reference {
            let name = ref_name(r);
            if let Some(Schema::Object(inner)) = defs.get(&name) {
                o = inner.clone();
                continue;
            }
            return Some(o);
        }
        // See through single-element `allOf` and `Option<T>` wrappers to the
        // real type (e.g. so a field classifies as its inner type, not a union).
        let inner = match transparent_inner(&o) {
            Some(Schema::Object(inner)) => inner.clone(),
            _ => break,
        };
        o = inner;
    }
    Some(o)
}

fn describe_type(o: &SchemaObject) -> String {
    if let Some(r) = &o.reference {
        return format!("<{}>", ref_name(r));
    }
    // A transparent wrapper (single-arm `allOf` / `Option<T>`) describes as its
    // inner type, e.g. `<InstanceDiskAttachment>` rather than "(variants)".
    if let Some(Schema::Object(inner)) = transparent_inner(o) {
        return describe_type(inner);
    }
    // A real multi-arm union has no single inner type to name.
    if is_multi_variant(o) {
        return "(variants)".to_string();
    }
    if let Some(values) = &o.enum_values {
        if !values.is_empty() {
            let items: Vec<String> = values.iter().take(4).map(display_value).collect();
            return items.join(" | ");
        }
    }
    if o.array.is_some() {
        let item = match item_schema(o) {
            Some(Schema::Object(io)) => describe_type(io),
            _ => "any".to_string(),
        };
        return format!("[{}, ...]", item);
    }
    scalar_type_name(o).unwrap_or_else(|| match instance_type(&o.instance_type) {
        Some(InstanceType::Array) => "[...]".to_string(),
        Some(InstanceType::Object) => "object".to_string(),
        _ => "any".to_string(),
    })
}

fn variant_label(schema: &Schema, defs: &Defs) -> String {
    if let Schema::Object(o) = schema {
        // Tagged-object variant: {type: "object", properties: {type: {enum: ["local"]}, …}}.
        if let Some(ov) = &o.object {
            for (name, prop) in &ov.properties {
                let Schema::Object(s) = prop else { continue };
                let Some(values) = &s.enum_values else { continue };
                if values.len() == 1 {
                    if let Value::String(v) = &values[0] {
                        return format!("\"{}\"  ({})", v, name);
                    }
                }
            }
        }
        // Scalar enum variant: {type: "string", enum: ["never"]}.
        if let Some(values) = &o.enum_values {
            if values.len() == 1 {
                if let Value::String(v) = &values[0] {
                    return format!("\"{}\"", v);
                }
            }
        }
        // Named arm: schemars emits the Rust variant name as the arm's title
        // (e.g. `NameOrId`'s `id` / `name`). Prefer it over a generic label.
        if let Some(title) = o.metadata.as_ref().and_then(|m| m.title.clone()) {
            return title;
        }
    }
    // Last resort: describe the arm's resolved type (e.g. `Uuid`) rather than
    // an opaque "variant".
    if let Some(resolved) = resolve_to_object(schema, defs) {
        let d = describe_type(&resolved);
        if d != "any" {
            return d;
        }
    }
    "variant".to_string()
}

/// If `schema` is a scalar (string/integer/etc.) with a single-value `enum`,
/// return that value. This is the OpenAPI shape for one branch of a
/// `oneOf`-of-enum (e.g. `auto_restart_policy = "never" | "best_effort"`).
fn scalar_variant_value(schema: &Schema) -> Option<Value> {
    let Schema::Object(o) = schema else { return None };
    let values = o.enum_values.as_ref()?;
    if values.len() == 1 {
        return Some(values[0].clone());
    }
    None
}

// -----------------------------------------------------------------------------
// View flattening
// -----------------------------------------------------------------------------

#[derive(Clone)]
struct Row {
    depth: u16,
    label: String,
    type_desc: String,
    default: Option<String>,
    annotations: Vec<String>,
    is_required: bool,
    included: bool,
    expandable: bool,
    expanded: bool,
    is_variant_child: bool,
    is_selected_variant: bool,
    path: Vec<usize>,
}

fn flatten(node: &Node, depth: u16, path: &mut Vec<usize>, out: &mut Vec<Row>) {
    let is_oneof_parent = matches!(node.kind, NodeKind::OneOf);
    let is_variant_child = matches!(node.kind, NodeKind::Variant);

    out.push(Row {
        depth,
        label: node.label.clone(),
        type_desc: node.type_desc.clone(),
        default: node.default.as_ref().map(display_value),
        annotations: node.annotations.clone(),
        is_required: node.is_required,
        included: node.included,
        expandable: node.expandable,
        expanded: node.expanded,
        is_variant_child,
        is_selected_variant: false, // set by parent below
        path: path.clone(),
    });

    if node.expanded {
        for (i, child) in node.children.iter().enumerate() {
            path.push(i);
            let pre_len = out.len();
            flatten(child, depth + 1, path, out);
            // Tag whether this variant child is the selected one, so the
            // renderer can mark it visually.
            if is_oneof_parent && i == node.selected_variant {
                if let Some(row) = out.get_mut(pre_len) {
                    row.is_selected_variant = true;
                }
            }
            path.pop();
        }
    }
}

fn node_at_mut<'a>(root: &'a mut Node, path: &[usize]) -> Option<&'a mut Node> {
    let mut cur = root;
    for &i in path {
        cur = cur.children.get_mut(i)?;
    }
    Some(cur)
}

/// Mark every node along `path` — and each of its ancestors up to the root —
/// as `included`. Without this, a deep choice (picking a variant, or
/// including a nested field) is silently dropped from the export because
/// some ancestor optional container was never itself included.
fn include_path(root: &mut Node, path: &[usize]) {
    let mut cur = root;
    cur.included = true;
    for &i in path {
        let Some(next) = cur.children.get_mut(i) else {
            return;
        };
        next.included = true;
        cur = next;
    }
}

// -----------------------------------------------------------------------------
// App / event loop
// -----------------------------------------------------------------------------

struct App {
    root: Node,
    defs: Defs,
    rows: Vec<Row>,
    state: ListState,
    title: String,
}

impl App {
    fn new(schema: RootSchema, title: String) -> Self {
        let defs = schema.definitions.clone();
        let root = build_root(&schema);
        let mut s = ListState::default();
        s.select(Some(0));
        let mut app = Self {
            root,
            defs,
            rows: Vec::new(),
            state: s,
            title,
        };
        app.refresh();
        app
    }

    fn refresh(&mut self) {
        let mut path = Vec::new();
        self.rows.clear();
        flatten(&self.root, 0, &mut path, &mut self.rows);
        if let Some(i) = self.state.selected() {
            if i >= self.rows.len() {
                self.state.select(self.rows.len().checked_sub(1));
            }
        }
    }

    fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let cur = self.state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, self.rows.len() as isize - 1);
        self.state.select(Some(next as usize));
    }

    fn jump_top(&mut self) {
        self.state.select((!self.rows.is_empty()).then_some(0));
    }

    fn jump_bottom(&mut self) {
        let n = self.rows.len();
        self.state.select((n > 0).then_some(n - 1));
    }

    /// The row under the cursor, cloned so handlers can mutate the tree.
    fn selected_row(&self) -> Option<Row> {
        self.rows.get(self.state.selected()?).cloned()
    }

    /// Pick a oneOf variant: update the parent's `selected_variant`, include
    /// the parent *and* every ancestor (so the choice survives to the export
    /// even when nested in optional containers), and expand the arm.
    fn select_variant(&mut self, row: &Row) {
        let Self { root, defs, .. } = self;
        if let Some((parent_path, child_idx)) = split_path(&row.path) {
            if let Some(parent) = node_at_mut(root, parent_path) {
                parent.selected_variant = child_idx;
            }
            include_path(root, parent_path);
        }
        if let Some(node) = node_at_mut(root, &row.path) {
            if !node.populated {
                populate(node, defs, 0);
            }
            node.expanded = true;
        }
        self.refresh();
    }

    /// Enter behavior: depends on context.
    ///   - On a oneOf variant child: select this variant.
    ///   - On any expandable: toggle expanded (peek without committing —
    ///     Space is the key that includes/excludes).
    ///   - Otherwise: noop.
    fn activate(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.is_variant_child {
            return self.select_variant(&row);
        }
        if !row.expandable {
            return;
        }
        let Self { root, defs, .. } = self;
        if let Some(node) = node_at_mut(root, &row.path) {
            if !node.populated {
                populate(node, defs, 0);
            }
            node.expanded = !node.expanded;
        }
        self.refresh();
    }

    /// Space: the universal "commit" key.
    ///   - On a oneOf variant child: pick this variant (same as Enter).
    ///   - On an optional field: toggle include.
    ///   - On a required field: no-op (already in).
    fn toggle_include(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.is_variant_child {
            return self.select_variant(&row);
        }
        if row.is_required {
            return;
        }
        let Self { root, defs, .. } = self;
        let mut now_included = false;
        if let Some(node) = node_at_mut(root, &row.path) {
            node.included = !node.included;
            now_included = node.included;
            if now_included && node.expandable {
                if !node.populated {
                    populate(node, defs, 0);
                }
                node.expanded = true;
            }
        }
        // Including a nested field must also pull in its ancestors, or the
        // export drops the whole subtree. Excluding leaves ancestors alone.
        if now_included {
            include_path(root, &row.path);
        }
        self.refresh();
    }

    fn collapse_or_up(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.expandable && row.expanded {
            if let Some(node) = node_at_mut(&mut self.root, &row.path) {
                node.expanded = false;
                // Inclusion stays as-is; ← is just "collapse the view".
            }
            self.refresh();
        } else if !row.path.is_empty() {
            let parent_path = &row.path[..row.path.len() - 1];
            if let Some(parent_idx) = self.rows.iter().position(|r| r.path == parent_path) {
                self.state.select(Some(parent_idx));
            }
        }
    }
}

fn split_path(path: &[usize]) -> Option<(&[usize], usize)> {
    let (last, rest) = path.split_last()?;
    Some((rest, *last))
}

fn run(terminal: &mut TerminalT, schema: RootSchema, title: String) -> Result<Outcome> {
    let mut app = App::new(schema, title);
    loop {
        terminal.draw(|f| draw(f, &mut app))?;
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match (key.code, key.modifiers) {
                (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Esc, _) => {
                    return Ok(Outcome::Cancel);
                }
                (KeyCode::Char('q'), _) => {
                    let value = app.root.placeholder_value();
                    return Ok(Outcome::Export(value));
                }
                (KeyCode::Down | KeyCode::Char('j'), _) => app.move_by(1),
                (KeyCode::Up | KeyCode::Char('k'), _) => app.move_by(-1),
                (KeyCode::PageDown, _) => app.move_by(10),
                (KeyCode::PageUp, _) => app.move_by(-10),
                (KeyCode::Char('g'), _) => app.jump_top(),
                (KeyCode::Char('G'), _) => app.jump_bottom(),
                (KeyCode::Enter | KeyCode::Right, _) => app.activate(),
                (KeyCode::Char(' '), _) => app.toggle_include(),
                (KeyCode::Left, _) => app.collapse_or_up(),
                _ => {}
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Rendering
// -----------------------------------------------------------------------------

fn draw(f: &mut ratatui::Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            "schema-tui  ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("· {}", app.title),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    f.render_widget(header, chunks[0]);

    let items: Vec<ListItem> = app.rows.iter().map(row_to_item).collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▎ ");
    f.render_stateful_widget(list, chunks[1], &mut app.state);

    let footer = Paragraph::new(Line::from(vec![
        Span::styled("↑↓", Style::default().fg(Color::Yellow)),
        Span::raw(" move   "),
        Span::styled("Enter", Style::default().fg(Color::Yellow)),
        Span::raw(" peek / pick variant   "),
        Span::styled("space", Style::default().fg(Color::Yellow)),
        Span::raw(" include in output   "),
        Span::styled("←", Style::default().fg(Color::Yellow)),
        Span::raw(" collapse   "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" export   "),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::raw(" cancel"),
    ]))
    .style(Style::default().fg(Color::DarkGray));
    f.render_widget(footer, chunks[2]);
}

fn build_marker(row: &Row) -> (String, Style) {
    if row.is_variant_child {
        let s = if row.is_selected_variant { "●  " } else { "○  " };
        let style = if row.is_selected_variant {
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        return (s.to_string(), style);
    }
    let arrow = if row.expandable {
        if row.expanded { "▼" } else { "▶" }
    } else {
        " "
    };
    // For optional fields, second column shows '+' when included.
    let include_mark = if row.is_required {
        " "
    } else if row.included {
        "+"
    } else {
        " "
    };
    let s = format!("{}{} ", arrow, include_mark);
    let style = if row.is_required {
        Style::default().fg(Color::DarkGray)
    } else if row.included {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    (s, style)
}

fn row_to_item(row: &Row) -> ListItem<'_> {
    let indent: String = "  ".repeat(row.depth as usize);

    // Two-column marker: [expand state][inclusion / variant pick].
    //   ▼+  expanded + included (will be in output)
    //   ▼   expanded but not included (peeking)
    //   ▶+  collapsed but included
    //   ▶   collapsed and not included
    //   ●   selected oneOf variant
    //   ○   unselected oneOf variant
    let (marker, marker_style) = build_marker(row);

    let label = if row.is_required || row.is_variant_child {
        row.label.clone()
    } else {
        format!("[{}]", row.label)
    };
    let label_style = if row.is_variant_child {
        if row.is_selected_variant {
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::DIM)
        }
    } else if row.is_required {
        Style::default().add_modifier(Modifier::BOLD)
    } else if row.included {
        Style::default()
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };

    let mut spans: Vec<Span> = vec![
        Span::raw(indent),
        Span::styled(marker, marker_style),
        Span::styled(label, label_style),
    ];

    if !row.type_desc.is_empty() {
        spans.push(Span::raw(": "));
        let type_style = if row.type_desc.starts_with('<') {
            Style::default().fg(Color::Cyan)
        } else if row.type_desc.starts_with('"') {
            Style::default().fg(Color::Green)
        } else {
            Style::default()
        };
        spans.push(Span::styled(row.type_desc.clone(), type_style));
    }

    for ann in &row.annotations {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            ann.clone(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if let Some(def) = &row.default {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("(default: {})", def),
            Style::default().fg(Color::DarkGray),
        ));
    }

    ListItem::new(Line::from(spans))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The default export (every required field, first `oneOf` variant) must
    // match the placeholder values produced by progenitor's `generate_value`:
    // empty string for strings, 0 for integers, the leading enum value for the
    // discriminator, and the first variant assembled for nested `oneOf`s.
    #[test]
    fn template_export_uses_progenitor_values() {
        let schema: RootSchema =
            serde_json::from_str(include_str!("../tests/fixtures/disk_create.json"))
                .expect("parse fixture schema");
        let root = build_root(&schema);
        assert_eq!(
            root.placeholder_value(),
            serde_json::json!({
                "description": "",
                "name": "",
                "size": 0,
                "disk_backend": { "type": "local" },
            })
        );
    }

    // schemars encodes `Option<T>` as `anyOf: [T, null]`. Such a field must be
    // seen through to its real type — classified as expandable and described as
    // the inner type — not rendered as an opaque "(variants)" leaf. (Regression
    // for the schemars-sourced schema in `oxide`, where optional oneOf fields
    // like `boot_disk`/`cpu_platform` showed no drilldown.)
    #[test]
    fn nullable_wrapper_is_seen_through() {
        let schema: RootSchema = serde_json::from_value(serde_json::json!({
            "title": "Demo",
            "type": "object",
            "required": [],
            "properties": {
                "boot_disk": {
                    "anyOf": [
                        { "$ref": "#/definitions/DiskAttach" },
                        { "type": "null" }
                    ]
                }
            },
            "definitions": {
                "DiskAttach": {
                    "oneOf": [
                        { "type": "object", "required": ["type"],
                          "properties": { "type": { "type": "string", "enum": ["a"] } } },
                        { "type": "object", "required": ["type"],
                          "properties": { "type": { "type": "string", "enum": ["b"] } } }
                    ]
                }
            }
        }))
        .unwrap();

        let defs = &schema.definitions;
        let props = &schema.schema.object.as_ref().unwrap().properties;
        let Schema::Object(boot_disk) = props.get("boot_disk").unwrap() else {
            panic!("boot_disk should be a schema object");
        };

        let (kind, expandable, _) = classify(boot_disk, defs);
        assert!(matches!(kind, NodeKind::OneOf), "should classify as OneOf");
        assert!(expandable, "nullable oneOf must be expandable");
        assert_eq!(describe_type(boot_disk), "<DiskAttach>");
    }

    // An array of objects is a drillable `Array` node: the user customizes one
    // representative element, and the export is `[element]` with the element's
    // required fields. A scalar array stays a flat leaf.
    #[test]
    fn array_of_objects_drills_into_element() {
        let schema: RootSchema = serde_json::from_value(serde_json::json!({
            "title": "Demo",
            "type": "object",
            "required": ["disks"],
            "properties": {
                "disks": { "type": "array", "items": { "$ref": "#/definitions/Disk" } }
            },
            "definitions": {
                "Disk": {
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "name": { "type": "string" },
                        "size": { "type": "integer", "format": "uint64" }
                    }
                }
            }
        }))
        .unwrap();

        let defs = &schema.definitions;
        let props = &schema.schema.object.as_ref().unwrap().properties;
        let Schema::Object(disks) = props.get("disks").unwrap() else {
            panic!("disks should be a schema object");
        };
        let (kind, expandable, _) = classify(disks, defs);
        assert!(matches!(kind, NodeKind::Array), "array of objects → Array");
        assert!(expandable);

        let root = build_root(&schema);
        assert_eq!(
            root.placeholder_value(),
            serde_json::json!({ "disks": [ { "name": "" } ] })
        );

        // A scalar array is NOT drillable — it stays a leaf with `[<scalar>]`.
        let scalar_arr: SchemaObject = serde_json::from_value(serde_json::json!({
            "type": "array", "items": { "type": "string" }
        }))
        .unwrap();
        let (kind, expandable, val) = classify(&scalar_arr, defs);
        assert!(matches!(kind, NodeKind::Leaf), "scalar array → Leaf");
        assert!(!expandable);
        assert_eq!(val, Some(serde_json::json!([""])));
    }

    // A `NameOrId`-shaped oneOf — arms are `{title, allOf:[scalar]}`, not tagged
    // objects or single-enum scalars. The arms must (a) be labeled by their
    // title (`id` / `name`), not "variant", and (b) export the scalar
    // placeholder, not an empty `{}`. (Regression for the array-of-NameOrId
    // fields like `anti_affinity_groups` / `ssh_public_keys`.)
    #[test]
    fn scalar_oneof_arms_get_real_labels_and_values() {
        let schema: RootSchema = serde_json::from_value(serde_json::json!({
            "title": "Demo", "type": "object", "required": ["who"],
            "properties": { "who": { "$ref": "#/definitions/NameOrId" } },
            "definitions": {
                "NameOrId": { "oneOf": [
                    { "title": "id", "allOf": [{ "type": "string", "format": "uuid" }] },
                    { "title": "name", "allOf": [{ "type": "string" }] }
                ]}
            }
        }))
        .unwrap();

        let mut app = App::new(schema, "t".into());
        // Expand the oneOf so its arms become visible rows.
        let who = app.rows.iter().position(|r| r.label == "who").unwrap();
        app.state.select(Some(who));
        app.activate();
        let labels: Vec<&str> = app
            .rows
            .iter()
            .filter(|r| r.is_variant_child)
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(labels, vec!["id", "name"], "arms labeled by title");

        // The default export picks the first arm and emits its scalar value
        // (empty string for a string leaf) — never `{}`.
        assert_eq!(
            app.root.placeholder_value(),
            serde_json::json!({ "who": "" })
        );
    }

    // Picking a variant nested inside an *optional* container must pull every
    // ancestor into the export. Otherwise the choice is silently dropped because
    // the outer optional array was never itself included — the bug behind
    // "everything I opened up exported as just the required fields".
    #[test]
    fn picking_nested_variant_includes_optional_ancestors() {
        let schema: RootSchema = serde_json::from_value(serde_json::json!({
            "title": "Demo", "type": "object", "required": [],
            "properties": {
                "anti_affinity_groups": {
                    "type": "array", "items": { "$ref": "#/definitions/NameOrId" }
                }
            },
            "definitions": {
                "NameOrId": { "oneOf": [
                    { "title": "id", "allOf": [{ "type": "string", "format": "uuid" }] },
                    { "title": "name", "allOf": [{ "type": "string" }] }
                ]}
            }
        }))
        .unwrap();

        let mut app = App::new(schema, "t".into());

        // Drill in without ever pressing Space on the array: expand the array,
        // expand its `[item]` element, then pick the `name` arm.
        let select = |app: &mut App, label: &str| {
            let i = app.rows.iter().position(|r| r.label == label).unwrap();
            app.state.select(Some(i));
        };
        select(&mut app, "anti_affinity_groups");
        app.activate();
        select(&mut app, "[item]");
        app.activate();
        select(&mut app, "name");
        app.activate();

        // The optional array was never Space-included, yet the deep pick must
        // carry it (and a real scalar value) into the output.
        assert_eq!(
            app.root.placeholder_value(),
            serde_json::json!({ "anti_affinity_groups": [""] })
        );
    }

    // Selecting (including) an optional array should pull in the required fields
    // of its element automatically.
    #[test]
    fn including_optional_array_pulls_in_required_element_fields() {
        let schema: RootSchema = serde_json::from_value(serde_json::json!({
            "title": "Demo", "type": "object", "required": [],
            "properties": {
                "groups": { "type": "array", "items": { "$ref": "#/definitions/Spec" } }
            },
            "definitions": {
                "Spec": { "type": "object", "required": ["group"], "properties": {
                    "group": { "type": "string" },
                    "ip_version": { "type": "string" }
                }}
            }
        }))
        .unwrap();

        let mut app = App::new(schema, "t".into());
        let groups_idx = app.rows.iter().position(|r| r.label == "groups").unwrap();
        app.state.select(Some(groups_idx));
        app.toggle_include();

        assert_eq!(
            app.root.placeholder_value(),
            serde_json::json!({ "groups": [ { "group": "" } ] })
        );
    }
}
