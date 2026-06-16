# Roadmap — Epic 45: transforms round 2 (individual props, 3D flatten, perspective)

Extends 2D transforms (E5-M3: `transform` translate/scale/rotate/skew/matrix +
transform-origin) with the individual `translate`/`rotate`/`scale` properties, a
flattened 3D-function subset, and `perspective`/`backface-visibility`. The
rasterizer is 2D (tiny-skia), so 3D is a best-effort flatten to a 2D affine.

Same per-milestone pipeline. Additive: pages using only the existing 2D
`transform` render byte-identically (golden + existing transform tests unchanged).

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E45-M1** | **Individual `translate`/`rotate`/`scale` properties**: parsed into style fields and composed into the effective transform in spec order (translate, rotate, scale, then `transform`). | `style`, `paint` | `translate: 20px 10px; rotate: 45deg; scale: 2` compose like the equivalent `transform`, and combine with a transform declaration (tested + visual) | ✅ |
| **E45-M2** | **3D function flatten**: `translate3d`/`translateZ`, `rotateX`/`rotateY`/`rotateZ`/`rotate3d`, `scale3d`/`scaleZ`, `perspective()`, `matrix3d` parsed and flattened to a 2D affine (rotateX→y-axis cos-scale, rotateY→x-axis cos-scale, translateZ ignored without perspective, etc.). | `style` | `transform: rotateY(60deg)` foreshortens horizontally (x-scale ≈ cos 60° = .5); `translate3d(10px,20px,5px)` translates by (10,20); rotateZ == rotate (tested + visual) | ✅ |
| **E45-M3** | **`perspective` + `backface-visibility`**: parse `perspective`/`perspective-origin`; `backface-visibility: hidden` hides an element rotated past 90° on X/Y (its back faces the viewer); `transform-style` parsed. | `style`, `paint` | a `rotateY(180deg)` element with `backface-visibility:hidden` is not painted; visible paints it (tested + visual) | ✅ |

## Non-goals (deferred)

- True 3D rendering / z-ordering / perspective projection of children
  (`transform-style: preserve-3d` is parsed but renders flat); real `perspective`
  foreshortening geometry beyond the flattened-affine approximation.
- 3D `transform-origin` z component, `rotate3d` arbitrary-axis exactness (MVP
  handles axis-aligned + a reasonable general case), and per-vertex perspective.
