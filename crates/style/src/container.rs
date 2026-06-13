//! `@container` size-condition evaluation (E25-M1). A condition is the same
//! `and`/`or`/`not` tree as `@supports`, with a size feature at the leaf. Unlike
//! `@media`, it is evaluated per element against its nearest query container's
//! content-box size (inline + block px), not the viewport.

use starfish_css::{ContainerCondition, SizeAxis, SizeFeature, SizeOp};

/// Whether `cond` holds for a query container of `inline` × `block` px. An
/// `Unknown` leaf (unparseable feature) never matches.
pub(crate) fn container_matches(cond: &ContainerCondition, inline: f32, block: f32) -> bool {
    match cond {
        ContainerCondition::And(v) => v.iter().all(|c| container_matches(c, inline, block)),
        ContainerCondition::Or(v) => v.iter().any(|c| container_matches(c, inline, block)),
        ContainerCondition::Not(c) => !container_matches(c, inline, block),
        ContainerCondition::Size(f) => size_matches(f, inline, block),
        ContainerCondition::Unknown => false,
    }
}

/// A single size feature. `Width`/`InlineSize` query the inline extent,
/// `Height`/`BlockSize` the block extent (horizontal-tb mapping for the MVP).
fn size_matches(f: &SizeFeature, inline: f32, block: f32) -> bool {
    let actual = match f.axis {
        SizeAxis::Width | SizeAxis::InlineSize => inline,
        SizeAxis::Height | SizeAxis::BlockSize => block,
    };
    match f.op {
        SizeOp::Min => actual >= f.value,
        SizeOp::Max => actual <= f.value,
    }
}
