# Roadmap — Epic 69: CSS Motion Path (`offset-path`)

CSS Motion Path (`offset-path`/`offset-distance`/`offset-rotate`/`offset-anchor`/
`offset` shorthand) is entirely absent. It positions an element along a path:
the box is translated so its anchor rides a point at `offset-distance` along the
path, optionally rotated to the path tangent. This maps cleanly onto existing
primitives — `parse_path_data` (`crates/paint/src/svg_path.rs`) parses the path,
and `compose_transform` (`crates/paint/src/display.rs`) already builds the box's
`[f32;6]` layer transform, so the motion offset is just a translate(+rotate)
prepended there. `offset-distance` is animatable via the existing `--at` clock.

Computed state is a single BOXED `offset: Option<Box<OffsetPath>>` (8 bytes on
`ComputedStyle`) to respect the recursive style/layout stack-depth limit. Default
`None` → byte-identical to today.

Same per-milestone pipeline. Additive: no `offset-path` → byte-identical (golden
+ existing transform tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E69-M1** | **`offset-path: path()` + `offset-distance`**: boxed `OffsetPath` field; parse `offset-path: path("<svg-path>")` and `offset-distance: <length>\|<percentage>`. At paint time, flatten the path to a polyline (arc-length), find the point at the distance, and translate the box so its center rides that point (prepended in `compose_transform`). | `style`, `paint` | a box with `offset-path:path('M0,0 L200,100'); offset-distance:50%` is translated to the path midpoint (tested + visual) | ✅ |
| **E69-M2** | **`offset-rotate`**: `offset-rotate: auto \| reverse \| <angle> \| auto <angle>`. `auto` rotates the box to the path tangent at the point; `<angle>` adds a fixed rotation; `reverse` = `auto 180deg`. Compose the rotation into the motion transform (around the ride point). | `style`, `paint` | a box with `offset-rotate:auto` along a curve tilts to follow the tangent; a fixed angle rotates it (tested + visual) | ☐ |
| **E69-M3** | **`offset-path: ray()` + basic shapes + `offset` shorthand + `offset-anchor`**: `ray(<angle>)` (a ray from the offset position at the angle), `circle()`/`ellipse()` paths (reuse the slice/shape machinery), `offset-anchor: <position>` (which box point rides the path; default center), and the `offset` shorthand (`offset-path offset-distance / offset-anchor` etc.). | `style`, `paint` | `offset-path:ray(45deg); offset-distance:50px` moves the box up-right along the ray; `offset-anchor:left top` rides the corner (tested + visual) | ☐ |

## Non-goals (deferred)

- Full `offset-position` (the auto start anchor) beyond defaulting to the box's
  static position; `offset-path: <url>` referencing an SVG `<path>` element.
- Exact arc-length reparameterization of cubic/quadratic Béziers beyond a
  fine-polyline flattening approximation.
- `contain`/layout effects of the offset (motion path is a paint-time transform
  like `transform`; it does not affect layout or scroll size).
- `ray()` `contain` / `size` keywords (`closest-side`, etc.) beyond a plain angle
  + distance.
