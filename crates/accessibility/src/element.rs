//! Small AX attribute helpers + AX tree debugger (PRD §25).
//!
//! Every accessor is fallible-tolerant: Notification Center's AX structure
//! differs between macOS versions, so a missing attribute is `None`, never a
//! fatal error.

use axuielement::AXUIElement;

/// Best-effort string attribute read. Returns `None` on missing value or AX error.
pub fn get_string(element: &AXUIElement, attribute: &str) -> Option<String> {
    element
        .string_attribute(attribute)
        .ok()
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Recursively collect visible text under `element` (depth-limited).
///
/// Walks `AXChildren`, gathering `AXTitle` / `AXValue` / `AXDescription` from
/// `AXStaticText`-like roles. Caps at `max_nodes` to bound pathological trees.
pub fn collect_texts(element: &AXUIElement, max_depth: usize) -> Vec<String> {
    let mut out = Vec::new();
    collect_texts_inner(element, 0, max_depth, &mut out, &mut 0);
    out
}

fn collect_texts_inner(
    element: &AXUIElement,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<String>,
    visited: &mut usize,
) {
    if depth > max_depth || *visited > 500 {
        return;
    }
    *visited += 1;

    let role = get_string(element, "AXRole").unwrap_or_default();
    if role == "AXStaticText" || role == "AXTextField" || role == "AXHeading" {
        for attr in ["AXValue", "AXTitle", "AXDescription"] {
            if let Some(text) = get_string(element, attr) {
                if !out.contains(&text) {
                    out.push(text);
                }
                break;
            }
        }
    }

    let Ok(children) = element.children() else {
        return;
    };
    for child in children.iter().take(50) {
        collect_texts_inner(child, depth + 1, max_depth, out, visited);
    }
}

/// Render the AX subtree as an indented tree for the diagnostics screen.
///
/// ```text
/// AXApplication
/// └── AXWindow
///     └── AXGroup
///         └── AXNotificationCenterBanner
/// ```
pub fn dump_tree(element: &AXUIElement, max_depth: usize) -> String {
    let mut out = String::new();
    dump_tree_inner(element, 0, max_depth, &mut out, &mut 0);
    if out.is_empty() {
        out.push_str("(empty AX tree — permission may be missing or process gone)\n");
    }
    out
}

fn dump_tree_inner(
    element: &AXUIElement,
    depth: usize,
    max_depth: usize,
    out: &mut String,
    visited: &mut usize,
) {
    if depth > max_depth || *visited > 300 {
        if *visited > 300 {
            out.push_str(&format!(
                "{}… (truncated at 300 nodes)\n",
                "    ".repeat(depth.min(8))
            ));
        }
        return;
    }
    *visited += 1;

    let indent = if depth == 0 {
        String::new()
    } else {
        format!("{}└── ", "    ".repeat(depth - 1))
    };
    let role = get_string(element, "AXRole").unwrap_or_else(|| "?".to_string());
    let mut extras = Vec::new();
    if let Some(subrole) = get_string(element, "AXSubrole") {
        extras.push(format!("subrole={subrole}"));
    }
    if let Some(id) = get_string(element, "AXIdentifier") {
        extras.push(format!("id={id}"));
    }
    if let Some(title) = get_string(element, "AXTitle") {
        let short: String = title.chars().take(60).collect();
        extras.push(format!("title={short:?}"));
    }
    let suffix = if extras.is_empty() {
        String::new()
    } else {
        format!(" [{}]", extras.join(" "))
    };
    out.push_str(&format!("{indent}{role}{suffix}\n"));

    let Ok(children) = element.children() else {
        return;
    };
    for child in children.iter().take(30) {
        dump_tree_inner(child, depth + 1, max_depth, out, visited);
    }
}
