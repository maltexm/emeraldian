//! Reading Mermaid sources: what a flowchart says, and where its boxes go.
//!
//! Mermaid has twenty-odd diagram types; only the flowchart family (`flowchart`
//! and `graph`) is drawn here, so anything else is refused with a short reason
//! and the caller shows the source instead. What comes out is geometry in cell
//! coordinates — columns across, rows down — and turning that into braille dots
//! is the renderer's problem.

use unicode_width::UnicodeWidthStr;

/// Which way a flow runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    TopDown,
    BottomUp,
    LeftRight,
    RightToLeft,
}

/// The outline a node is drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeShape {
    Box,
    Diamond,
    Circle,
}

/// The line an edge is drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeStyle {
    Solid,
    Dotted,
    Thick,
    /// A plain line with no arrowheads at all (`---`).
    Open,
}

/// A node of the graph, by id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub shape: NodeShape,
}

/// One connection between two nodes, by index into [`Flow::nodes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub label: Option<String>,
    pub style: EdgeStyle,
    pub arrow_start: bool,
    pub arrow_end: bool,
}

/// A parsed flowchart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flow {
    pub direction: Direction,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// A node where the layout put it, in cell coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub shape: NodeShape,
    pub label: String,
    /// The label as it is set in the node, wrapped.
    pub lines: Vec<String>,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The polyline of an edge, in cell coordinates, with an optional label.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub points: Vec<(f64, f64)>,
    pub style: EdgeStyle,
    pub arrow_start: bool,
    pub arrow_end: bool,
    pub label: Option<(String, f64, f64)>,
}

/// A whole flowchart laid out: everything on it, and how big it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagram {
    pub width: f64,
    pub height: f64,
    pub nodes: Vec<Placed>,
    pub edges: Vec<Link>,
}

/// Most nodes and edges that are drawn; a bigger source stays source. Checked
/// as each is added, so a hostile line is refused before it has been built.
const MAX_NODES: usize = 150;
const MAX_EDGES: usize = 300;

fn too_large() -> String {
    format!("too large to draw: more than {MAX_NODES} nodes or {MAX_EDGES} edges")
}
/// Longest a node's label runs before it wraps onto another line, in columns.
const WRAP: usize = 32;

/// Diagram types Mermaid has that are not drawn here.
const OTHER_TYPES: [&str; 22] = [
    "sequencediagram",
    "classdiagram",
    "statediagram",
    "statediagram-v2",
    "erdiagram",
    "journey",
    "gantt",
    "pie",
    "quadrantchart",
    "requirementdiagram",
    "gitgraph",
    "mindmap",
    "timeline",
    "sankey-beta",
    "xychart-beta",
    "block-beta",
    "packet-beta",
    "architecture-beta",
    "c4context",
    "zenuml",
    "kanban",
    "radar-beta",
];

/// Statements that style or wire a diagram without adding to its shape.
const IGNORED: [&str; 9] = [
    "classdef",
    "class",
    "style",
    "linkstyle",
    "click",
    "direction",
    "acctitle",
    "accdescr",
    "title",
];

/// Reads a flowchart source.
///
/// # Errors
///
/// A short reason when the source is not a flowchart emeraldian draws — a
/// diagram type it does not support, a broken statement, nothing at all.
pub fn parse(source: &str) -> Result<Flow, String> {
    let statements = statements(source);
    let mut statements = statements.iter().map(String::as_str);
    let header = statements.next().ok_or("the diagram is empty")?;

    let mut words = header.split_whitespace();
    let kind = words.next().unwrap_or_default();
    let lowered = kind.to_lowercase();
    if !matches!(lowered.as_str(), "flowchart" | "graph" | "flowchart-elk") {
        return Err(if OTHER_TYPES.contains(&lowered.as_str()) {
            format!("{kind} diagrams are not drawn yet, only flowchart and graph")
        } else {
            "not a flowchart: the first line should be `flowchart TD` or `graph LR`".into()
        });
    }
    let direction = match words.next().map(str::to_uppercase).as_deref() {
        None | Some("TD" | "TB") => Direction::TopDown,
        Some("BT") => Direction::BottomUp,
        Some("LR") => Direction::LeftRight,
        Some("RL") => Direction::RightToLeft,
        Some(other) => return Err(format!("unknown direction {other}")),
    };

    let mut flow = Flow {
        direction,
        nodes: Vec::new(),
        edges: Vec::new(),
    };
    for statement in statements {
        let first = statement
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_lowercase();
        if first == "subgraph" || first == "end" {
            return Err("subgraph is not drawn yet".into());
        }
        if IGNORED.contains(&first.as_str()) && statement.contains(char::is_whitespace) {
            continue;
        }
        statement_into(statement, &mut flow)?;
    }
    if flow.nodes.is_empty() {
        return Err("the flowchart has no nodes".into());
    }
    Ok(flow)
}

/// The statements of a source: comments and front matter gone, lines split at
/// `;` outside brackets and quotes, blanks dropped.
fn statements(source: &str) -> Vec<String> {
    let mut lines = source.lines().peekable();
    // YAML front matter (`---` … `---`), which Mermaid reads its title from.
    if lines.peek().is_some_and(|line| line.trim() == "---") {
        lines.next();
        for line in lines.by_ref() {
            if line.trim() == "---" {
                break;
            }
        }
    }
    let mut out = Vec::new();
    for line in lines {
        let line = line.find("%%").map_or(line, |at| &line[..at]);
        let (mut depth, mut quoted) = (0_i32, false);
        let mut current = String::new();
        for c in line.chars() {
            match c {
                '"' => quoted = !quoted,
                '[' | '(' | '{' if !quoted => depth += 1,
                ']' | ')' | '}' if !quoted => depth -= 1,
                ';' if !quoted && depth <= 0 => {
                    out.push(std::mem::take(&mut current));
                    continue;
                }
                _ => {}
            }
            current.push(c);
        }
        out.push(current);
    }
    out.into_iter()
        .map(|statement| statement.trim().to_string())
        .filter(|statement| !statement.is_empty())
        .collect()
}

/// One statement: `A --> B & C -->|x| D`, or just `A[Label]`.
fn statement_into(statement: &str, flow: &mut Flow) -> Result<(), String> {
    let chars: Vec<char> = statement.chars().collect();
    let mut at = 0;
    let mut left = group(&chars, &mut at, flow)?;
    loop {
        skip_spaces(&chars, &mut at);
        if at >= chars.len() {
            return Ok(());
        }
        let mut written = link(&chars, &mut at)?;
        skip_spaces(&chars, &mut at);
        if chars.get(at) == Some(&'|') {
            let close = chars[at + 1..]
                .iter()
                .position(|&c| c == '|')
                .ok_or("an edge label is missing its closing |")?;
            let text: String = chars[at + 1..at + 1 + close].iter().collect();
            written.label = Some(clean(&text));
            at += close + 2;
            skip_spaces(&chars, &mut at);
        }
        if at >= chars.len() {
            return Err("an edge has no node at its end".into());
        }
        let right = group(&chars, &mut at, flow)?;
        // Before the product is built, not after: `A & A & … --> B & B & …`.
        if flow.edges.len() + left.len() * right.len() > MAX_EDGES {
            return Err(too_large());
        }
        if let Some(&same) = left.iter().find(|from| right.contains(from)) {
            return Err(format!(
                "{} linked to itself is not drawn yet",
                flow.nodes[same].id
            ));
        }
        for &from in &left {
            for &to in &right {
                flow.edges.push(Edge {
                    from,
                    to,
                    label: written.label.clone().filter(|label| !label.is_empty()),
                    style: written.style,
                    arrow_start: written.arrow_start,
                    arrow_end: written.arrow_end,
                });
            }
        }
        left = right;
    }
}

fn skip_spaces(chars: &[char], at: &mut usize) {
    while chars.get(*at).is_some_and(|c| c.is_whitespace()) {
        *at += 1;
    }
}

/// `A & B & C`: the nodes a link can start or end at.
fn group(chars: &[char], at: &mut usize, flow: &mut Flow) -> Result<Vec<usize>, String> {
    let mut ids = vec![node(chars, at, flow)?];
    loop {
        let mut look = *at;
        skip_spaces(chars, &mut look);
        if chars.get(look) != Some(&'&') {
            return Ok(ids);
        }
        look += 1;
        skip_spaces(chars, &mut look);
        *at = look;
        ids.push(node(chars, at, flow)?);
        if ids.len() > MAX_EDGES {
            return Err(too_large());
        }
    }
}

/// Opening and closing brackets of the node shapes, longest opener first.
const SHAPES: [(&str, &[&str], NodeShape); 12] = [
    ("(((", &[")))"], NodeShape::Circle),
    ("((", &["))"], NodeShape::Circle),
    ("([", &["])"], NodeShape::Box),
    ("(", &[")"], NodeShape::Box),
    ("[[", &["]]"], NodeShape::Box),
    ("[(", &[")]"], NodeShape::Box),
    ("[/", &["/]", "\\]"], NodeShape::Box),
    ("[\\", &["\\]", "/]"], NodeShape::Box),
    ("[", &["]"], NodeShape::Box),
    ("{{", &["}}"], NodeShape::Box),
    ("{", &["}"], NodeShape::Diamond),
    (">", &["]"], NodeShape::Box),
];

/// One node: an id, and the label and shape if this is where it is spelled out.
fn node(chars: &[char], at: &mut usize, flow: &mut Flow) -> Result<usize, String> {
    let start = *at;
    while chars
        .get(*at)
        .is_some_and(|&c| c.is_alphanumeric() || c == '_')
    {
        *at += 1;
    }
    if *at == start {
        let rest: String = chars[start..].iter().take(20).collect();
        return Err(format!("cannot read `{rest}` as a node"));
    }
    let id: String = chars[start..*at].iter().collect();

    // Only a node with a shape written after it needs the rest of the line.
    let rest: String = if matches!(chars.get(*at), Some('(' | '[' | '{' | '>')) {
        chars[*at..].iter().collect()
    } else {
        String::new()
    };
    let mut spelled = None;
    for (open, closes, shape) in SHAPES {
        let Some(body) = rest.strip_prefix(open) else {
            continue;
        };
        let (label, used) = match body.strip_prefix('"') {
            Some(quoted) => {
                let end = quoted
                    .find('"')
                    .ok_or_else(|| format!("unclosed quote in the label of {id}"))?;
                let after = &quoted[end + 1..];
                let close = closes
                    .iter()
                    .find(|close| after.starts_with(**close))
                    .ok_or_else(|| format!("unclosed {open} in node {id}"))?;
                (
                    quoted[..end].to_string(),
                    open.len() + 1 + end + 1 + close.len(),
                )
            }
            None => {
                let (end, close) = closes
                    .iter()
                    .filter_map(|close| body.find(close).map(|end| (end, *close)))
                    .min_by_key(|(end, _)| *end)
                    .ok_or_else(|| format!("unclosed {open} in node {id}"))?;
                (body[..end].to_string(), open.len() + end + close.len())
            }
        };
        *at += rest[..used].chars().count();
        spelled = Some((clean(&label), shape));
        break;
    }
    // `A:::highlight` hangs a style class on the node.
    if chars[*at..].starts_with(&[':', ':', ':']) {
        *at += 3;
        while chars
            .get(*at)
            .is_some_and(|&c| c.is_alphanumeric() || c == '_' || c == '-')
        {
            *at += 1;
        }
    }

    let index = flow
        .nodes
        .iter()
        .position(|node| node.id == id)
        .unwrap_or_else(|| {
            flow.nodes.push(Node {
                label: id.clone(),
                id,
                shape: NodeShape::Box,
            });
            flow.nodes.len() - 1
        });
    if flow.nodes.len() > MAX_NODES {
        return Err(too_large());
    }
    if let Some((label, shape)) = spelled {
        flow.nodes[index].label = label;
        flow.nodes[index].shape = shape;
    }
    Ok(index)
}

/// A label as it is shown: `<br/>` becomes a space, quotes and the space
/// round them go.
fn clean(label: &str) -> String {
    let mut text = label.trim().to_string();
    for br in ["<br/>", "<br />", "<br>", "<BR/>", "<BR>"] {
        text = text.replace(br, " ");
    }
    text.trim().trim_matches('"').trim().to_string()
}

/// A link as it is written, before it knows which nodes it joins.
struct Written {
    label: Option<String>,
    style: EdgeStyle,
    arrow_start: bool,
    arrow_end: bool,
}

/// `-->`, `---`, `-.->`, `==>`, `--x`, `<-->`, and the `-- text -->` forms.
fn link(chars: &[char], at: &mut usize) -> Result<Written, String> {
    let get = |i: usize| chars.get(i).copied();
    let mut i = *at;
    let mut written = Written {
        label: None,
        style: EdgeStyle::Solid,
        arrow_start: false,
        arrow_end: false,
    };
    match get(i) {
        Some('<') => {
            written.arrow_start = true;
            i += 1;
        }
        // Cross and circle ends have no arrowhead drawn.
        Some('x' | 'o') if matches!(get(i + 1), Some('-' | '=')) => i += 1,
        _ => {}
    }

    let bad = |from: usize| {
        let rest: String = chars[from.min(chars.len())..].iter().take(20).collect();
        format!("cannot read `{rest}` as an edge")
    };
    // Text written into the line: everything up to the next run of `mark`.
    let text_until = |from: usize, mark: &[char]| -> Option<(String, usize)> {
        let end = (from..chars.len()).find(|&j| chars[j..].starts_with(mark))?;
        Some((clean(&chars[from..end].iter().collect::<String>()), end))
    };
    let run = |mut j: usize, c: char| {
        while chars.get(j) == Some(&c) {
            j += 1;
        }
        j
    };

    let mut open = false;
    match get(i) {
        Some('-') if get(i + 1) == Some('.') => {
            written.style = EdgeStyle::Dotted;
            i = run(i + 1, '.');
            if get(i) == Some(' ') {
                let (text, end) = text_until(i, &['.', '-']).ok_or_else(|| bad(*at))?;
                written.label = Some(text);
                i = run(end, '.');
            }
            if get(i) != Some('-') {
                return Err(bad(*at));
            }
            i += 1;
        }
        Some('=') => {
            written.style = EdgeStyle::Thick;
            let end = run(i, '=');
            if end - i == 2 && get(end) == Some(' ') {
                let (text, stop) = text_until(end, &['=', '=']).ok_or_else(|| bad(*at))?;
                written.label = Some(text);
                i = run(stop, '=');
            } else if end - i >= 2 {
                open = end - i >= 3;
                i = end;
            } else {
                return Err(bad(*at));
            }
        }
        Some('-') => {
            let end = run(i, '-');
            if end - i == 2 && get(end) == Some(' ') {
                let (text, stop) = text_until(end, &['-', '-']).ok_or_else(|| bad(*at))?;
                written.label = Some(text);
                let after = run(stop, '-');
                open = after - stop >= 3;
                i = after;
            } else if end - i >= 2 {
                open = end - i >= 3;
                i = end;
            } else {
                return Err(bad(*at));
            }
        }
        _ => return Err(bad(*at)),
    }

    // The end: an arrowhead, a cross or circle, or nothing.
    let boundary = |j: usize| get(j).is_none_or(|c| c.is_whitespace() || c == '|');
    match get(i) {
        Some('>') => {
            written.arrow_end = true;
            i += 1;
        }
        Some('x' | 'o') if boundary(i + 1) => {
            i += 1;
            open = false;
        }
        _ if open || written.style != EdgeStyle::Solid => {}
        _ => return Err(bad(*at)),
    }
    if open && !written.arrow_end && written.style == EdgeStyle::Solid {
        written.style = EdgeStyle::Open;
    }
    *at = i;
    Ok(written)
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// A vertex of the layered graph: a node, or the place an edge passes a rank
/// it skips.
struct Vertex {
    /// The node it is, or `None` for a dummy.
    node: Option<usize>,
    rank: usize,
    /// Extent along the flow, and across it, in cells.
    along: usize,
    across: usize,
    /// Position of its near corner along and across the flow.
    at_along: usize,
    at_across: usize,
}

/// An edge cut into hops of one rank each.
struct Chain {
    /// Vertices from the lower rank to the higher.
    path: Vec<usize>,
    /// The edge was written the other way and is drawn reversed.
    reversed: bool,
}

/// Wraps a label at word boundaries into lines of at most [`WRAP`] columns.
fn wrap(label: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in label.split_whitespace() {
        match lines.last_mut() {
            Some(line) if UnicodeWidthStr::width(line.as_str()) + 1 + word.width() <= WRAP => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Places every node and routes every edge of a parsed flowchart.
#[must_use]
pub fn layout(flow: &Flow) -> Diagram {
    let vertical = matches!(flow.direction, Direction::TopDown | Direction::BottomUp);

    // -- Sizes ---------------------------------------------------------------
    let lines: Vec<Vec<String>> = flow.nodes.iter().map(|node| wrap(&node.label)).collect();
    let sizes: Vec<(usize, usize)> = flow
        .nodes
        .iter()
        .zip(&lines)
        .map(|(node, lines)| {
            let text = lines
                .iter()
                .map(|line| UnicodeWidthStr::width(line.as_str()))
                .max()
                .unwrap_or(0);
            let width = text + if node.shape == NodeShape::Box { 4 } else { 6 };
            (width, lines.len() + 2)
        })
        .collect();

    // -- Ranks ---------------------------------------------------------------
    let count = flow.nodes.len();
    let (forward, reversed) = split_cycles(flow);
    let mut rank = vec![0_usize; count];
    {
        let mut indegree = vec![0_usize; count];
        let mut out: Vec<Vec<usize>> = vec![Vec::new(); count];
        for &(from, to) in &forward {
            indegree[to] += 1;
            out[from].push(to);
        }
        let mut ready: Vec<usize> = (0..count).filter(|&n| indegree[n] == 0).collect();
        ready.reverse();
        while let Some(n) = ready.pop() {
            for &next in &out[n] {
                rank[next] = rank[next].max(rank[n] + 1);
                indegree[next] -= 1;
                if indegree[next] == 0 {
                    ready.push(next);
                }
            }
        }
    }
    let ranks = rank.iter().max().map_or(1, |max| max + 1);

    // -- Vertices and chains -------------------------------------------------
    let mut vertices: Vec<Vertex> = (0..count)
        .map(|n| Vertex {
            node: Some(n),
            rank: rank[n],
            along: if vertical { sizes[n].1 } else { sizes[n].0 },
            across: if vertical { sizes[n].0 } else { sizes[n].1 },
            at_along: 0,
            at_across: 0,
        })
        .collect();
    // One per edge, in the order of the edges (`None`: a self loop).
    let mut chains: Vec<Option<Chain>> = Vec::with_capacity(flow.edges.len());
    for (index, edge) in flow.edges.iter().enumerate() {
        let is_reversed = reversed.contains(&index);
        let (low, high) = if is_reversed {
            (edge.to, edge.from)
        } else {
            (edge.from, edge.to)
        };
        if edge.from == edge.to || rank[low] >= rank[high] {
            chains.push(None);
            continue;
        }
        let mut path = vec![low];
        for between in rank[low] + 1..rank[high] {
            vertices.push(Vertex {
                node: None,
                rank: between,
                along: 1,
                across: 1,
                at_along: 0,
                at_across: 0,
            });
            path.push(vertices.len() - 1);
        }
        path.push(high);
        chains.push(Some(Chain {
            path,
            reversed: is_reversed,
        }));
    }

    // -- Order within ranks --------------------------------------------------
    let mut rows: Vec<Vec<usize>> = vec![Vec::new(); ranks];
    for (v, vertex) in vertices.iter().enumerate() {
        rows[vertex.rank].push(v);
    }
    let mut up: Vec<Vec<usize>> = vec![Vec::new(); vertices.len()];
    let mut down: Vec<Vec<usize>> = vec![Vec::new(); vertices.len()];
    for chain in chains.iter().flatten() {
        for hop in chain.path.windows(2) {
            down[hop[0]].push(hop[1]);
            up[hop[1]].push(hop[0]);
        }
    }
    let mut slot = vec![0_usize; vertices.len()];
    let reindex = |rows: &[Vec<usize>], slot: &mut [usize]| {
        for row in rows {
            for (i, &v) in row.iter().enumerate() {
                slot[v] = i;
            }
        }
    };
    reindex(&rows, &mut slot);
    for sweep in 0..6 {
        let downwards = sweep % 2 == 0;
        let indices: Vec<usize> = if downwards {
            (1..ranks).collect()
        } else {
            (0..ranks.saturating_sub(1)).rev().collect()
        };
        for r in indices {
            let neighbours = if downwards { &up } else { &down };
            let mut keyed: Vec<(f64, usize)> = rows[r]
                .iter()
                .map(|&v| {
                    let near = &neighbours[v];
                    let key = if near.is_empty() {
                        slot[v] as f64
                    } else {
                        near.iter().map(|&n| slot[n] as f64).sum::<f64>() / near.len() as f64
                    };
                    (key, v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
            rows[r] = keyed.into_iter().map(|(_, v)| v).collect();
            reindex(&rows, &mut slot);
        }
    }

    // -- Position across the flow --------------------------------------------
    let apart = if vertical { 3 } else { 2 };
    for row in &rows {
        let mut next = 0;
        for &v in row {
            vertices[v].at_across = next;
            next += vertices[v].across + apart;
        }
    }
    for round in 0..8 {
        let downwards = round % 2 == 0;
        let indices: Vec<usize> = if downwards {
            (1..ranks).collect()
        } else {
            (0..ranks.saturating_sub(1)).rev().collect()
        };
        for r in indices {
            let neighbours = if downwards { &up } else { &down };
            let wanted: Vec<Option<usize>> = rows[r]
                .iter()
                .map(|&v| {
                    let near = &neighbours[v];
                    (!near.is_empty()).then(|| {
                        // Twice the centre it would like: the mean of its
                        // neighbours' centres, kept doubled to stay whole.
                        let sum: usize = near
                            .iter()
                            .map(|&n| 2 * vertices[n].at_across + vertices[n].across)
                            .sum();
                        sum / near.len()
                    })
                })
                .collect();
            settle(&rows[r], &wanted, &mut vertices, apart);
        }
    }

    // -- Position along the flow ---------------------------------------------
    // The widest label on an edge whose labelled hop leaves rank `r`.
    let label_room = |r: usize| -> usize {
        flow.edges
            .iter()
            .zip(&chains)
            .filter_map(|(edge, chain)| {
                let chain = chain.as_ref()?;
                let label = edge.label.as_ref()?;
                (vertices[chain.path[label_hop(chain)]].rank == r)
                    .then(|| UnicodeWidthStr::width(label.as_str()))
            })
            .max()
            .unwrap_or(0)
    };
    let mut band_start = vec![0_usize; ranks];
    let mut band_end = vec![0_usize; ranks];
    let mut cursor = 0;
    for r in 0..ranks {
        let thickness = rows[r]
            .iter()
            .filter(|&&v| vertices[v].node.is_some())
            .map(|&v| vertices[v].along)
            .max()
            .unwrap_or(1);
        band_start[r] = cursor;
        band_end[r] = cursor + thickness;
        let gap = match (vertical, label_room(r)) {
            (true, 0) => 3,
            (true, _) => 4,
            (false, 0) => 6,
            (false, width) => (width + 4).max(6),
        };
        cursor = band_end[r] + gap;
    }
    let extent = band_end[ranks - 1];
    for vertex in &mut vertices {
        vertex.at_along = band_start[vertex.rank];
        if vertex.node.is_none() {
            vertex.along = band_end[vertex.rank] - band_start[vertex.rank];
        }
    }

    // -- Mapping to the page -------------------------------------------------
    let flip = |along: usize| -> usize {
        match flow.direction {
            Direction::TopDown | Direction::LeftRight => along,
            Direction::BottomUp | Direction::RightToLeft => extent - 1 - along,
        }
    };
    // A cell given as (along, across), as (x, y).
    let cell = |along: usize, across: usize| -> (i64, i64) {
        let along = flip(along) as i64;
        if vertical {
            (across as i64, along)
        } else {
            (along, across as i64)
        }
    };
    // The top left cell of a block given as (along, across) and its extents.
    let corner = |along: usize, across: usize, d_along: usize| -> (i64, i64) {
        let far = along + d_along - 1;
        let first = flip(along).min(flip(far));
        if vertical {
            (across as i64, first as i64)
        } else {
            (first as i64, across as i64)
        }
    };

    let mut placed: Vec<Placed> = Vec::with_capacity(count);
    for n in 0..count {
        let vertex = &vertices[n];
        let (x, y) = corner(vertex.at_along, vertex.at_across, vertex.along);
        placed.push(Placed {
            shape: flow.nodes[n].shape,
            label: flow.nodes[n].label.clone(),
            lines: lines[n].clone(),
            x: x as f64,
            y: y as f64,
            width: sizes[n].0 as f64,
            height: sizes[n].1 as f64,
        });
    }

    // -- Routing -------------------------------------------------------------
    // A way back leaves and enters a roomy node one cell off its centre, so its
    // head is not mistaken for the end of the stem the way forward grows from.
    let port = |v: usize, back: bool| {
        let offset = usize::from(back && vertices[v].node.is_some() && vertices[v].across >= 5);
        vertices[v].at_across + vertices[v].across / 2 + offset
    };
    let mut links: Vec<Link> = Vec::with_capacity(flow.edges.len());
    for (edge, chain) in flow.edges.iter().zip(&chains) {
        let Some(chain) = chain else {
            links.push(Link {
                points: Vec::new(),
                style: edge.style,
                arrow_start: edge.arrow_start,
                arrow_end: edge.arrow_end,
                label: None,
            });
            continue;
        };
        // Points as (along, across), from the first free cell beside the upper
        // node to the border cell of the lower one.
        let first = chain.path[0];
        let mut points: Vec<(usize, usize)> = vec![(
            vertices[first].at_along + vertices[first].along,
            port(first, chain.reversed),
        )];
        let mut label_at = None;
        let hops = chain.path.len() - 1;
        let labelled = label_hop(chain);
        for (h, hop) in chain.path.windows(2).enumerate() {
            let jog = band_end[vertices[hop[0]].rank] + 1;
            points.push((jog, port(hop[0], chain.reversed)));
            points.push((jog, port(hop[1], chain.reversed)));
            if h == labelled {
                label_at = Some((jog + 1, port(hop[1], chain.reversed)));
            }
            if h + 1 == hops {
                points.push((vertices[hop[1]].at_along, port(hop[1], chain.reversed)));
            }
        }
        let mut concrete: Vec<(i64, i64)> = points.iter().map(|&(a, c)| cell(a, c)).collect();
        concrete.dedup();
        simplify(&mut concrete);
        if chain.reversed {
            concrete.reverse();
            // Now it begins on the lower node's border and ends beside the
            // upper one: the start steps in to the first free cell, the end
            // out onto the border.
            if concrete.len() >= 2 {
                let n = concrete.len();
                concrete[0] = outwards(concrete[0], concrete[1], -1);
                concrete[n - 1] = outwards(concrete[n - 1], concrete[n - 2], 1);
                concrete.dedup();
                simplify(&mut concrete);
            }
        }
        let label = edge.label.as_ref().zip(label_at).map(|(text, (a, c))| {
            let width = UnicodeWidthStr::width(text.as_str());
            let (x, y) = if vertical {
                corner(a, c + 2, 1)
            } else {
                // Above the run into the target, from the cell after the jog.
                let near = corner(a, c.saturating_sub(1), width);
                (near.0, near.1)
            };
            (text.clone(), x as f64, y as f64)
        });
        links.push(Link {
            points: concrete
                .into_iter()
                .map(|(x, y)| (x as f64, y as f64))
                .collect(),
            style: edge.style,
            arrow_start: edge.arrow_start,
            arrow_end: edge.arrow_end,
            label,
        });
    }

    finish(placed, links)
}

/// The hop of a chain that carries its label: the one that enters the node the
/// edge points at.
fn label_hop(chain: &Chain) -> usize {
    if chain.reversed {
        0
    } else {
        chain.path.len() - 2
    }
}

/// A point moved one cell along the line from `neighbour` through it (`1`:
/// further from the neighbour), or back toward the neighbour (`-1`).
fn outwards(point: (i64, i64), neighbour: (i64, i64), sign: i64) -> (i64, i64) {
    (
        point.0 + sign * (point.0 - neighbour.0).signum(),
        point.1 + sign * (point.1 - neighbour.1).signum(),
    )
}

/// Drops the middle point of any three on one line.
fn simplify(points: &mut Vec<(i64, i64)>) {
    let mut i = 1;
    while i + 1 < points.len() {
        let (a, b, c) = (points[i - 1], points[i], points[i + 1]);
        if (a.0 == b.0 && b.0 == c.0) || (a.1 == b.1 && b.1 == c.1) {
            points.remove(i);
        } else {
            i += 1;
        }
    }
}

/// Splits edges into those that point down the ranks, as `(from, to)`, and the
/// indices of those that close a cycle and are drawn reversed. Self loops are
/// in neither.
fn split_cycles(flow: &Flow) -> (Vec<(usize, usize)>, std::collections::HashSet<usize>) {
    let count = flow.nodes.len();
    let mut out: Vec<Vec<(usize, usize)>> = vec![Vec::new(); count];
    for (index, edge) in flow.edges.iter().enumerate() {
        if edge.from != edge.to {
            out[edge.from].push((edge.to, index));
        }
    }
    // 0 unseen, 1 on the stack, 2 done.
    let mut state = vec![0_u8; count];
    let mut back = std::collections::HashSet::new();
    for root in 0..count {
        if state[root] != 0 {
            continue;
        }
        let mut stack = vec![(root, 0_usize)];
        state[root] = 1;
        while let Some((node, next)) = stack.last().copied() {
            if let Some(&(to, index)) = out[node].get(next) {
                if let Some(top) = stack.last_mut() {
                    top.1 += 1;
                }
                match state[to] {
                    0 => {
                        state[to] = 1;
                        stack.push((to, 0));
                    }
                    1 => {
                        back.insert(index);
                    }
                    _ => {}
                }
            } else {
                state[node] = 2;
                stack.pop();
            }
        }
    }
    let forward = flow
        .edges
        .iter()
        .enumerate()
        .filter(|(index, edge)| edge.from != edge.to && !back.contains(index))
        .map(|(_, edge)| (edge.from, edge.to))
        .collect();
    (forward, back)
}

/// Moves the vertices of one rank toward where their neighbours want them
/// (`wanted` is twice the centre), keeping their order and their distance.
fn settle(row: &[usize], wanted: &[Option<usize>], vertices: &mut [Vertex], apart: usize) {
    let n = row.len();
    let desired: Vec<i64> = row
        .iter()
        .zip(wanted)
        .map(|(&v, wanted)| match wanted {
            Some(twice) => (*twice as i64 - vertices[v].across as i64) / 2,
            None => vertices[v].at_across as i64,
        })
        .collect();
    let (mut left, mut right) = (vec![0_i64; n], vec![0_i64; n]);
    let mut edge = i64::MIN / 4;
    for i in 0..n {
        left[i] = desired[i].max(edge);
        edge = left[i] + (vertices[row[i]].across + apart) as i64;
    }
    let mut edge = i64::MAX / 4;
    for i in (0..n).rev() {
        right[i] = desired[i].min(edge - vertices[row[i]].across as i64 - apart as i64);
        edge = right[i];
    }
    // Two valid placements; their mean keeps the order and the room between.
    for i in 0..n {
        let mean = (left[i] + right[i]).div_euclid(2);
        vertices[row[i]].at_across = mean.max(0) as usize;
    }
    // Squeezing at zero can leave neighbours too close: push right to fix.
    let mut edge = 0_usize;
    for &v in row {
        vertices[v].at_across = vertices[v].at_across.max(edge);
        edge = vertices[v].at_across + vertices[v].across + apart;
    }
}

/// Puts the page's top left at the origin and measures it.
fn finish(mut nodes: Vec<Placed>, mut edges: Vec<Link>) -> Diagram {
    let mut min = (f64::MAX, f64::MAX);
    let mut max = (0.0_f64, 0.0_f64);
    let mut note = |x0: f64, y0: f64, x1: f64, y1: f64| {
        min = (min.0.min(x0), min.1.min(y0));
        max = (max.0.max(x1), max.1.max(y1));
    };
    for node in &nodes {
        note(node.x, node.y, node.x + node.width, node.y + node.height);
    }
    for link in &edges {
        for &(x, y) in &link.points {
            note(x, y, x + 1.0, y + 1.0);
        }
        if let Some((text, x, y)) = &link.label {
            let width = UnicodeWidthStr::width(text.as_str()) as f64;
            note(*x, *y, x + width, y + 1.0);
        }
    }
    if min.0 > max.0 {
        return Diagram {
            width: 0.0,
            height: 0.0,
            nodes,
            edges,
        };
    }
    let (dx, dy) = (-min.0, -min.1);
    for node in &mut nodes {
        node.x += dx;
        node.y += dy;
    }
    for link in &mut edges {
        for point in &mut link.points {
            point.0 += dx;
            point.1 += dy;
        }
        if let Some((_, x, y)) = &mut link.label {
            *x += dx;
            *y += dy;
        }
    }
    Diagram {
        width: max.0 + dx,
        height: max.1 + dy,
        nodes,
        edges,
    }
}

/// A source laid out in one step.
///
/// # Errors
///
/// As [`parse`].
pub fn diagram(source: &str) -> Result<Diagram, String> {
    Ok(layout(&parse(source)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow(source: &str) -> Flow {
        parse(source).expect("a flowchart")
    }

    fn label_of<'a>(diagram: &'a Diagram, id: &str) -> &'a Placed {
        diagram
            .nodes
            .iter()
            .find(|node| node.label == id)
            .unwrap_or_else(|| panic!("no node labelled {id} in {diagram:?}"))
    }

    // -- Reading -------------------------------------------------------------

    #[test]
    fn a_flowchart_gives_up_its_nodes_edges_and_direction() {
        let parsed = flow("flowchart LR\n  A[Vault] --> B{Ok?}\n  B --> C[Reader]");
        assert_eq!(parsed.direction, Direction::LeftRight);
        assert_eq!(
            parsed
                .nodes
                .iter()
                .map(|node| (node.label.as_str(), node.shape))
                .collect::<Vec<_>>(),
            vec![
                ("Vault", NodeShape::Box),
                ("Ok?", NodeShape::Diamond),
                ("Reader", NodeShape::Box),
            ]
        );
        assert_eq!(parsed.edges.len(), 2);
        assert_eq!((parsed.edges[0].from, parsed.edges[0].to), (0, 1));
        assert_eq!((parsed.edges[1].from, parsed.edges[1].to), (1, 2));
        assert!(parsed.edges.iter().all(|edge| edge.arrow_end));
    }

    #[test]
    fn a_node_with_no_label_of_its_own_is_named_after_its_id() {
        let parsed = flow("graph TD\n  A --> B");
        assert_eq!(parsed.nodes[0].label, "A");
        assert_eq!(parsed.nodes[1].label, "B");
    }

    #[test]
    fn edges_carry_their_labels_and_line_styles() {
        let parsed = flow(
            "flowchart TD\n\
             A -->|yes| B\n\
             A -- maybe --> C\n\
             A -.-> D\n\
             A ==> E\n\
             A --- F\n\
             A --x G\n\
             A --o H",
        );
        let label = |to: usize| {
            parsed
                .edges
                .iter()
                .find(|edge| edge.to == to)
                .and_then(|edge| edge.label.clone())
        };
        let style = |to: usize| {
            parsed
                .edges
                .iter()
                .find(|edge| edge.to == to)
                .map(|edge| edge.style)
                .expect("the edge")
        };
        assert_eq!(label(1), Some("yes".to_string()));
        assert_eq!(label(2), Some("maybe".to_string()));
        assert_eq!(style(3), EdgeStyle::Dotted);
        assert_eq!(style(4), EdgeStyle::Thick);
        assert_eq!(style(5), EdgeStyle::Open);
        let cross = parsed
            .edges
            .iter()
            .find(|edge| edge.to == 6)
            .expect("A --x G");
        assert!(!cross.arrow_end, "an x is not an arrowhead");
        let open = parsed
            .edges
            .iter()
            .find(|edge| edge.to == 7)
            .expect("A --o H");
        assert!(!open.arrow_end);
    }

    #[test]
    fn several_nodes_can_be_reached_from_one() {
        let parsed = flow("flowchart LR\n  A --> B & C\n  D --> A");
        assert_eq!(parsed.edges.len(), 3);
        assert_eq!(
            parsed
                .edges
                .iter()
                .map(|edge| (edge.from, edge.to))
                .collect::<Vec<_>>(),
            vec![(0, 1), (0, 2), (3, 0)]
        );
    }

    #[test]
    fn shapes_without_a_drawing_of_their_own_stay_boxes() {
        let parsed = flow(
            "flowchart TD\n\
             A[[Subroutine]]\n\
             B[(Database)]\n\
             C>Asymmetric]\n\
             D([Stadium])\n\
             E{{Hexagon}}\n\
             F[/Parallelogram/]",
        );
        let shapes: Vec<_> = parsed.nodes.iter().map(|node| node.shape).collect();
        assert_eq!(
            shapes,
            vec![
                NodeShape::Box,
                NodeShape::Box,
                NodeShape::Box,
                NodeShape::Box,
                NodeShape::Box,
                NodeShape::Box
            ],
            "only the diamond and the circle have outlines of their own: {shapes:?}"
        );
        assert_eq!(parsed.nodes[1].label, "Database");
        assert_eq!(
            parsed.nodes[4].label, "Hexagon",
            "and they still give labels"
        );
    }

    #[test]
    fn a_label_keeps_its_punctuation_and_quotes_around_it() {
        let parsed = flow(
            r##"flowchart TD
             A["A (nested) label, and <b>bold</b>"]
             B["Two<br/>lines"]"##,
        );
        assert_eq!(parsed.nodes[0].label, "A (nested) label, and <b>bold</b>");
        assert_eq!(parsed.nodes[1].label, "Two lines");
    }

    #[test]
    fn comments_directives_and_theme_lines_are_ignored() {
        let parsed = flow(
            "%%{init: {'theme':'dark'}}%%\n\
             %% a whole comment line\n\
             flowchart TD\n\
             classDef ok fill:#d3f9d8\n\
             class A ok\n\
             style B fill:#e7f5ff\n\
             linkStyle 0 stroke:#333\n\
             A --> B %% trailing note\n\
             click A \"https://example.com\"",
        );
        assert_eq!(parsed.nodes.len(), 2);
        assert_eq!(parsed.edges.len(), 1);
    }

    #[test]
    fn anything_that_is_not_a_flowchart_is_refused_with_a_reason() {
        let reason = |source: &str| parse(source).expect_err("not a flowchart");
        assert!(
            reason("sequenceDiagram\n  A->>B: hi").contains("sequenceDiagram"),
            "the type it doesn't draw is named: {}",
            reason("sequenceDiagram\n  A->>B: hi")
        );
        assert!(reason("classDiagram\n  A *-- B").contains("classDiagram"));
        assert!(reason("stateDiagram-v2\n  [*] --> S").contains("stateDiagram-v2"));
        assert!(reason("# just a note\n").contains("flowchart"));
        assert!(reason("").contains("empty"));
    }

    #[test]
    fn an_unfinished_statement_is_an_error_not_a_partial_diagram() {
        let reason = |source: &str| parse(source).expect_err("not complete");
        assert!(reason("flowchart TD\n  A -->").contains("edge"));
        assert!(reason("flowchart TD\n  A[unclosed").contains("["));
        assert!(reason("flowchart TD\n  subgraph one\n  A --> B\n  end").contains("subgraph"));
    }

    // -- Placing -------------------------------------------------------------

    #[test]
    fn children_sit_below_their_parents_in_a_top_down_flow() {
        let diagram = layout(&flow(
            "flowchart TD\n  A[Root] --> B[Left]\n  A --> C[Right]",
        ));
        let (root, left, right) = (
            label_of(&diagram, "Root"),
            label_of(&diagram, "Left"),
            label_of(&diagram, "Right"),
        );
        assert!(left.y > root.y + root.height, "{left:?} below {root:?}");
        assert!(right.y > root.y + root.height);
        assert_eq!(left.y, right.y, "siblings share a row: {left:?} {right:?}");
        assert!(
            left.x < right.x,
            "and are told apart across it: {left:?} {right:?}"
        );
        assert!(
            diagram.width >= right.x + right.width,
            "both are on the page"
        );
        assert!(diagram.height >= left.y + left.height);
    }

    #[test]
    fn children_sit_after_their_parents_left_to_right() {
        let diagram = layout(&flow("flowchart LR\n  A[Start] --> B[Done]"));
        let (start, done) = (label_of(&diagram, "Start"), label_of(&diagram, "Done"));
        assert!(
            done.x >= start.x + start.width,
            "{done:?} is not after {start:?}"
        );
        assert_eq!(start.y, done.y, "a straight run stays level");
    }

    #[test]
    fn a_right_to_left_flow_runs_backwards() {
        let diagram = layout(&flow("flowchart RL\n  A[Start] --> B[Done]"));
        let (start, done) = (label_of(&diagram, "Start"), label_of(&diagram, "Done"));
        assert!(
            done.x + done.width <= start.x,
            "{done:?} is left of {start:?}"
        );

        let upward = layout(&flow("flowchart BT\n  A[Start] --> B[Done]"));
        let (start, done) = (label_of(&upward, "Start"), label_of(&upward, "Done"));
        assert!(
            done.y + done.height <= start.y,
            "{done:?} is above {start:?}"
        );
    }

    #[test]
    fn every_node_has_room_for_its_label() {
        let diagram = layout(&flow(
            "flowchart TD\n\
             A[short]\n\
             B[a considerably longer label]\n\
             C{decision}",
        ));
        for node in &diagram.nodes {
            let label = UnicodeWidthStr::width(node.label.as_str()) as f64;
            assert!(
                node.width >= label + 2.0,
                "{:?} cannot hold {label} characters in {}",
                node.label,
                node.width
            );
            assert!(
                node.height >= 3.0,
                "{:?} needs a border: {}",
                node.label,
                node.height
            );
        }
        let short = label_of(&diagram, "short");
        let long = label_of(&diagram, "a considerably longer label");
        assert!(long.width > short.width, "a long label makes a wide box");
        let diamond = label_of(&diagram, "decision");
        assert_eq!(diamond.shape, NodeShape::Diamond);
    }

    #[test]
    fn edges_reach_the_nodes_they_connect_in_right_angled_segments() {
        let diagram = layout(&flow(
            "flowchart TD\n  A[Root] --> B[Left]\n  A --> C[Right]\n  B --> C",
        ));
        assert_eq!(diagram.edges.len(), 3);
        for link in &diagram.edges {
            assert!(link.points.len() >= 2, "{link:?}");
            for pair in link.points.windows(2) {
                let horizontal = pair[0].1 == pair[1].1;
                let vertical = pair[0].0 == pair[1].0;
                assert!(
                    horizontal || vertical,
                    "an edge is routed as corners, not a diagonal: {pair:?}"
                );
            }
        }
        // The first edge leaves A's bottom border and lands on B's top border.
        let root = label_of(&diagram, "Root");
        let left = label_of(&diagram, "Left");
        let first = &diagram.edges[0];
        let (start, end) = (first.points[0], first.points[first.points.len() - 1]);
        assert_eq!(
            start.1,
            root.y + root.height,
            "leaves the parent's underside"
        );
        assert!(
            (root.x..=root.x + root.width).contains(&start.0),
            "and from within it: {start:?}"
        );
        assert_eq!(end.1, left.y, "and lands on the child's lid: {end:?}");
        assert!(
            (left.x..=left.x + left.width).contains(&end.0),
            "{end:?} within {left:?}"
        );
    }

    #[test]
    fn an_edge_label_sits_between_the_two_nodes_it_joins() {
        let diagram = layout(&flow("flowchart TD\n  A -->|yes| B\n  C -->|no| D"));
        let link = diagram
            .edges
            .iter()
            .find(|link| link.label.as_ref().is_some_and(|(text, ..)| text == "yes"))
            .expect("the labelled edge");
        let (text, x, y) = link.label.clone().expect("a label");
        assert_eq!(text, "yes");
        let (a, b) = (label_of(&diagram, "A"), label_of(&diagram, "B"));
        assert!(
            y > a.y + a.height && y < b.y,
            "between the rows of A and B: {y} vs {}..{}",
            a.y + a.height,
            b.y
        );
        assert!(
            x > link.points[0].0 && x <= b.x + b.width,
            "right of the line, still across the nodes it joins: {x}"
        );
    }

    #[test]
    fn a_cycle_lays_out_inside_the_page_it_reports() {
        let diagram = layout(&flow("flowchart TD\n  A --> B\n  B --> C\n  C --> A"));
        assert_eq!(diagram.nodes.len(), 3);
        assert!(diagram.width.is_finite() && diagram.height.is_finite());
        for node in &diagram.nodes {
            assert!(
                node.x >= 0.0 && node.y >= 0.0,
                "{node:?} starts on the page"
            );
            assert!(
                node.x + node.width <= diagram.width && node.y + node.height <= diagram.height,
                "{node:?} fits in {}x{}",
                diagram.width,
                diagram.height
            );
        }
    }

    #[test]
    fn every_edge_starts_beside_a_node_and_ends_on_one_even_when_it_runs_backwards() {
        let diagram = layout(&flow(
            "flowchart TD\n  A --> B\n  B --> C\n  C --> A\n  B --> A",
        ));
        let inside = |(x, y): (f64, f64)| {
            diagram.nodes.iter().any(|node| {
                x >= node.x && x < node.x + node.width && y >= node.y && y < node.y + node.height
            })
        };
        for link in &diagram.edges {
            let (first, last) = (link.points[0], link.points[link.points.len() - 1]);
            assert!(!inside(first), "starts in free space: {link:?}");
            assert!(inside(last), "ends on a node's border: {link:?}");
            let on_border = diagram.nodes.iter().any(|node| {
                let (x, y) = last;
                inside(last)
                    && (y == node.y
                        || y == node.y + node.height - 1.0
                        || x == node.x
                        || x == node.x + node.width - 1.0)
            });
            assert!(on_border, "not just somewhere inside: {link:?}");
        }
    }

    #[test]
    fn a_way_back_uses_a_port_of_its_own_beside_the_way_forward() {
        let diagram = layout(&flow(
            "flowchart TD\n  A[Start node] --> B[Stop node]\n  B --> A",
        ));
        let (forward, back) = (&diagram.edges[0], &diagram.edges[1]);
        assert_ne!(
            forward.points[0].0,
            back.points[back.points.len() - 1].0,
            "the head into Start is not on the stem out of it: {diagram:?}"
        );
        assert_ne!(
            forward.points[forward.points.len() - 1].0,
            back.points[0].0,
            "and the same at Stop"
        );
    }

    #[test]
    fn a_huge_group_is_refused_before_it_is_expanded() {
        // 2000 x 2000 would be 4 million edges if built and counted after.
        let side = |name: &str| vec![name; 2000].join(" & ");
        let source = format!("flowchart TD\n{} --> {}", side("A"), side("B"));
        let started = std::time::Instant::now();
        let reason = parse(&source).expect_err("far too many edges");
        assert!(reason.contains("too large"), "{reason}");
        assert!(
            started.elapsed().as_millis() < 100,
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_long_chain_in_one_statement_is_refused_as_fast_as_many_short_ones() {
        for source in [
            // Ten thousand distinct nodes, then two nodes taking turns ten thousand times.
            format!(
                "flowchart TD\n{}",
                (0..10_000)
                    .map(|n| format!("N{n}"))
                    .collect::<Vec<_>>()
                    .join(" --> ")
            ),
            format!(
                "flowchart TD\n{}",
                (0..10_000)
                    .map(|n| if n % 2 == 0 { "A" } else { "B" })
                    .collect::<Vec<_>>()
                    .join(" --> ")
            ),
        ] {
            let started = std::time::Instant::now();
            let reason = parse(&source).expect_err("far too big");
            assert!(reason.contains("too large"), "{reason}");
            assert!(
                started.elapsed().as_millis() < 500,
                "{:?} for {} bytes",
                started.elapsed(),
                source.len()
            );
        }
    }

    #[test]
    fn a_node_linked_to_itself_is_refused_with_a_reason_not_drawn_without_its_loop() {
        let reason = parse("flowchart TD\n  A[Again] -->|retry| A").expect_err("a self loop");
        assert!(reason.contains("itself"), "{reason}");
        assert!(
            parse("flowchart TD\n  A --> B & A").is_err(),
            "inside a group too"
        );
        assert!(parse("flowchart TD\n  A --> B --> B").is_err());
    }

    #[test]
    fn a_source_lays_out_in_one_step() {
        let laid = diagram("flowchart LR\n  A[One] --> B[Two]").expect("drawn");
        assert_eq!(laid.nodes.len(), 2);
        assert!(laid.width > 0.0 && laid.height > 0.0);
        assert!(diagram("sequenceDiagram\nA->>B: x").is_err());
    }
}
