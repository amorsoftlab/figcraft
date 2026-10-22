//! Provider-neutral AI design boundary.
//!
//! Providers return a small, bounded design IR; this module turns it into regular FigCraft
//! commands. The provider never receives a privileged engine handle, and applying a result is one
//! undo step. Network access and credentials belong to the swappable UI/provider adapter.

use figcraft_color::Paint;
use figcraft_doc::{Document, Node, NodeId, NodeKind};
use serde_json::{Map, Value, json};

use crate::cmd::CommandSpec;
use crate::params::{get, num, string};
use crate::{Result, Session, bad, spec};

const CMD: &str = "ai.applyDesign";
const MAX_NODES: usize = 500;
const MAX_DEPTH: usize = 32;
const MAX_TEXT_CHARS: usize = 20_000;
const MAX_SIZE: f64 = 100_000.0;

/// A provider's editable document reduced to the deliberately small [`ai.applyDesign`](CMD) IR.
/// Unsupported imported layer kinds are omitted and reported rather than flattened to pixels.
pub struct ImportedDesign {
    pub ir: Value,
    pub warnings: Vec<String>,
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            CMD,
            "Apply generated design",
            "{name?, width, height, x?, y?, nodes?: [{type: frame|rectangle|ellipse|text, name?, x?, y?, width?, height?, text?, props?, children?}]} → {id, nodes}: validates a provider-neutral design IR and creates native editable layers as one undo step",
            false,
            apply
        ),
        spec!(
            "ai.open",
            "AI design",
            "{}  opens the optional AI design prompt in an interface; programmatic callers get no dialog",
            false,
            |s, _| {
                s.request_ui("dialog.ai", json!({}));
                Ok(json!({"requested": true}))
            }
        ),
    ]
}

fn finite_size(v: Option<f64>, default: f64, field: &str) -> Result<f64> {
    let v = v.unwrap_or(default);
    if !v.is_finite() || v <= 0.0 || v > MAX_SIZE {
        return Err(bad(CMD, format!("`{field}` must be a positive finite number no larger than {MAX_SIZE}")));
    }
    Ok(v)
}

fn finite_coord(v: Option<f64>, field: &str) -> Result<f64> {
    let v = v.unwrap_or(0.0);
    if !v.is_finite() || v.abs() > MAX_SIZE {
        return Err(bad(CMD, format!("`{field}` must be finite and between -{MAX_SIZE} and {MAX_SIZE}")));
    }
    Ok(v)
}

fn clean_text(value: Option<String>, field: &str, max: usize) -> Result<String> {
    let value = value.unwrap_or_default();
    if value.chars().count() > max || value.chars().any(|c| c == '\0') {
        return Err(bad(CMD, format!("`{field}` is too long or contains a null character")));
    }
    Ok(value)
}

fn solid(paints: &[Paint]) -> Option<String> {
    paints.iter().find(|p| p.visible).and_then(Paint::solid_color).map(|c| c.to_css_hex())
}

fn node_props(n: &Node) -> Map<String, Value> {
    let mut props = Map::new();
    if let Some(fill) = solid(&n.fills) {
        props.insert("fill".into(), json!(fill));
    }
    if let Some(stroke) = solid(&n.strokes) {
        props.insert("stroke".into(), json!(stroke));
        props.insert("strokeWeight".into(), json!(n.stroke.weight));
    }
    if n.opacity < 1.0 {
        props.insert("opacity".into(), json!(f64::from(n.opacity) * 100.0));
    }
    if n.corners.radii.iter().any(|r| *r > 0.0) {
        props.insert("cornerRadius".into(), json!(n.corners.radii));
    }
    if n.clip {
        props.insert("clipsContent".into(), json!(true));
    }
    if let NodeKind::Text(text) = &n.kind {
        props.insert("fontFamily".into(), json!(text.style.font_family));
        props.insert("fontStyle".into(), json!(text.style.font_style));
        props.insert("fontSize".into(), json!(text.style.font_size));
    }
    props
}

fn import_node(doc: &Document, id: NodeId, count: &mut usize, warnings: &mut Vec<String>) -> Option<Value> {
    if *count >= MAX_NODES {
        if !warnings.iter().any(|w| w.contains("layer limit")) {
            warnings.push(format!("generated design exceeded the {MAX_NODES}-layer limit; later layers were omitted"));
        }
        return None;
    }
    let n = doc.node(id)?;
    let kind = match n.kind {
        NodeKind::Frame
        | NodeKind::Group
        | NodeKind::Section { .. }
        | NodeKind::Component { .. }
        | NodeKind::ComponentSet { .. }
        | NodeKind::Instance { .. } => "frame",
        NodeKind::Rectangle => "rectangle",
        NodeKind::Ellipse { .. } => "ellipse",
        NodeKind::Text(_) => "text",
        _ => {
            warnings.push(format!("unsupported generated layer `{}` ({}) was omitted", n.name, n.kind.type_name()));
            return None;
        }
    };
    *count += 1;
    let c = n.transform.as_coeffs();
    if (c[0] - 1.0).abs() > 1e-6 || c[1].abs() > 1e-6 || c[2].abs() > 1e-6 || (c[3] - 1.0).abs() > 1e-6 {
        warnings.push(format!("generated layer `{}` had a rotated or scaled transform; its editable approximation keeps position and size", n.name));
    }
    let mut out = json!({
        "type": kind,
        "name": n.name,
        "x": c[4],
        "y": c[5],
        "width": n.width.max(1.0),
        "height": n.height.max(1.0),
        "props": node_props(n),
    });
    if let NodeKind::Text(text) = &n.kind {
        out["text"] = json!(text.characters);
    }
    if kind == "frame" {
        out["children"] = Value::Array(n.children.iter().filter_map(|child| import_node(doc, *child, count, warnings)).collect());
    }
    Some(out)
}

/// Convert the first page of an imported editable provider export to the bounded native AI IR.
/// A single top-level frame becomes the IR root; otherwise the page's layers become root children.
pub fn document_to_ir(doc: &Document, fallback_name: &str) -> Result<ImportedDesign> {
    let page_id = doc.pages.first().copied().ok_or_else(|| bad(CMD, "the generated design has no page"))?;
    let page = doc.node(page_id).ok_or_else(|| bad(CMD, "the generated design page is missing"))?;
    let sole = (page.children.len() == 1).then(|| page.children[0]).and_then(|id| doc.node(id));
    let (name, width, height, props, children): (String, f64, f64, Map<String, Value>, &[NodeId]) = match sole {
        Some(n) if n.kind.is_container() => (n.name.clone(), n.width, n.height, node_props(n), &n.children),
        _ => (fallback_name.to_string(), 390.0, 844.0, Map::new(), &page.children),
    };
    let mut warnings = Vec::new();
    let mut count = 0;
    let nodes = children.iter().filter_map(|id| import_node(doc, *id, &mut count, &mut warnings)).collect::<Vec<_>>();
    Ok(ImportedDesign { ir: json!({"name": name, "width": width.max(1.0), "height": height.max(1.0), "props": props, "nodes": nodes}), warnings })
}

fn add_nodes(nodes: &[Value], parent_step: usize, depth: usize, commands: &mut Vec<(String, Value)>) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(bad(CMD, format!("generated layers nest deeper than {MAX_DEPTH}")));
    }
    for node in nodes {
        if commands.len() > MAX_NODES {
            return Err(bad(CMD, format!("a generated design may contain at most {MAX_NODES} layers")));
        }
        let kind = string(node, "type").unwrap_or_default().to_ascii_lowercase();
        if !matches!(kind.as_str(), "frame" | "rectangle" | "ellipse" | "text") {
            return Err(bad(CMD, format!("unsupported generated layer type `{kind}`")));
        }
        let mut params = Map::new();
        params.insert("type".into(), json!(kind));
        params.insert("parent".into(), json!({"$step": parent_step}));
        params.insert("select".into(), json!(false));
        params.insert("x".into(), json!(finite_coord(num(node, "x"), "nodes[].x")?));
        params.insert("y".into(), json!(finite_coord(num(node, "y"), "nodes[].y")?));
        if let Some(name) = get(node, "name") {
            let name = clean_text(name.as_str().map(str::to_string), "nodes[].name", 256)?;
            params.insert("name".into(), json!(name));
        }
        if kind == "text" {
            let text = clean_text(string(node, "text"), "nodes[].text", MAX_TEXT_CHARS)?;
            params.insert("text".into(), json!(text));
            if let Some(width) = num(node, "width") {
                params.insert("width".into(), json!(finite_size(Some(width), 0.0, "nodes[].width")?));
            }
        } else {
            params.insert("width".into(), json!(finite_size(num(node, "width"), 100.0, "nodes[].width")?));
            params.insert("height".into(), json!(finite_size(num(node, "height"), 100.0, "nodes[].height")?));
        }
        if let Some(props) = get(node, "props") {
            if !props.is_object() {
                return Err(bad(CMD, "`nodes[].props` must be an object"));
            }
            params.insert("props".into(), props.clone());
        }
        let step = commands.len();
        commands.push(("node.create".into(), Value::Object(params)));
        match get(node, "children") {
            Some(Value::Array(children)) if kind == "frame" => add_nodes(children, step, depth + 1, commands)?,
            Some(Value::Array(children)) if children.is_empty() => {}
            Some(Value::Array(_)) => return Err(bad(CMD, "only generated frames may have `children`")),
            Some(_) => return Err(bad(CMD, "`nodes[].children` must be an array")),
            None => {}
        }
    }
    Ok(())
}

fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    let width = finite_size(num(p, "width"), 390.0, "width")?;
    let height = finite_size(num(p, "height"), 844.0, "height")?;
    let name = clean_text(string(p, "name").or_else(|| Some("AI design".into())), "name", 256)?;
    let mut root = json!({
        "type": "frame",
        "name": name,
        "width": width,
        "height": height,
        "select": true,
        "props": {"clipsContent": true}
    });
    if let Some(Value::Object(props)) = get(p, "props") {
        root["props"] = Value::Object(props.clone());
        root["props"]["clipsContent"] = json!(true);
    } else if get(p, "props").is_some() {
        return Err(bad(CMD, "`props` must be an object"));
    }
    if let Some(x) = num(p, "x") {
        root["x"] = json!(finite_coord(Some(x), "x")?);
    }
    if let Some(y) = num(p, "y") {
        root["y"] = json!(finite_coord(Some(y), "y")?);
    }
    let mut commands = vec![("node.create".into(), root)];
    match get(p, "nodes") {
        Some(Value::Array(nodes)) => add_nodes(nodes, 0, 1, &mut commands)?,
        Some(_) => return Err(bad(CMD, "`nodes` must be an array")),
        None => {}
    }
    let count = commands.len();
    let results = s.batch("Generate design", &commands)?;
    let id = results.first().and_then(|v| v.get("id")).cloned().unwrap_or(Value::Null);
    Ok(json!({"id": id, "nodes": count}))
}

#[cfg(test)]
mod tests {
    use figcraft_doc::NodeKind;
    use serde_json::json;

    use crate::Session;

    #[test]
    fn generated_ir_makes_native_editable_layers_in_one_step() {
        let mut s = Session::new();
        let before = s.ds().unwrap().history.undo.len();
        let result = s
            .execute(
                super::CMD,
                &json!({
                    "name": "Banking",
                    "width": 390,
                    "height": 844,
                    "nodes": [
                        {"type": "rectangle", "name": "Card", "x": 24, "y": 120, "width": 342, "height": 160,
                         "props": {"cornerRadius": 16, "fill": "#FFFFFF"}},
                        {"type": "text", "name": "Balance", "x": 40, "y": 144, "text": "$12,450", "props": {"fontSize": 28}}
                    ]
                }),
            )
            .unwrap();
        assert_eq!(result["nodes"], 3);
        assert_eq!(s.ds().unwrap().history.undo.len(), before + 1);
        let root = s.selection()[0];
        let node = s.doc().unwrap().node(root).unwrap();
        assert!(matches!(node.kind, NodeKind::Frame));
        assert_eq!(node.children.len(), 2);
        s.undo().unwrap();
        assert!(s.doc().unwrap().node(root).is_none());
    }

    #[test]
    fn generated_ir_is_bounded_and_rejects_unknown_layers() {
        let mut s = Session::new();
        assert!(s.execute(super::CMD, &json!({"width": 390, "height": 844, "nodes": [{"type": "video"}]})).is_err());
        assert!(s.execute(super::CMD, &json!({"width": 100_001, "height": 844})).is_err());
        assert!(s.execute(super::CMD, &json!({"nodes": [{"type": "text", "text": "x", "children": [{}]}]})).is_err());
    }

    #[test]
    fn imported_editable_document_becomes_provider_neutral_ir() {
        let mut source = Session::new();
        let frame = source
            .execute("node.create", &json!({"type":"frame","name":"Home","x":20,"y":30,"width":390,"height":844,"props":{"fill":"#F5F5F5"}}))
            .unwrap()["id"]
            .as_u64()
            .map(figcraft_doc::NodeId)
            .unwrap();
        source.execute("text.create", &json!({"parent":frame,"x":24,"y":40,"text":"Welcome","props":{"fontSize":28,"fill":"#111111"}})).unwrap();
        let converted = super::document_to_ir(source.doc().unwrap(), "Generated").unwrap();
        assert_eq!(converted.ir["name"], "Home");
        assert_eq!(converted.ir["width"], 390.0);
        assert_eq!(converted.ir["nodes"][0]["type"], "text");
        assert_eq!(converted.ir["nodes"][0]["text"], "Welcome");
        assert!(converted.warnings.is_empty());

        let mut target = Session::new();
        target.execute(super::CMD, &converted.ir).unwrap();
        let root = target.selection()[0];
        assert_eq!(target.doc().unwrap().node(root).unwrap().name, "Home");
    }
}
