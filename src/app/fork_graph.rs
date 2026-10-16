use super::*;
use std::collections::{HashMap, HashSet};

const NODE_SIZE: egui::Vec2 = egui::vec2(170.0, 28.0);
const COLUMN: f32 = 220.0;
const ROW: f32 = 40.0;
const MARGIN: f32 = 12.0;

pub(super) struct ForkGraph<'a> {
    pub(super) sessions: &'a [TermTab],
    pub(super) forks: &'a [Fork],
    pub(super) names: &'a [String],
    pub(super) shown: usize,
    pub(super) remote: &'a str,
}

pub(super) enum GraphAction {
    Show(usize),
    Fork(usize),
    Reopen(String),
}

struct Node {
    id: String,
    tab: Option<usize>,
    depth: usize,
    row: f32,
}

impl ForkGraph<'_> {
    fn conversation_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        let tabs = self.sessions.iter().filter(|t| t.kind == TermKind::Claude).map(|t| t.id.clone());
        let edges = self.forks.iter().flat_map(|f| [f.parent.clone(), f.child.clone()]);
        for id in tabs.chain(edges) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }

    fn tab_of(&self, id: &str) -> Option<usize> {
        self.sessions.iter().position(|t| t.kind == TermKind::Claude && t.id == id)
    }

    fn label(&self, node: &Node) -> String {
        match node.tab {
            Some(i) => self.names[i].clone(),
            None => format!("closed · {}", &node.id[..8]),
        }
    }

    fn layout(&self) -> Vec<Node> {
        let ids = self.conversation_ids();
        let known: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
        for f in self.forks {
            children.entry(f.parent.as_str()).or_default().push(f.child.as_str());
        }
        let roots = ids.iter().filter(|id| Fork::parent_of(self.forks, id).is_none_or(|p| !known.contains(p)));
        let mut nodes = Vec::new();
        let mut placed = HashSet::new();
        let mut next_row = 0.0;
        for root in roots {
            self.place(root, 0, &children, &mut placed, &mut next_row, &mut nodes);
        }
        nodes
    }

    fn place<'a>(&self, id: &'a str, depth: usize, children: &HashMap<&str, Vec<&'a str>>, placed: &mut HashSet<&'a str>, next_row: &mut f32, nodes: &mut Vec<Node>) -> f32 {
        if !placed.insert(id) {
            return *next_row;
        }
        let at = nodes.len();
        nodes.push(Node { id: id.to_string(), tab: self.tab_of(id), depth, row: 0.0 });
        let rows: Vec<f32> = children.get(id).into_iter().flatten().map(|child| self.place(child, depth + 1, children, placed, next_row, nodes)).collect();
        let row = if rows.is_empty() {
            *next_row += 1.0;
            *next_row - 1.0
        } else {
            (rows[0] + rows[rows.len() - 1]) / 2.0
        };
        nodes[at].row = row;
        row
    }

    fn dot(&self, nodes: &[Node]) -> String {
        let mut dot = String::from("digraph forks {\n  rankdir=LR;\n  node [shape=box, style=rounded, fontname=\"monospace\"];\n");
        for node in nodes {
            let label = self.label(node).replace('"', "\\\"");
            let style = if node.tab.is_some() { "" } else { ", color=gray, fontcolor=gray" };
            dot += &format!("  \"{}\" [label=\"{label}\\n{}\"{style}];\n", node.id, &node.id[..8]);
        }
        for f in self.forks {
            dot += &format!("  \"{}\" -> \"{}\";\n", f.parent, f.child);
        }
        dot + "}\n"
    }
}

fn open_as_svg(dot: &str) -> Result<(), String> {
    let dir = crate::engine::app_dir().join("fork-graph");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let (source, svg) = (dir.join("forks.dot"), dir.join("forks.svg"));
    std::fs::write(&source, dot).map_err(|e| e.to_string())?;
    let rendered = std::process::Command::new("dot").arg("-Tsvg").arg("-o").arg(&svg).arg(&source).status().map_err(|e| format!("cannot run dot (graphviz): {e}"))?;
    if !rendered.success() {
        return Err(format!("dot failed ({rendered})"));
    }
    std::process::Command::new("xdg-open").arg(&svg).spawn().map(|_| ()).map_err(|e| format!("cannot open {}: {e}", svg.display()))
}

fn node_rect(origin: egui::Pos2, node: &Node) -> egui::Rect {
    egui::Rect::from_min_size(origin + egui::vec2(MARGIN + node.depth as f32 * COLUMN, MARGIN + node.row * ROW), NODE_SIZE)
}

fn edge(painter: &egui::Painter, from: egui::Rect, to: egui::Rect, color: Color32) {
    let (start, end) = (from.right_center(), to.left_center());
    let bend = (end.x - start.x) / 2.0;
    let points = [start, start + egui::vec2(bend, 0.0), end - egui::vec2(bend, 0.0), end];
    painter.add(egui::epaint::CubicBezierShape::from_points_stroke(points, false, Color32::TRANSPARENT, egui::Stroke::new(1.5, color)));
}

fn graph_ui(ui: &mut egui::Ui, graph: &ForkGraph, nodes: &[Node]) -> Option<GraphAction> {
    let columns = nodes.iter().map(|n| n.depth + 1).max().unwrap_or(1) as f32;
    let rows = nodes.iter().map(|n| n.row + 1.0).fold(1.0, f32::max);
    let size = egui::vec2(2.0 * MARGIN + (columns - 1.0) * COLUMN + NODE_SIZE.x, 2.0 * MARGIN + (rows - 1.0) * ROW + NODE_SIZE.y);
    let (area, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(area);
    let rect_of: HashMap<&str, egui::Rect> = nodes.iter().map(|n| (n.id.as_str(), node_rect(area.min, n))).collect();
    let line = ui.visuals().weak_text_color();
    for f in graph.forks {
        if let (Some(&from), Some(&to)) = (rect_of.get(f.parent.as_str()), rect_of.get(f.child.as_str())) {
            edge(&painter, from, to, line);
        }
    }
    let mut action = None;
    for node in nodes {
        let rect = rect_of[node.id.as_str()];
        let response = ui.interact(rect, egui::Id::new(("fork-node", &node.id)), egui::Sense::click());
        let shown = node.tab == Some(graph.shown);
        let open = node.tab.is_some();
        let fill = if shown { CLAUDE.gamma_multiply(0.35) } else if response.hovered() { ui.visuals().widgets.hovered.bg_fill } else { ui.visuals().extreme_bg_color };
        let stroke = egui::Stroke::new(if shown { 2.0 } else { 1.0 }, if open { CLAUDE } else { line });
        painter.rect(rect, 6.0, fill, stroke, egui::StrokeKind::Inside);
        let text = if open { ui.visuals().text_color() } else { line };
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, graph.label(node), egui::FontId::proportional(13.0), text);
        let hover = match node.tab {
            Some(_) => format!("conversation {}\nclick: show its tab · right-click: fork", node.id),
            None => format!("conversation {} (no tab)\nclick: open it again in a tab ({} --resume)", node.id, "claude"),
        };
        let response = response.on_hover_text(hover);
        if response.clicked() {
            action = Some(match node.tab {
                Some(i) => GraphAction::Show(i),
                None => GraphAction::Reopen(node.id.clone()),
            });
        }
        response.context_menu(|ui| {
            if let Some(i) = node.tab
                && ui.button("⑂ Fork").clicked()
            {
                action = Some(GraphAction::Fork(i));
            }
            if ui.button("📋 Copy the conversation ID").clicked() {
                ui.ctx().copy_text(node.id.clone());
            }
        });
    }
    action
}

pub(super) fn fork_graph_window(ctx: &egui::Context, open: &mut bool, graph: &ForkGraph) -> Option<GraphAction> {
    let nodes = graph.layout();
    let error_id = egui::Id::new(("fork-graph-error", graph.remote));
    let mut action = None;
    egui::Window::new("🌳 Claude forks").id(egui::Id::new(("fork-graph", graph.remote))).open(open).default_size([640.0, 360.0]).resizable(true).show(ctx, |ui| {
        ui.horizontal(|ui| {
            if ui.button("📋 Copy as dot").on_hover_text("The graph in Graphviz's dot language").clicked() {
                ui.ctx().copy_text(graph.dot(&nodes));
            }
            if ui.button("🖼 Open as SVG").on_hover_text("Rendered by Graphviz (dot -Tsvg), opened with xdg-open").clicked() {
                let result = open_as_svg(&graph.dot(&nodes)).err();
                ui.ctx().data_mut(|d| d.insert_temp(error_id, result));
            }
            ui.weak("a fork starts with the whole history of its parent");
        });
        if let Some(Some(error)) = ui.ctx().data(|d| d.get_temp::<Option<String>>(error_id)) {
            ui.colored_label(Color32::LIGHT_RED, error);
        }
        ui.separator();
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            action = graph_ui(ui, graph, &nodes);
        });
    });
    action
}
