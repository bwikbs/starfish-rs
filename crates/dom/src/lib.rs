//! starfish-dom — arena-based DOM node tree.
//!
//! The whole tree is owned by a single [`Document`]; nodes live in a `Vec`
//! indexed by a `u32`-newtype [`NodeId`]. Sibling/parent links are plain
//! integers, so `append_child` is O(1) and traversal needs no borrow dance.

/// Index into `Document::nodes`. 4 bytes, `Copy`. Only valid for the
/// `Document` that produced it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(u32);

#[derive(Debug)]
pub struct Node {
    pub kind: NodeKind,
    parent: Option<NodeId>,
    first_child: Option<NodeId>,
    last_child: Option<NodeId>,
    prev_sibling: Option<NodeId>,
    next_sibling: Option<NodeId>,
}

#[derive(Debug)]
pub enum NodeKind {
    Document,
    Doctype(Doctype),
    Element(Element),
    Text(String),
    Comment(String),
}

#[derive(Debug)]
pub struct Doctype {
    pub name: String,
}

#[derive(Debug)]
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

    /// Append `child` as the last child of `parent`. `child` must be detached.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        debug_assert!(self.node(child).parent.is_none(), "child must be detached");
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
}
