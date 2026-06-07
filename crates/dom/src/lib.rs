//! starfish-dom — arena-based DOM node tree.
//!
//! The whole tree is owned by a single [`Document`]; nodes live in a `Vec`
//! indexed by a `u32`-newtype [`NodeId`]. Sibling/parent links are plain
//! integers, so `append_child` is O(1) and traversal needs no borrow dance.

/// Index into `Document::nodes`. 4 bytes, `Copy`. Only valid for the
/// `Document` that produced it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(u32);

impl NodeId {
    /// The raw arena index. Stable for the life of the `Document` (ids are
    /// never invalidated; the arena only grows). Useful as a plain-integer key
    /// for host-side caches that can't carry a `NodeId`.
    pub fn index(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone)]
pub struct Node {
    pub kind: NodeKind,
    parent: Option<NodeId>,
    first_child: Option<NodeId>,
    last_child: Option<NodeId>,
    prev_sibling: Option<NodeId>,
    next_sibling: Option<NodeId>,
}

#[derive(Debug, Clone)]
pub enum NodeKind {
    Document,
    Doctype(Doctype),
    Element(Element),
    Text(String),
    Comment(String),
}

#[derive(Debug, Clone)]
pub struct Doctype {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Element {
    /// Lowercased tag name, e.g. `"div"`.
    pub name: String,
    pub attrs: Vec<Attr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr {
    /// Lowercased attribute name.
    pub name: String,
    pub value: String,
}

#[derive(Clone)]
pub struct Document {
    nodes: Vec<Node>,
    root: NodeId,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    /// New document containing only the `Document` root node (always `nodes[0]`).
    pub fn new() -> Document {
        let root = NodeId(0);
        let nodes = vec![Node {
            kind: NodeKind::Document,
            parent: None,
            first_child: None,
            last_child: None,
            prev_sibling: None,
            next_sibling: None,
        }];
        Document { nodes, root }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    fn push(&mut self, kind: NodeKind) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Node {
            kind,
            parent: None,
            first_child: None,
            last_child: None,
            prev_sibling: None,
            next_sibling: None,
        });
        id
    }

    // --- node creation (returns a detached node owned by the arena) ---

    pub fn create_element(&mut self, name: &str) -> NodeId {
        self.push(NodeKind::Element(Element {
            name: name.to_ascii_lowercase(),
            attrs: Vec::new(),
        }))
    }

    pub fn create_text(&mut self, data: &str) -> NodeId {
        self.push(NodeKind::Text(data.to_string()))
    }

    pub fn create_comment(&mut self, data: &str) -> NodeId {
        self.push(NodeKind::Comment(data.to_string()))
    }

    pub fn create_doctype(&mut self, name: &str) -> NodeId {
        self.push(NodeKind::Doctype(Doctype {
            name: name.to_ascii_lowercase(),
        }))
    }

    // --- tree mutation ---

    /// Append `child` as the last child of `parent`. If `child` is already in
    /// the tree it is detached from its old parent first (a move / re-parent).
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        self.detach(child);
        let last = self.node(parent).last_child;
        self.node_mut(child).parent = Some(parent);
        match last {
            Some(prev) => {
                self.node_mut(prev).next_sibling = Some(child);
                self.node_mut(child).prev_sibling = Some(prev);
            }
            None => {
                self.node_mut(parent).first_child = Some(child);
            }
        }
        self.node_mut(parent).last_child = Some(child);
    }

    /// Unlink `id` from its current parent, fixing sibling/parent links. A
    /// no-op if `id` is already detached (has no parent). Keeps every
    /// parent/first_child/last_child/prev/next link mutually consistent.
    pub fn detach(&mut self, id: NodeId) {
        let (parent, prev, next) = {
            let n = self.node(id);
            (n.parent, n.prev_sibling, n.next_sibling)
        };
        let Some(parent) = parent else { return };
        match prev {
            Some(p) => self.node_mut(p).next_sibling = next,
            None => self.node_mut(parent).first_child = next,
        }
        match next {
            Some(nx) => self.node_mut(nx).prev_sibling = prev,
            None => self.node_mut(parent).last_child = prev,
        }
        let n = self.node_mut(id);
        n.parent = None;
        n.prev_sibling = None;
        n.next_sibling = None;
    }

    /// Remove `child` iff it is currently a child of `parent`. `Ok(child)` on
    /// success; `Err(())` if `child`'s parent is not `parent` (the JS layer
    /// maps this to a thrown NotFoundError). The unit error is the deliberate
    /// "not a child" signal — a custom error type is overkill for this binding.
    #[allow(clippy::result_unit_err)]
    pub fn remove_child(&mut self, parent: NodeId, child: NodeId) -> Result<NodeId, ()> {
        if self.node(child).parent != Some(parent) {
            return Err(());
        }
        self.detach(child);
        Ok(child)
    }

    /// Insert `child` immediately before `reference` (a child of `parent`). A
    /// `None` reference appends. `child` is detached from any old parent first.
    /// `Err(())` if `reference` is `Some` but not a child of `parent` (the JS
    /// layer throws on it; the unit error is a deliberate signal).
    #[allow(clippy::result_unit_err)]
    pub fn insert_before(
        &mut self,
        parent: NodeId,
        child: NodeId,
        reference: Option<NodeId>,
    ) -> Result<(), ()> {
        match reference {
            None => {
                self.append_child(parent, child);
                Ok(())
            }
            Some(r) => {
                if self.node(r).parent != Some(parent) {
                    return Err(());
                }
                self.detach(child);
                let prev = self.node(r).prev_sibling;
                {
                    let n = self.node_mut(child);
                    n.parent = Some(parent);
                    n.next_sibling = Some(r);
                    n.prev_sibling = prev;
                }
                self.node_mut(r).prev_sibling = Some(child);
                match prev {
                    Some(p) => self.node_mut(p).next_sibling = Some(child),
                    None => self.node_mut(parent).first_child = Some(child),
                }
                Ok(())
            }
        }
    }

    // --- access ---

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.0 as usize]
    }

    pub fn kind(&self, id: NodeId) -> &NodeKind {
        &self.node(id).kind
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).parent
    }

    pub fn first_child(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).first_child
    }

    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).next_sibling
    }

    pub fn last_child(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).last_child
    }

    pub fn prev_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).prev_sibling
    }

    /// Collect children in order (convenience; allocates).
    pub fn children(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut cur = self.first_child(id);
        while let Some(c) = cur {
            out.push(c);
            cur = self.next_sibling(c);
        }
        out
    }

    // --- element helpers ---

    /// Tag name if `id` is an element, else `None`.
    pub fn tag_name(&self, id: NodeId) -> Option<&str> {
        match self.kind(id) {
            NodeKind::Element(e) => Some(&e.name),
            _ => None,
        }
    }

    /// First attribute value with this (already-lowercased) name.
    pub fn get_attribute(&self, id: NodeId, name: &str) -> Option<&str> {
        match self.kind(id) {
            NodeKind::Element(e) => e
                .attrs
                .iter()
                .find(|a| a.name == name)
                .map(|a| a.value.as_str()),
            _ => None,
        }
    }

    /// Set (or replace the first) attribute `name` (lowercased) on an element.
    /// A no-op if `id` is not an element.
    pub fn set_attribute(&mut self, id: NodeId, name: &str, value: &str) {
        let name = name.to_ascii_lowercase();
        if let NodeKind::Element(e) = &mut self.node_mut(id).kind {
            match e.attrs.iter_mut().find(|a| a.name == name) {
                Some(a) => a.value = value.to_string(),
                None => e.attrs.push(Attr {
                    name,
                    value: value.to_string(),
                }),
            }
        }
    }

    /// Remove every attribute named `name`. Returns whether any existed.
    pub fn remove_attribute(&mut self, id: NodeId, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        if let NodeKind::Element(e) = &mut self.node_mut(id).kind {
            let before = e.attrs.len();
            e.attrs.retain(|a| a.name != name);
            return e.attrs.len() != before;
        }
        false
    }

    // --- E7-M1: element-only sibling / index / structural helpers ---

    /// Nearest previous sibling that is an Element (skips text/comment).
    pub fn prev_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let mut cur = self.prev_sibling(id);
        while let Some(s) = cur {
            if self.tag_name(s).is_some() {
                return Some(s);
            }
            cur = self.prev_sibling(s);
        }
        None
    }

    /// Nearest following sibling that is an Element (skips text/comment).
    pub fn next_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let mut cur = self.next_sibling(id);
        while let Some(s) = cur {
            if self.tag_name(s).is_some() {
                return Some(s);
            }
            cur = self.next_sibling(s);
        }
        None
    }

    /// 1-based index of `id` among its element siblings (text/comment ignored).
    /// Returns 1 for a first/only element child. (Caller guarantees `id` is an
    /// Element.)
    pub fn element_index(&self, id: NodeId) -> u32 {
        let mut n = 1;
        let mut cur = self.prev_element_sibling(id);
        while let Some(s) = cur {
            n += 1;
            cur = self.prev_element_sibling(s);
        }
        n
    }

    /// 1-based index counting from the end (for `:last-child`/symmetry).
    pub fn element_index_from_end(&self, id: NodeId) -> u32 {
        let mut n = 1;
        let mut cur = self.next_element_sibling(id);
        while let Some(s) = cur {
            n += 1;
            cur = self.next_element_sibling(s);
        }
        n
    }

    /// Total number of element children of `id`'s parent (for `:only-child`:
    /// true iff this == 1). 0 if no parent.
    pub fn element_sibling_count(&self, id: NodeId) -> u32 {
        match self.parent(id) {
            Some(p) => self
                .children(p)
                .into_iter()
                .filter(|c| self.tag_name(*c).is_some())
                .count() as u32,
            None => 0,
        }
    }

    /// 1-based index of `id` among its same-tag element siblings (for
    /// `:nth-of-type`). (Caller guarantees `id` is an Element.)
    pub fn element_type_index(&self, id: NodeId) -> u32 {
        let tag = self.tag_name(id);
        let mut n = 1;
        let mut cur = self.prev_element_sibling(id);
        while let Some(s) = cur {
            if self.tag_name(s) == tag {
                n += 1;
            }
            cur = self.prev_element_sibling(s);
        }
        n
    }

    /// `:empty` — `id` has no element children and no Text child containing a
    /// non-whitespace character. Comments are ignored. (Caller guarantees `id`
    /// is an Element.)
    pub fn is_empty_element(&self, id: NodeId) -> bool {
        for c in self.children(id) {
            match self.kind(c) {
                NodeKind::Element(_) => return false,
                NodeKind::Text(t) if t.chars().any(|ch| !ch.is_whitespace()) => return false,
                _ => {}
            }
        }
        true
    }

    /// `:root` — `id` is an Element whose parent is the Document node (i.e. the
    /// document element).
    pub fn is_root_element(&self, id: NodeId) -> bool {
        self.tag_name(id).is_some()
            && matches!(self.parent(id).map(|p| self.kind(p)), Some(NodeKind::Document))
    }

    /// If `parent`'s last child is a `Text` node, push `s` onto it and return
    /// `true`; otherwise return `false` (caller then creates a new Text node).
    /// Text-coalescing helper for the tree builder.
    pub fn append_text(&mut self, parent: NodeId, s: &str) -> bool {
        if let Some(last) = self.node(parent).last_child {
            if let NodeKind::Text(t) = &mut self.node_mut(last).kind {
                t.push_str(s);
                return true;
            }
        }
        false
    }

    /// Serialize the subtree at `id` to an indented S-expr-ish string (for
    /// tests). The whole document is `serialize(self.root())`.
    pub fn serialize(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.serialize_into(id, 0, &mut out);
        out
    }

    fn serialize_into(&self, id: NodeId, depth: usize, out: &mut String) {
        for _ in 0..depth {
            out.push_str("  ");
        }
        match self.kind(id) {
            NodeKind::Document => out.push_str("(document"),
            NodeKind::Doctype(d) => {
                out.push_str("(doctype ");
                out.push_str(&d.name);
            }
            NodeKind::Element(e) => {
                out.push_str("(element ");
                out.push_str(&e.name);
                for a in &e.attrs {
                    out.push(' ');
                    out.push_str(&a.name);
                    out.push_str("=\"");
                    out.push_str(&a.value);
                    out.push('"');
                }
            }
            NodeKind::Text(t) => {
                out.push('"');
                out.push_str(t);
                out.push('"');
                return;
            }
            NodeKind::Comment(c) => {
                out.push_str("(comment \"");
                out.push_str(c);
                out.push_str("\")");
                return;
            }
        }
        let children = self.children(id);
        for c in children {
            out.push('\n');
            self.serialize_into(c, depth + 1, out);
        }
        out.push(')');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_child_links() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = doc.create_element("a");
        let b = doc.create_element("b");
        let c = doc.create_element("c");
        doc.append_child(root, a);
        doc.append_child(a, b);
        doc.append_child(a, c);

        assert_eq!(doc.parent(b), Some(a));
        assert_eq!(doc.parent(c), Some(a));
        assert_eq!(doc.first_child(a), Some(b));
        assert_eq!(doc.next_sibling(b), Some(c));
        assert_eq!(doc.next_sibling(c), None);
        // children() in insertion order
        assert_eq!(doc.children(a), vec![b, c]);
    }

    #[test]
    fn get_attribute_first_wins_and_missing() {
        let mut doc = Document::new();
        let e = doc.create_element("div");
        if let NodeKind::Element(el) = &mut doc.node_mut(e).kind {
            el.attrs.push(Attr {
                name: "class".into(),
                value: "first".into(),
            });
            el.attrs.push(Attr {
                name: "class".into(),
                value: "second".into(),
            });
        }
        assert_eq!(doc.get_attribute(e, "class"), Some("first"));
        assert_eq!(doc.get_attribute(e, "id"), None);
    }

    #[test]
    fn tag_name_none_for_non_element() {
        let mut doc = Document::new();
        let t = doc.create_text("hi");
        assert_eq!(doc.tag_name(t), None);
        let e = doc.create_element("p");
        assert_eq!(doc.tag_name(e), Some("p"));
    }

    #[test]
    fn append_text_coalesces() {
        let mut doc = Document::new();
        let p = doc.create_element("p");

        // first append: no existing text child
        assert!(!doc.append_text(p, "a"));
        let t = doc.create_text("a");
        doc.append_child(p, t);
        // second append: coalesces into the existing text node
        assert!(doc.append_text(p, "b"));
        assert_eq!(doc.children(p).len(), 1);
        match doc.kind(doc.children(p)[0]) {
            NodeKind::Text(s) => assert_eq!(s, "ab"),
            _ => panic!("expected text"),
        }

        // a non-text child intervenes: next text append must not coalesce
        let br = doc.create_element("br");
        doc.append_child(p, br);
        assert!(!doc.append_text(p, "c"));
    }

    #[test]
    fn names_lowercased() {
        let mut doc = Document::new();
        let e = doc.create_element("DIV");
        assert_eq!(doc.tag_name(e), Some("div"));
    }

    #[test]
    fn append_child_reparents() {
        let mut doc = Document::new();
        let a = doc.create_element("a");
        let b = doc.create_element("b");
        let c = doc.create_element("c");
        doc.append_child(a, c);
        doc.append_child(b, c); // move c from a to b
        assert_eq!(doc.children(a), vec![]);
        assert_eq!(doc.children(b), vec![c]);
        assert_eq!(doc.parent(c), Some(b));
    }

    #[test]
    fn detach_middle_keeps_links() {
        let mut doc = Document::new();
        let p = doc.create_element("p");
        let a = doc.create_element("a");
        let b = doc.create_element("b");
        let c = doc.create_element("c");
        doc.append_child(p, a);
        doc.append_child(p, b);
        doc.append_child(p, c);
        doc.detach(b);
        assert_eq!(doc.children(p), vec![a, c]);
        assert_eq!(doc.next_sibling(a), Some(c));
        assert_eq!(doc.prev_sibling(c), Some(a));
        assert_eq!(doc.parent(b), None);
        // detach last
        doc.detach(c);
        assert_eq!(doc.children(p), vec![a]);
        assert_eq!(doc.last_child(p), Some(a));
        assert_eq!(doc.next_sibling(a), None);
    }

    #[test]
    fn remove_child_only_own() {
        let mut doc = Document::new();
        let p = doc.create_element("p");
        let q = doc.create_element("q");
        let a = doc.create_element("a");
        doc.append_child(p, a);
        assert_eq!(doc.remove_child(q, a), Err(()));
        assert_eq!(doc.remove_child(p, a), Ok(a));
        assert_eq!(doc.children(p), vec![]);
        assert_eq!(doc.parent(a), None);
    }

    #[test]
    fn insert_before_orders() {
        let mut doc = Document::new();
        let p = doc.create_element("p");
        let a = doc.create_element("a");
        let b = doc.create_element("b");
        let x = doc.create_element("x");
        doc.append_child(p, a);
        doc.append_child(p, b);
        assert_eq!(doc.insert_before(p, x, Some(b)), Ok(()));
        assert_eq!(doc.children(p), vec![a, x, b]);
        assert_eq!(doc.prev_sibling(x), Some(a));
        assert_eq!(doc.next_sibling(x), Some(b));
        // insert before first child
        let y = doc.create_element("y");
        assert_eq!(doc.insert_before(p, y, Some(a)), Ok(()));
        assert_eq!(doc.children(p), vec![y, a, x, b]);
        assert_eq!(doc.first_child(p), Some(y));
        // None reference appends
        let z = doc.create_element("z");
        assert_eq!(doc.insert_before(p, z, None), Ok(()));
        assert_eq!(doc.children(p), vec![y, a, x, b, z]);
        // bad reference errors
        let lone = doc.create_element("lone");
        let w = doc.create_element("w");
        assert_eq!(doc.insert_before(p, w, Some(lone)), Err(()));
    }

    #[test]
    fn set_remove_attribute() {
        let mut doc = Document::new();
        let e = doc.create_element("div");
        doc.set_attribute(e, "class", "a");
        assert_eq!(doc.get_attribute(e, "class"), Some("a"));
        doc.set_attribute(e, "CLASS", "b"); // case-insensitive, replaces first
        assert_eq!(doc.get_attribute(e, "class"), Some("b"));
        assert!(doc.remove_attribute(e, "class"));
        assert_eq!(doc.get_attribute(e, "class"), None);
        assert!(!doc.remove_attribute(e, "class"));
        // no-op on non-element
        let t = doc.create_text("hi");
        doc.set_attribute(t, "x", "y");
        assert_eq!(doc.get_attribute(t, "x"), None);
    }

    // --- E7-M1: element-only structural helpers ---

    /// Build `<parent>` with the given children: 'e'+tag for an element with
    /// that tag char, 't'+text, 'c'+comment. Returns (doc, parent, element ids
    /// in order added).
    fn build_children(spec: &[(&str, &str)]) -> (Document, NodeId, Vec<NodeId>) {
        let mut doc = Document::new();
        let parent = doc.create_element("ul");
        doc.append_child(doc.root(), parent);
        let mut ids = Vec::new();
        for (kind, data) in spec {
            let id = match *kind {
                "e" => doc.create_element(data),
                "t" => doc.create_text(data),
                "c" => doc.create_comment(data),
                _ => unreachable!(),
            };
            doc.append_child(parent, id);
            ids.push(id);
        }
        (doc, parent, ids)
    }

    #[test]
    fn element_index_skips_non_elements() {
        // text, li, comment, li, text → two element children.
        let (doc, _p, ids) = build_children(&[
            ("t", "  "),
            ("e", "li"),
            ("c", "x"),
            ("e", "li"),
            ("t", "\n"),
        ]);
        let li1 = ids[1];
        let li2 = ids[3];
        assert_eq!(doc.element_index(li1), 1);
        assert_eq!(doc.element_index(li2), 2);
        assert_eq!(doc.element_index_from_end(li1), 2);
        assert_eq!(doc.element_index_from_end(li2), 1);
        assert_eq!(doc.element_sibling_count(li1), 2);
        assert_eq!(doc.prev_element_sibling(li2), Some(li1));
        assert_eq!(doc.next_element_sibling(li1), Some(li2));
        assert_eq!(doc.prev_element_sibling(li1), None);
        assert_eq!(doc.next_element_sibling(li2), None);
    }

    #[test]
    fn element_type_index_vs_element_index() {
        // p, span, p → second p has type-index 2 but element-index 3.
        let (doc, _p, ids) = build_children(&[("e", "p"), ("e", "span"), ("e", "p")]);
        let p2 = ids[2];
        assert_eq!(doc.element_index(p2), 3);
        assert_eq!(doc.element_type_index(p2), 2);
        // first p: both 1.
        assert_eq!(doc.element_type_index(ids[0]), 1);
    }

    #[test]
    fn is_empty_element_cases() {
        // <p></p>
        let mut doc = Document::new();
        let empty = doc.create_element("p");
        assert!(doc.is_empty_element(empty));
        // <p>  </p> whitespace only
        let ws = doc.create_element("p");
        let t = doc.create_text("   \n");
        doc.append_child(ws, t);
        assert!(doc.is_empty_element(ws));
        // <p><!--c--></p>
        let comm = doc.create_element("p");
        let c = doc.create_comment("c");
        doc.append_child(comm, c);
        assert!(doc.is_empty_element(comm));
        // <p>x</p>
        let text = doc.create_element("p");
        let x = doc.create_text("x");
        doc.append_child(text, x);
        assert!(!doc.is_empty_element(text));
        // <p><b></b></p>
        let elem = doc.create_element("p");
        let b = doc.create_element("b");
        doc.append_child(elem, b);
        assert!(!doc.is_empty_element(elem));
    }

    #[test]
    fn is_root_element_check() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        doc.append_child(doc.root(), html);
        let body = doc.create_element("body");
        doc.append_child(html, body);
        assert!(doc.is_root_element(html));
        assert!(!doc.is_root_element(body));
    }

    #[test]
    fn document_clones_preserving_ids() {
        let mut doc = Document::new();
        let p = doc.create_element("p");
        doc.append_child(doc.root(), p);
        let t = doc.create_text("hi");
        doc.append_child(p, t);
        let clone = doc.clone();
        assert_eq!(clone.serialize(clone.root()), doc.serialize(doc.root()));
        // NodeId indices stay valid against the clone.
        assert_eq!(clone.tag_name(p), Some("p"));
    }
}
