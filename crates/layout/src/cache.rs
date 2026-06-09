//! Intrinsic-measurement memoization (E12-M1). During one `layout()` pass,
//! table/grid/flex re-lay-out the SAME cell/item subtree 2–3× to read intrinsic
//! scalars (preferred width, natural height, flex base). Each `measure_*` call
//! runs `layout_block` against a FRESH containing block + EMPTY float context, so
//! the returned scalar depends only on (the frozen styled subtree, the available
//! width). `NodeId` is a stable proxy for that frozen subtree and `width.to_bits()`
//! captures the one width-conditioned input, so we cache the scalar per
//! `(NodeId, MeasureKind, width)` and skip the re-layout on a repeat. The cache is
//! stack-local to one `layout()` call → it never leaks across renders and needs no
//! invalidation.

use std::cell::RefCell;
use std::collections::HashMap;

use starfish_dom::NodeId;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum MeasureKind {
    FlexBaseMain,
    GridWidth,
    GridHeight,
    TableCellWidth,
    TableCellHeight,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct MeasureKey {
    node: NodeId,
    kind: MeasureKind,
    width_bits: u32,
}

#[derive(Default)]
pub(crate) struct LayoutCache {
    measures: RefCell<HashMap<MeasureKey, f32>>,
}

impl LayoutCache {
    /// Cached intrinsic measurement. On a hit returns the stored scalar; on a
    /// miss runs `compute` and stores it. The `RefCell` borrow is dropped BEFORE
    /// `compute` runs so a nested (recursive) measure can't double-borrow.
    pub(crate) fn measure(
        &self,
        node: NodeId,
        kind: MeasureKind,
        width: f32,
        compute: impl FnOnce() -> f32,
    ) -> f32 {
        let key = MeasureKey {
            node,
            kind,
            width_bits: width.to_bits(),
        };
        // The `Ref` from `borrow()` lives only for this `if let`; it is dropped at
        // the closing brace, so `compute()` below never runs while a borrow is held.
        if let Some(v) = self.measures.borrow().get(&key) {
            return *v;
        }
        #[cfg(test)]
        MEASURE_UNCACHED_CALLS.with(|c| c.set(c.get() + 1));
        let v = compute();
        self.measures.borrow_mut().insert(key, v);
        v
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static MEASURE_UNCACHED_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}
