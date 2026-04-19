//! Graph analysis: cycle detection, unreachability, dominator computation,
//! and parallel context-conflict detection.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::schema::FunctionDef;

use super::Validator;
use super::helpers::block_writes;
use super::report::Location;

// ---- Graph helper functions -----------------------------------------------

/// Compute, for each block, the transitive closure of `depends_on`.
pub(crate) fn transitive_depends_on(func: &FunctionDef) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in func.blocks.keys() {
        let mut acc: BTreeSet<String> = BTreeSet::new();
        let mut stack: Vec<&str> = func
            .blocks
            .get(name)
            .unwrap()
            .depends_on()
            .iter()
            .map(String::as_str)
            .collect();
        while let Some(n) = stack.pop() {
            if acc.insert(n.to_string())
                && let Some(b) = func.blocks.get(n)
            {
                for d in b.depends_on() {
                    stack.push(d.as_str());
                }
            }
        }
        out.insert(name.clone(), acc);
    }
    out
}

/// Compute, per block, the set of blocks forward-reachable via any chain of
/// `transitions[].goto` edges (excluding the block itself).
pub(crate) fn transitive_ctrl_reach(func: &FunctionDef) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in func.blocks.keys() {
        let mut acc: BTreeSet<String> = BTreeSet::new();
        let mut queue: VecDeque<&str> = VecDeque::new();
        if let Some(b) = func.blocks.get(name) {
            for t in b.transitions() {
                queue.push_back(t.goto.as_str());
            }
        }
        while let Some(n) = queue.pop_front() {
            if acc.insert(n.to_string())
                && let Some(b) = func.blocks.get(n)
            {
                for t in b.transitions() {
                    if !acc.contains(t.goto.as_str()) {
                        queue.push_back(t.goto.as_str());
                    }
                }
            }
        }
        out.insert(name.clone(), acc);
    }
    out
}

// ---- Validator methods ----------------------------------------------------

impl Validator<'_> {
    pub(crate) fn detect_dataflow_cycles(&mut self, func: &FunctionDef, floc: &Location) {
        // 0=white, 1=gray, 2=black
        let mut color: BTreeMap<&str, u8> = func.blocks.keys().map(|k| (k.as_str(), 0u8)).collect();
        for start in func.blocks.keys() {
            if color[start.as_str()] != 0 {
                continue;
            }
            let mut stack: Vec<(&str, usize)> = vec![(start.as_str(), 0)];
            color.insert(start.as_str(), 1);
            while let Some(&(node, idx)) = stack.last() {
                let deps = func.blocks.get(node).unwrap().depends_on();
                if idx < deps.len() {
                    let next = deps[idx].as_str();
                    let last_idx = stack.len() - 1;
                    stack[last_idx].1 += 1;
                    match color.get(next).copied() {
                        Some(0) => {
                            color.insert(next, 1);
                            stack.push((next, 0));
                        }
                        Some(1) => {
                            // `next` is a prerequisite of `node` (data flows next → node).
                            self.err(
                                floc.clone().with_block(node).with_field("depends_on"),
                                format!(
                                    "dataflow cycle: `{node}` depends on `{next}`, closing a cycle in `depends_on`"
                                ),
                            );
                        }
                        _ => {}
                    }
                } else {
                    color.insert(node, 2);
                    stack.pop();
                }
            }
        }
    }

    pub(crate) fn detect_unreachable_blocks(&mut self, func: &FunctionDef, floc: &Location) {
        let mut inbound: BTreeMap<&str, usize> =
            func.blocks.keys().map(|k| (k.as_str(), 0usize)).collect();
        let mut rev_deps: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (name, block) in &func.blocks {
            for t in block.transitions() {
                if let Some(c) = inbound.get_mut(t.goto.as_str()) {
                    *c += 1;
                }
            }
            for d in block.depends_on() {
                if func.blocks.contains_key(d) {
                    if let Some(c) = inbound.get_mut(name.as_str()) {
                        *c += 1;
                    }
                    if let Some((k, _)) = func.blocks.get_key_value(d) {
                        rev_deps.entry(k.as_str()).or_default().push(name.as_str());
                    }
                }
            }
        }
        let mut reachable: BTreeSet<&str> = BTreeSet::new();
        let entries: Vec<&str> = inbound
            .iter()
            .filter(|(_, c)| **c == 0)
            .map(|(k, _)| *k)
            .collect();
        let mut queue: VecDeque<&str> = entries.iter().copied().collect();
        for e in &entries {
            reachable.insert(e);
        }
        while let Some(node) = queue.pop_front() {
            let block = func.blocks.get(node).unwrap();
            for t in block.transitions() {
                if reachable.insert(t.goto.as_str()) {
                    if let Some((k, _)) = func.blocks.get_key_value(&t.goto) {
                        queue.push_back(k.as_str());
                    }
                }
            }
            if let Some(succs) = rev_deps.get(node) {
                for s in succs {
                    if reachable.insert(*s) {
                        queue.push_back(*s);
                    }
                }
            }
        }
        for name in func.blocks.keys() {
            if !reachable.contains(name.as_str()) {
                self.warn(
                    floc.clone().with_block(name),
                    "block is unreachable from any entry point",
                );
            }
        }
    }

    pub(crate) fn detect_parallel_context_conflicts(
        &mut self,
        func: &FunctionDef,
        floc: &Location,
    ) {
        let names: Vec<&str> = func.blocks.keys().map(String::as_str).collect();
        let closure = transitive_depends_on(func);
        let ctrl_reach = transitive_ctrl_reach(func);
        for i in 0..names.len() {
            for j in (i + 1)..names.len() {
                let a = names[i];
                let b = names[j];
                let a_dep_b = closure.get(a).is_some_and(|s| s.contains(b));
                let b_dep_a = closure.get(b).is_some_and(|s| s.contains(a));
                if a_dep_b || b_dep_a {
                    continue;
                }
                let a_ctrl_b = ctrl_reach.get(a).is_some_and(|s| s.contains(b));
                let b_ctrl_a = ctrl_reach.get(b).is_some_and(|s| s.contains(a));
                if a_ctrl_b || b_ctrl_a {
                    continue;
                }
                let (sa_ctx, sa_wf) = block_writes(func.blocks.get(a).unwrap());
                let (sb_ctx, sb_wf) = block_writes(func.blocks.get(b).unwrap());
                let ctx_overlap: BTreeSet<&str> = sa_ctx.intersection(&sb_ctx).copied().collect();
                let wf_overlap: BTreeSet<&str> = sa_wf.intersection(&sb_wf).copied().collect();
                for k in ctx_overlap {
                    self.warn(
                        floc.clone(),
                        format!(
                            "blocks `{a}` and `{b}` may run in parallel and both write `set_context.{k}`"
                        ),
                    );
                }
                for k in wf_overlap {
                    self.warn(
                        floc.clone(),
                        format!(
                            "blocks `{a}` and `{b}` may run in parallel and both write `set_workflow.{k}`"
                        ),
                    );
                }
            }
        }
    }
}

/// Compute the dominator set for each block in a function's control-flow
/// graph (transitions only) using a caller-supplied explicit entry block.
///
/// Unlike [`compute_dominators`] (which infers entry from zero in-degree),
/// this variant accepts the entry block name directly. This is necessary for
/// the imperative runner, where the entry block is already known via
/// find_entry_block and may have non-zero in-degree due to backward edges
/// (loops that jump back to the head block).
///
/// Returns `dom[n]` = the set of all blocks that dominate `n` (including `n`
/// itself). Iterative fixed-point, Kildall-style.
pub(crate) fn compute_dominators_with_entry(
    func: &FunctionDef,
    entry: &str,
) -> BTreeMap<String, BTreeSet<String>> {
    let names: Vec<String> = func.blocks.keys().cloned().collect();
    let all: BTreeSet<String> = names.iter().cloned().collect();

    // Predecessors via control edges only.
    let mut preds: BTreeMap<String, BTreeSet<String>> =
        names.iter().map(|n| (n.clone(), BTreeSet::new())).collect();
    for (src, b) in &func.blocks {
        for t in b.transitions() {
            if let Some(e) = preds.get_mut(&t.goto) {
                e.insert(src.clone());
            }
        }
    }

    // Entry block is dominated only by itself; every other block starts
    // with dom = all blocks.
    let mut dom: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for n in &names {
        if n == entry {
            let mut s = BTreeSet::new();
            s.insert(n.clone());
            dom.insert(n.clone(), s);
        } else {
            dom.insert(n.clone(), all.clone());
        }
    }

    // Iterative fixed-point: dom[n] = {n} ∪ ⋂{dom[p] | p ∈ preds[n]}.
    let mut changed = true;
    while changed {
        changed = false;
        for n in &names {
            if n == entry {
                continue; // entry's dom set is pinned
            }
            let p = &preds[n];
            let mut new_set: Option<BTreeSet<String>> = None;
            for pn in p {
                let d = &dom[pn];
                new_set = Some(match new_set {
                    None => d.clone(),
                    Some(acc) => acc.intersection(d).cloned().collect(),
                });
            }
            let mut new_set = new_set.unwrap_or_default();
            new_set.insert(n.clone());
            if new_set != dom[n] {
                dom.insert(n.clone(), new_set);
                changed = true;
            }
        }
    }

    dom
}

/// Compute the dominator set for each block in a function's control-flow
/// graph (transitions only). Inbound-zero blocks are treated as entry points.
///
/// Uses an iterative fixed-point dominator set computation (Kildall-style):
/// each non-entry block's dominator set is initialised to all blocks, then
/// iteratively refined to `{self} ∪ ⋂{dom(p) | p ∈ preds}` until no set
/// changes. A separate BFS post-processing step resets nodes that are
/// unreachable from any entry to be dominated only by themselves.
pub(crate) fn compute_dominators(func: &FunctionDef) -> BTreeMap<String, BTreeSet<String>> {
    let names: Vec<String> = func.blocks.keys().cloned().collect();
    let all: BTreeSet<String> = names.iter().cloned().collect();

    // Predecessors via control edges only.
    let mut preds: BTreeMap<String, BTreeSet<String>> =
        names.iter().map(|n| (n.clone(), BTreeSet::new())).collect();
    for (src, b) in &func.blocks {
        for t in b.transitions() {
            if let Some(e) = preds.get_mut(&t.goto) {
                e.insert(src.clone());
            }
        }
    }
    // Entry = blocks with no inbound control edges.
    let entries: BTreeSet<String> = preds
        .iter()
        .filter(|(_, p)| p.is_empty())
        .map(|(k, _)| k.clone())
        .collect();

    let mut dom: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for n in &names {
        if entries.contains(n) {
            let mut s = BTreeSet::new();
            s.insert(n.clone());
            dom.insert(n.clone(), s);
        } else {
            dom.insert(n.clone(), all.clone());
        }
    }

    let mut changed = true;
    while changed {
        changed = false;
        for n in &names {
            if entries.contains(n) {
                continue;
            }
            let p = &preds[n];
            let mut new_set: Option<BTreeSet<String>> = None;
            for pn in p {
                let d = &dom[pn];
                new_set = Some(match new_set {
                    None => d.clone(),
                    Some(acc) => acc.intersection(d).cloned().collect(),
                });
            }
            let mut new_set = new_set.unwrap_or_default();
            new_set.insert(n.clone());
            if new_set != dom[n] {
                dom.insert(n.clone(), new_set);
                changed = true;
            }
        }
    }

    // Post-process: nodes unreachable from any entry should only be dominated
    // by themselves.
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = entries.iter().cloned().collect();
    while let Some(n) = queue.pop_front() {
        if !visited.insert(n.clone()) {
            continue;
        }
        if let Some(b) = func.blocks.get(&n) {
            for t in b.transitions() {
                if !visited.contains(t.goto.as_str()) {
                    queue.push_back(t.goto.clone());
                }
            }
        }
    }
    for n in &names {
        if !visited.contains(n) {
            let mut s = BTreeSet::new();
            s.insert(n.clone());
            dom.insert(n.clone(), s);
        }
    }
    dom
}
