//! Drawing a Mermaid flowchart, rather than printing its source.
//!
//! The design round hands back a `flowchart TD` block. Printed as code it is
//! eight lines of `A[Clients] -->|HTTPS| N[Nginx x2]`, which is a diagram
//! somebody else has to render — and mid-interview nobody renders anything.
//! The block still has its copy button for Excalidraw; this is so the panel
//! shows a picture without being asked.
//!
//! Only the subset jay's own prompt asks for is parsed: `flowchart TD` or `LR`,
//! rectangular nodes, and edges with optional labels. Anything not understood
//! falls back to the source text, because a wrong diagram is worse than a
//! listing and there is no way to tell the reader which they are looking at.

use eframe::egui;

/// A node, in declaration order.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Flowchart {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// Top-down, as against left-right. Everything jay emits is top-down; `LR`
    /// is parsed so a hand-written one does not silently come out sideways.
    pub top_down: bool,
}

/// Parse the subset. `None` for anything else, including an empty graph.
pub fn parse(source: &str) -> Option<Flowchart> {
    let mut lines = source.lines().map(str::trim).filter(|l| !l.is_empty());
    let header = lines.next()?;
    let top_down = match header.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["flowchart" | "graph", dir] => match *dir {
            "TD" | "TB" => true,
            "LR" | "RL" => false,
            _ => return None,
        },
        _ => return None,
    };

    let mut chart = Flowchart {
        nodes: Vec::new(),
        edges: Vec::new(),
        top_down,
    };

    for line in lines {
        // Comments and styling directives are ignored rather than rejected:
        // they change nothing about the shape and refusing the whole diagram
        // over a `%%` comment would be perverse.
        if line.starts_with("%%") || line.starts_with("style ") || line.starts_with("classDef ") {
            continue;
        }
        let (left, arrow_rest) = line.split_once("-->")?;
        let (label, right) = match arrow_rest.trim_start().strip_prefix('|') {
            Some(rest) => {
                let (label, right) = rest.split_once('|')?;
                (label.trim().to_string(), right)
            }
            None => (String::new(), arrow_rest),
        };

        let from = chart.intern(left.trim())?;
        let to = chart.intern(right.trim())?;
        chart.edges.push(Edge { from, to, label });
    }

    (!chart.nodes.is_empty()).then_some(chart)
}

impl Flowchart {
    /// Find or create the node this fragment names.
    ///
    /// A fragment is either `A[Some label]` on first mention or bare `A`
    /// afterwards, and both must land on the same node — jay emits the second
    /// form constantly, since a node with three edges is declared once.
    fn intern(&mut self, fragment: &str) -> Option<usize> {
        let (id, label) = match fragment.find(['[', '(', '{']) {
            Some(open) => {
                let id = fragment[..open].trim();
                let inner = fragment[open..]
                    .trim_matches(|c| matches!(c, '[' | ']' | '(' | ')' | '{' | '}'))
                    .trim()
                    .trim_matches('"');
                (id, Some(inner.to_string()))
            }
            None => (fragment.trim(), None),
        };
        if id.is_empty() || id.contains(char::is_whitespace) {
            return None;
        }

        match self.nodes.iter().position(|n| n.id == id) {
            Some(existing) => {
                // A later declaration with a label wins over a bare mention.
                if let Some(label) = label {
                    self.nodes[existing].label = label;
                }
                Some(existing)
            }
            None => {
                self.nodes.push(Node {
                    id: id.to_string(),
                    label: label.unwrap_or_else(|| id.to_string()),
                });
                Some(self.nodes.len() - 1)
            }
        }
    }

    /// Which rank each node sits on: one past its deepest parent.
    ///
    /// Longest path rather than shortest, so an edge never points backwards or
    /// sideways within a rank. Cycles cannot hang it — the iteration is capped
    /// at the node count, which is the most any acyclic graph needs.
    pub fn ranks(&self) -> Vec<usize> {
        let mut rank = vec![0usize; self.nodes.len()];
        for _ in 0..self.nodes.len() {
            let mut moved = false;
            for edge in &self.edges {
                if edge.from != edge.to && rank[edge.to] < rank[edge.from] + 1 {
                    rank[edge.to] = rank[edge.from] + 1;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        rank
    }
}

/// Box size and spacing, in points at the panel's usual width.
const NODE_W: f32 = 132.0;
const NODE_H: f32 = 40.0;
const RANK_GAP: f32 = 62.0;
const NODE_GAP: f32 = 16.0;

/// Draw `chart`, returning the space it took.
///
/// Laid out by rank: each rank is a row, spread evenly across the width. Not a
/// clever layout — no crossing minimisation, no edge routing — because the
/// prompt caps the diagram at eight nodes and at that size the naive placement
/// is legible and anything smarter is a week of work nobody asked for.
pub fn draw(
    ui: &mut egui::Ui,
    chart: &Flowchart,
    ink: egui::Color32,
    line: egui::Color32,
    fill: egui::Color32,
    faint: egui::Color32,
) {
    let ranks = chart.ranks();
    let depth = ranks.iter().copied().max().unwrap_or(0) + 1;

    // Nodes per rank, in declaration order, which keeps the drawing in the
    // order the answer reads.
    let mut rows: Vec<Vec<usize>> = vec![Vec::new(); depth];
    for (i, rank) in ranks.iter().enumerate() {
        rows[*rank].push(i);
    }
    let widest = rows.iter().map(Vec::len).max().unwrap_or(1);

    let available = ui.available_width();
    let wanted = widest as f32 * (NODE_W + NODE_GAP);
    // Shrink to fit rather than scroll sideways: a diagram you have to pan is
    // not a diagram you can glance at.
    let scale = (available / wanted).clamp(0.45, 1.0);
    let (node_w, node_h) = (NODE_W * scale, NODE_H * scale);
    let (rank_gap, node_gap) = (RANK_GAP * scale, NODE_GAP * scale);

    let height = depth as f32 * node_h + (depth.saturating_sub(1)) as f32 * rank_gap;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height + 8.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);

    // Centre of every node, so edges can be drawn before or after the boxes.
    let mut centres = vec![egui::Pos2::ZERO; chart.nodes.len()];
    for (rank, row) in rows.iter().enumerate() {
        let span = row.len() as f32 * node_w + (row.len().saturating_sub(1)) as f32 * node_gap;
        let mut x = rect.center().x - span / 2.0 + node_w / 2.0;
        let y = rect.top() + 4.0 + rank as f32 * (node_h + rank_gap) + node_h / 2.0;
        for node in row {
            centres[*node] = egui::pos2(x, y);
            x += node_w + node_gap;
        }
    }

    // Where a label has already been written, so the next one can be moved
    // out of the way. Two edges between the same pair of boxes land on exactly
    // the same midpoint otherwise, and "create paste" over "GET slug" reads as
    // "cr6ETesphgte" — observed, in the first drawing this ever produced.
    let mut taken: Vec<egui::Rect> = Vec::new();

    // Edges first, so the boxes sit on top of the lines rather than under them.
    for (index, edge) in chart.edges.iter().enumerate() {
        let (a, b) = (centres[edge.from], centres[edge.to]);
        // Parallel edges are fanned apart at both ends, so two arrows between
        // the same pair are two visible lines rather than one drawn twice.
        let parallel = chart.edges[..index]
            .iter()
            .filter(|e| e.from == edge.from && e.to == edge.to)
            .count() as f32;
        let fan = parallel * 9.0 * scale;
        let from = egui::pos2(a.x + fan, a.y + node_h / 2.0);
        let to = egui::pos2(b.x + fan, b.y - node_h / 2.0);
        painter.line_segment([from, to], egui::Stroke::new(1.0, line));

        // Arrowhead, drawn by hand: egui has no arrow primitive and a triangle
        // is three points.
        let dir = (to - from).normalized();
        let side = egui::vec2(-dir.y, dir.x) * 3.5;
        let base = to - dir * 7.0;
        painter.add(egui::Shape::convex_polygon(
            vec![to, base + side, base - side],
            line,
            egui::Stroke::NONE,
        ));

        if !edge.label.is_empty() {
            let font = egui::FontId::monospace(9.0 * scale.max(0.8));
            let galley = painter.layout_no_wrap(edge.label.clone(), font.clone(), faint);
            let mut at = from + (to - from) * 0.5;

            // Slide down the line until clear of every label already placed.
            // Down rather than sideways: the gap between two ranks is empty
            // and the space beside an edge usually is not.
            let step = galley.size().y + 2.0;
            for _ in 0..8 {
                let rect = egui::Rect::from_center_size(at, galley.size())
                    .expand2(egui::vec2(2.0, 0.0));
                if !taken.iter().any(|other| other.intersects(rect)) {
                    taken.push(rect);
                    break;
                }
                at.y += step;
            }

            painter.galley(
                at - galley.size() / 2.0,
                galley,
                faint,
            );
        }
    }

    for (i, node) in chart.nodes.iter().enumerate() {
        let box_rect = egui::Rect::from_center_size(centres[i], egui::vec2(node_w, node_h));
        painter.rect_filled(box_rect, 2.0, fill);
        painter.rect_stroke(
            box_rect,
            2.0,
            egui::Stroke::new(1.0, line),
            egui::StrokeKind::Inside,
        );
        painter.text(
            centres[i],
            egui::Align2::CENTER_CENTER,
            &node.label,
            egui::FontId::monospace(11.0 * scale.max(0.8)),
            ink,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = "flowchart TD\n\
        C[Clients] -->|HTTPS| N[Nginx x2]\n\
        N -->|create paste| A[App servers x2]\n\
        N -->|GET slug| A\n\
        A -->|insert row| P[Postgres primary]\n\
        P -->|streaming replication| R[Postgres replica]";

    #[test]
    fn parses_what_the_design_round_actually_emits() {
        let chart = parse(REAL).expect("should parse");
        assert!(chart.top_down);
        assert_eq!(
            chart.nodes.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(),
            ["Clients", "Nginx x2", "App servers x2", "Postgres primary", "Postgres replica"]
        );
        assert_eq!(chart.edges.len(), 5);
        assert_eq!(chart.edges[1].label, "create paste");
    }

    /// A node with several edges is declared once and referred to bare after,
    /// which jay does on every diagram.
    #[test]
    fn a_bare_mention_is_the_same_node() {
        let chart = parse(REAL).unwrap();
        assert_eq!(chart.nodes.len(), 5, "`A` was counted twice");
        assert_eq!(chart.edges[2].to, chart.edges[1].to);
    }

    #[test]
    fn an_unlabelled_edge_is_fine() {
        let chart = parse("flowchart TD\nA[One] --> B[Two]").unwrap();
        assert_eq!(chart.edges[0].label, "");
    }

    #[test]
    fn rank_is_one_past_the_deepest_parent() {
        let chart = parse(REAL).unwrap();
        assert_eq!(chart.ranks(), vec![0, 1, 2, 3, 4]);
    }

    /// Longest path, not shortest: with both a direct and an indirect route,
    /// the node sits below the longer one so no edge points sideways.
    #[test]
    fn a_shortcut_does_not_pull_a_node_up() {
        let chart = parse("flowchart TD\nA[a] --> B[b]\nB --> C[c]\nA --> C").unwrap();
        assert_eq!(chart.ranks(), vec![0, 1, 2]);
    }

    #[test]
    fn a_cycle_terminates() {
        let chart = parse("flowchart TD\nA[a] --> B[b]\nB --> A").unwrap();
        assert_eq!(chart.ranks().len(), 2);
    }

    #[test]
    fn anything_else_is_refused_rather_than_guessed_at() {
        assert!(parse("sequenceDiagram\nA->>B: hi").is_none());
        assert!(parse("flowchart TD").is_none());
        assert!(parse("").is_none());
        // A shape jay is told not to emit still parses; the label survives even
        // though the cylinder does not.
        let chart = parse("flowchart TD\nA[(Postgres)] --> B[App]").unwrap();
        assert_eq!(chart.nodes[0].label, "Postgres");
    }
}
