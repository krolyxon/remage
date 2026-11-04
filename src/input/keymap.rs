//! Per-mode key trie: pending sequences, counts and which-key candidates.

use std::collections::BTreeMap;

use super::keys::{Key, KeySeq};
use crate::action::Action;
use crate::state::Mode;

/// Bindings from one config layer, in file order. `None` is `"none"`: remove the binding.
pub type Bindings = Vec<(KeySeq, Option<Vec<Action>>)>;

/// One layer's `[keys.*]` tables.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyTables {
    pub global: Bindings,
    pub image: Bindings,
    pub grid: Bindings,
    pub visual: Bindings,
}

/// A completed binding: the actions to run and the count typed before it.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub actions: Vec<Action>,
    pub count: Option<usize>,
}

impl Run {
    /// The actions with the count applied.
    pub fn counted(self) -> Vec<Action> {
        match self.count {
            Some(n) => self.actions.into_iter().map(|a| a.with_count(n)).collect(),
            None => self.actions,
        }
    }
}

/// A next key that would continue the pending sequence.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub key: Key,
    pub actions: Option<Vec<Action>>,
    /// Longer sequences continue through this key.
    pub prefix: bool,
}

#[derive(Clone, Debug, Default)]
struct Node {
    actions: Option<Vec<Action>>,
    children: BTreeMap<Key, Node>,
}

impl Node {
    fn walk(&self, keys: &[Key]) -> Option<&Node> {
        keys.iter().try_fold(self, |n, k| n.children.get(k))
    }

    fn insert(&mut self, keys: &[Key], actions: Vec<Action>) {
        let node = keys
            .iter()
            .fold(self, |n, k| n.children.entry(*k).or_default());
        node.actions = Some(actions);
    }

    fn collect(&self, prefix: &mut Vec<Key>, out: &mut Vec<(KeySeq, Vec<Action>)>) {
        if let Some(a) = &self.actions {
            out.push((KeySeq(prefix.clone()), a.clone()));
        }
        for (k, child) in &self.children {
            prefix.push(*k);
            child.collect(prefix, out);
            prefix.pop();
        }
    }
}

/// Largest count accepted; more digits are ignored.
const MAX_COUNT: usize = 9_999_999;

#[derive(Clone, Debug, Default)]
pub struct Keymap {
    /// Image, grid, visual.
    tries: [Node; 3],
    pending: Vec<Key>,
    count: Option<usize>,
}

fn idx(mode: Mode) -> usize {
    match mode {
        Mode::Image => 0,
        Mode::Grid => 1,
        Mode::Visual => 2,
    }
}

impl Keymap {
    /// Build from config layers, lowest priority first. Within each mode the levels stack
    /// global < mode (< visual for visual mode, which sits on grid); later layers win inside a
    /// level, and `"none"` removes whatever a lower level bound.
    pub fn new(layers: &[&KeyTables]) -> Self {
        type Level = fn(&KeyTables) -> &Bindings;
        let global: Level = |t| &t.global;
        let image: Level = |t| &t.image;
        let grid: Level = |t| &t.grid;
        let visual: Level = |t| &t.visual;
        let chains: [&[Level]; 3] = [&[global, image], &[global, grid], &[global, grid, visual]];
        let mut km = Keymap::default();
        for (trie, chain) in km.tries.iter_mut().zip(chains) {
            let mut flat: BTreeMap<&KeySeq, &Vec<Action>> = BTreeMap::new();
            for level in chain {
                let mut merged: BTreeMap<&KeySeq, Option<&Vec<Action>>> = BTreeMap::new();
                for layer in layers {
                    for (k, v) in level(layer) {
                        merged.insert(k, v.as_ref());
                    }
                }
                for (k, v) in merged {
                    match v {
                        Some(a) => flat.insert(k, a),
                        None => flat.remove(k),
                    };
                }
            }
            for (k, a) in flat {
                trie.insert(&k.0, a.clone());
            }
        }
        km
    }

    fn root(&self, mode: Mode) -> &Node {
        &self.tries[idx(mode)]
    }

    /// Feed one key press. Returns the bindings it completed (usually none or one; an
    /// ambiguous prefix followed by a non-continuing key yields the prefix's binding first).
    pub fn feed(&mut self, mode: Mode, key: Key) -> Vec<Run> {
        if self.pending.is_empty()
            && let Some(d) = key.digit()
            && (d != 0 || self.count.is_some())
        {
            let c = self.count.unwrap_or(0).saturating_mul(10) + d as usize;
            self.count = Some(c.min(MAX_COUNT));
            return Vec::new();
        }
        self.pending.push(key);
        match self.root(mode).walk(&self.pending) {
            Some(node) if node.children.is_empty() => {
                let actions = node.actions.clone().unwrap_or_default();
                self.pending.clear();
                vec![Run {
                    actions,
                    count: self.count.take(),
                }]
            }
            Some(_) => Vec::new(),
            None => {
                self.pending.pop();
                let prefix = self
                    .root(mode)
                    .walk(&self.pending)
                    .and_then(|n| n.actions.clone())
                    .filter(|_| !self.pending.is_empty());
                self.pending.clear();
                match prefix {
                    // `g` is bound and so is `gg`: `gx` runs `g`, then `x` on its own.
                    Some(actions) => {
                        let mut out = vec![Run {
                            actions,
                            count: self.count.take(),
                        }];
                        out.extend(self.feed(mode, key));
                        out
                    }
                    None => {
                        self.count = None;
                        Vec::new()
                    }
                }
            }
        }
    }

    /// The pending sequence is itself bound and also a prefix of longer ones.
    pub fn is_ambiguous(&self, mode: Mode) -> bool {
        !self.pending.is_empty()
            && self
                .root(mode)
                .walk(&self.pending)
                .is_some_and(|n| n.actions.is_some() && !n.children.is_empty())
    }

    /// The user stopped typing: an ambiguous sequence runs its own binding.
    pub fn timeout(&mut self, mode: Mode) -> Vec<Run> {
        if !self.is_ambiguous(mode) {
            return Vec::new();
        }
        let actions = self
            .root(mode)
            .walk(&self.pending)
            .and_then(|n| n.actions.clone())
            .unwrap_or_default();
        self.pending.clear();
        vec![Run {
            actions,
            count: self.count.take(),
        }]
    }

    /// A key sequence (not a count) is waiting for more keys.
    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The keys of the pending sequence.
    pub fn pending_keys(&self) -> &[Key] {
        &self.pending
    }

    pub fn cancel(&mut self) {
        self.pending.clear();
        self.count = None;
    }

    /// The count and keys typed so far, for the status bar.
    pub fn pending_display(&self) -> String {
        let mut s = self.count.map(|c| c.to_string()).unwrap_or_default();
        if !self.pending.is_empty() {
            s.push_str(&KeySeq(self.pending.clone()).to_string());
        }
        s
    }

    /// The binding of a complete sequence (mouse buttons and the wheel bypass the trie).
    pub fn lookup(&self, mode: Mode, keys: &[Key]) -> Option<&[Action]> {
        self.root(mode).walk(keys)?.actions.as_deref()
    }

    /// The keys that continue the pending sequence, for which-key.
    pub fn candidates(&self, mode: Mode) -> Vec<Candidate> {
        let Some(node) = self.root(mode).walk(&self.pending) else {
            return Vec::new();
        };
        node.children
            .iter()
            .map(|(k, n)| Candidate {
                key: *k,
                actions: n.actions.clone(),
                prefix: !n.children.is_empty(),
            })
            .collect()
    }

    /// Every binding in a mode, in key order.
    pub fn bindings(&self, mode: Mode) -> Vec<(KeySeq, Vec<Action>)> {
        let mut out = Vec::new();
        self.root(mode).collect(&mut Vec::new(), &mut out);
        out
    }

    /// The sequences bound to exactly `action` in a mode, shortest first.
    pub fn keys_for(&self, mode: Mode, action: &Action) -> Vec<KeySeq> {
        let mut keys: Vec<KeySeq> = self
            .bindings(mode)
            .into_iter()
            .filter(|(_, a)| a.len() == 1 && a[0] == *action)
            .map(|(k, _)| k)
            .collect();
        keys.sort_by_key(|k| k.0.len());
        keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::Zoom;

    fn bind(pairs: &[(&str, &str)]) -> Bindings {
        pairs
            .iter()
            .map(|(k, a)| {
                let actions = (*a != "none").then(|| vec![a.parse().unwrap()]);
                (k.parse().unwrap(), actions)
            })
            .collect()
    }

    fn keys(s: &str) -> Vec<Key> {
        s.parse::<KeySeq>().unwrap().0
    }

    /// Feed a whole string and collect the counted actions.
    fn type_(km: &mut Keymap, mode: Mode, s: &str) -> Vec<Action> {
        keys(s)
            .into_iter()
            .flat_map(|k| km.feed(mode, k))
            .flat_map(Run::counted)
            .collect()
    }

    fn defaults() -> KeyTables {
        KeyTables {
            global: bind(&[
                ("n", "next"),
                ("gg", "first"),
                ("G", "last"),
                ("q", "quit"),
                ("0", "first"),
                ("<Space>w", "run wallpaper"),
                ("<Space>c", "run copy"),
            ]),
            image: bind(&[("+", "zoom in"), ("h", "pan left")]),
            grid: bind(&[("h", "prev"), ("v", "visual")]),
            visual: bind(&[("m", "mark-all")]),
        }
    }

    #[test]
    fn sequences_and_counts() {
        let t = defaults();
        let mut km = Keymap::new(&[&t]);
        let m = Mode::Image;
        assert_eq!(type_(&mut km, m, "n"), [Action::Next(1)]);
        assert_eq!(type_(&mut km, m, "g"), []);
        assert_eq!(km.pending_display(), "g");
        assert_eq!(type_(&mut km, m, "g"), [Action::First]);
        assert_eq!(type_(&mut km, m, "12G"), [Action::Goto(12)]);
        assert_eq!(type_(&mut km, m, "5gg"), [Action::Goto(5)]);
        assert_eq!(type_(&mut km, m, "3n"), [Action::Next(3)]);
        assert_eq!(type_(&mut km, m, "3+"), [Action::Zoom(Zoom::In(3))]);
        // `0` is a key unless a count is pending.
        assert_eq!(type_(&mut km, m, "0"), [Action::First]);
        assert_eq!(type_(&mut km, m, "10n"), [Action::Next(10)]);
        assert_eq!(km.pending_display(), "");
    }

    #[test]
    fn broken_sequences_are_dropped() {
        let t = defaults();
        let mut km = Keymap::new(&[&t]);
        let m = Mode::Image;
        assert_eq!(type_(&mut km, m, "5gx"), []);
        assert!(!km.is_pending());
        assert_eq!(km.pending_display(), "");
        assert_eq!(type_(&mut km, m, "<Space>"), []);
        assert_eq!(type_(&mut km, m, "<Esc>"), []);
        assert_eq!(type_(&mut km, m, "n"), [Action::Next(1)]);
        assert_eq!(type_(&mut km, m, "Z"), []);
    }

    #[test]
    fn modes_override_global() {
        let t = defaults();
        let mut km = Keymap::new(&[&t]);
        assert_eq!(
            type_(&mut km, Mode::Image, "h"),
            [Action::Pan(crate::state::view::Dir::Left)]
        );
        assert_eq!(type_(&mut km, Mode::Grid, "h"), [Action::Prev(1)]);
        // Visual sits on grid.
        assert_eq!(type_(&mut km, Mode::Visual, "h"), [Action::Prev(1)]);
        assert_eq!(type_(&mut km, Mode::Visual, "m"), [Action::MarkAll]);
        assert_eq!(type_(&mut km, Mode::Grid, "m"), []);
    }

    #[test]
    fn later_layers_win_and_none_removes() {
        let base = defaults();
        let user = KeyTables {
            global: bind(&[("n", "prev"), ("q", "none")]),
            image: bind(&[("+", "none"), ("G", "none")]),
            ..Default::default()
        };
        let mut km = Keymap::new(&[&base, &user]);
        assert_eq!(type_(&mut km, Mode::Image, "n"), [Action::Prev(1)]);
        assert_eq!(type_(&mut km, Mode::Image, "q"), []);
        assert_eq!(type_(&mut km, Mode::Image, "+"), []);
        // A mode's "none" masks the global binding in that mode only.
        assert_eq!(type_(&mut km, Mode::Image, "G"), []);
        assert_eq!(type_(&mut km, Mode::Grid, "G"), [Action::Last]);
    }

    #[test]
    fn ambiguous_prefix_waits_then_resolves() {
        let t = KeyTables {
            global: bind(&[("g", "first"), ("gg", "last"), ("n", "next")]),
            ..Default::default()
        };
        let mut km = Keymap::new(&[&t]);
        let m = Mode::Image;
        assert_eq!(type_(&mut km, m, "g"), []);
        assert!(km.is_ambiguous(m));
        assert_eq!(type_(&mut km, m, "g"), [Action::Last]);
        // A non-continuing key runs the prefix, then itself.
        assert_eq!(type_(&mut km, m, "gn"), [Action::First, Action::Next(1)]);
        type_(&mut km, m, "4g");
        let fired: Vec<Action> = km.timeout(m).into_iter().flat_map(Run::counted).collect();
        assert_eq!(fired, [Action::Goto(4)]);
        assert!(!km.is_pending());
    }

    #[test]
    fn which_key_candidates() {
        let t = defaults();
        let mut km = Keymap::new(&[&t]);
        type_(&mut km, Mode::Image, "<Space>");
        let c = km.candidates(Mode::Image);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].key, Key::plain('c'));
        assert_eq!(c[0].actions, Some(vec![Action::Run("copy".into())]));
        assert!(!c[0].prefix);
        km.cancel();
        let root = km.candidates(Mode::Image);
        assert!(root.iter().any(|c| c.key == Key::plain('g') && c.prefix));
    }

    #[test]
    fn lookup_and_reverse_lookup() {
        let t = defaults();
        let km = Keymap::new(&[&t]);
        assert_eq!(
            km.lookup(Mode::Image, &keys("n")),
            Some(&[Action::Next(1)][..])
        );
        assert_eq!(km.lookup(Mode::Image, &keys("g")), None);
        assert_eq!(
            km.keys_for(Mode::Image, &Action::First),
            vec!["0".parse().unwrap(), "gg".parse().unwrap()]
        );
        assert!(
            km.bindings(Mode::Grid)
                .iter()
                .any(|(k, _)| k.to_string() == "v")
        );
    }
}
