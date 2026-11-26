//! Cycle detection and ordering over the dependency edges.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::Edge;

fn adjacency<'a>(nodes: &'a [String], edges: &'a [Edge]) -> BTreeMap<&'a str, Vec<&'a str>> {
    let mut adj: BTreeMap<&str, Vec<&str>> = nodes.iter().map(|n| (n.as_str(), vec![])).collect();
    for e in edges {
        let deps = adj.entry(e.package.as_str()).or_default();
        if !deps.contains(&e.dependency.as_str()) {
            deps.push(e.dependency.as_str());
        }
    }
    for v in adj.values_mut() {
        v.sort();
    }
    adj
}

/// Every distinct cycle found by depth-first search, as `[a, b, a]` (a needs b needs a).
pub(crate) fn find_cycles(nodes: &[String], edges: &[Edge]) -> Vec<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Gray,
        Black,
    }
    fn visit<'a>(
        n: &'a str,
        adj: &BTreeMap<&'a str, Vec<&'a str>>,
        color: &mut BTreeMap<&'a str, Color>,
        stack: &mut Vec<&'a str>,
        out: &mut Vec<Vec<String>>,
        seen: &mut BTreeSet<Vec<String>>,
    ) {
        color.insert(n, Color::Gray);
        stack.push(n);
        for &d in adj.get(n).map(Vec::as_slice).unwrap_or_default() {
            match color.get(d).copied().unwrap_or(Color::White) {
                Color::White => visit(d, adj, color, stack, out, seen),
                Color::Gray => {
                    let pos = stack.iter().position(|s| *s == d).unwrap_or(0);
                    let mut cyc: Vec<String> = stack[pos..].iter().map(|s| s.to_string()).collect();
                    // Canonical rotation so the same cycle is reported once.
                    let min = (0..cyc.len()).min_by_key(|&i| cyc[i].clone()).unwrap_or(0);
                    cyc.rotate_left(min);
                    if seen.insert(cyc.clone()) {
                        cyc.push(cyc[0].clone());
                        out.push(cyc);
                    }
                }
                Color::Black => {}
            }
        }
        stack.pop();
        color.insert(n, Color::Black);
    }

    let adj = adjacency(nodes, edges);
    let mut color: BTreeMap<&str, Color> = BTreeMap::new();
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for n in adj.keys().copied().collect::<Vec<_>>() {
        if color.get(n).copied().unwrap_or(Color::White) == Color::White {
            visit(n, &adj, &mut color, &mut Vec::new(), &mut out, &mut seen);
        }
    }
    out
}

/// Dependencies first; ties alphabetical. Nodes stuck in a cycle come last, alphabetically.
pub(crate) fn topo_order(nodes: &[String], edges: &[Edge]) -> Vec<String> {
    let adj = adjacency(nodes, edges);
    let mut unmet: BTreeMap<&str, usize> = adj.iter().map(|(n, d)| (*n, d.len())).collect();
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (n, deps) in &adj {
        for d in deps {
            dependents.entry(d).or_default().push(n);
        }
    }
    let mut ready: BTreeSet<&str> = unmet
        .iter()
        .filter(|(_, c)| **c == 0)
        .map(|(n, _)| *n)
        .collect();
    let mut out: Vec<String> = Vec::new();
    while let Some(n) = ready.pop_first() {
        out.push(n.to_string());
        for &d in dependents.get(n).map(Vec::as_slice).unwrap_or_default() {
            if let Some(c) = unmet.get_mut(d) {
                *c -= 1;
                if *c == 0 {
                    ready.insert(d);
                }
            }
        }
    }
    for n in adj.keys() {
        if !out.iter().any(|o| o == n) {
            out.push(n.to_string());
        }
    }
    out
}
