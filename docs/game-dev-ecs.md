# padlock for Game Development — Data-Oriented Design and ECS

Struct layout is not an optimisation in game engines — it is the architecture. Entity Component System (ECS) and Data-Oriented Design (DOD) store thousands of component instances in contiguous arrays. A single misaligned field in a component struct degrades the entire system's cache performance across every entity, every frame.

padlock gives you the same analysis that game engine teams currently do manually with `sizeof` asserts and comment tables in header files.

---

## Why ECS makes struct layout critical

In a traditional OOP game loop:

```cpp
for (Entity* e : entities) {
    e->update();  // pointer chase — random memory access
}
```

In an ECS game loop:

```cpp
// All Transform components are contiguous in memory
for (Transform& t : transforms) {
    t.position += t.velocity * dt;  // sequential access — cache friendly
}
```

The ECS model only delivers its cache benefit if the component struct itself has no wasted bytes. A single misaligned field forces the CPU to load extra cache lines per entity. At 100 000 entities per frame at 60 fps, that is 360 million unnecessary cache line loads per second.

---

## Example: a poorly ordered Transform component

```cpp
// game/components/transform.h
struct Transform {
    bool    is_dirty;     // 1 byte
    // 7 bytes padding
    double  x, y, z;     // 24 bytes
    bool    is_static;    // 1 byte
    // 3 bytes padding
    float   scale;        // 4 bytes
};                        // total: 40 bytes — 10 wasted (25%)
```

```
$ padlock analyze game/components/transform.h --filter Transform

[✗] Transform  40B  fields=5  holes=2  score=38
    [HIGH] Padding waste: 10B (25%) — 7B after `is_dirty` (offset 1), 3B after `is_static` (offset 25)
    [HIGH] Reorder fields: 40B → 32B (saves 8B): x, y, z, scale, is_dirty, is_static
```

The fix reduces the struct by 8 bytes. With 100 000 entities that is 800 KB — from 3.8 cache lines per entity to 3 cache lines per entity.

```cpp
struct Transform {
    double  x, y, z;     // offset  0, 24 bytes
    float   scale;        // offset 24,  4 bytes
    bool    is_dirty;     // offset 28,  1 byte
    bool    is_static;    // offset 29,  1 byte
    // 2 bytes trailing padding (unavoidable, alignment requirement)
};                        // total: 32 bytes — 2 wasted (6%)
```

```
$ padlock fix game/components/transform.h
```

padlock rewrites the struct in-place, preserving `#pragma once`, comments, and preprocessor guards. A `.bak` backup is saved before any write.

---

## Example: a physics body with false sharing

Physics components are often updated on a separate thread from the main game loop.

```cpp
struct RigidBody {
    std::mutex  physics_mu;   // offset  0, locked by physics thread
    glm::vec3   position;     // offset 40
    glm::vec3   velocity;     // offset 52
    std::mutex  render_mu;    // offset 64, locked by render thread
    float       mass;         // offset 104
};
```

```
$ padlock analyze game/components/rigidbody.h

[✗] RigidBody  112B  fields=5  score=31
    [HIGH] False sharing: cache line 0: [physics_mu, position, velocity]; cache line 1: [render_mu]
           (inferred from type names — add guard annotations or verify with profiling)
```

`physics_mu` and `render_mu` are on adjacent cache lines. A physics thread write to `position` (on cache line 0) forces the render thread's cache line 1 to invalidate on cores that share an L2 cache — even though the render thread only touches `render_mu`.

Fix:

```cpp
// Pad each lock group to its own cache line
struct alignas(64) RigidBody {
    std::mutex  physics_mu;
    glm::vec3   position;
    glm::vec3   velocity;
    // physics data fits in one cache line — verified with padlock explain
    alignas(64)
    std::mutex  render_mu;
    float       mass;
};
```

To convert the inferred finding to confirmed, annotate the fields:

```cpp
struct RigidBody {
    std::mutex  physics_mu;
    int64_t     position_x GUARDED_BY(physics_mu);
    // ...
    std::mutex  render_mu;
    float       mass GUARDED_BY(render_mu);
};
```

---

## Integrating into your CI pipeline

Add to `.github/workflows/padlock.yml`:

```yaml
- name: Check component struct layouts
  uses: gidotencate/padlock@v0.9.3
  with:
    path: game/components/
    fail-on-severity: high
    output-format: sarif
    upload-sarif: 'true'
```

Findings appear as inline annotations on PR diffs. Any new struct that adds more than 30% padding waste fails the build.

---

## Locking down hot-path structs

Once a struct is optimised, use a compile-time assertion to prevent accidental regression:

```cpp
// In your component header — fails to compile if size ever grows unexpectedly
static_assert(sizeof(Transform) == 32, "Transform layout changed — check padding");
static_assert(sizeof(RigidBody) == 128, "RigidBody layout changed — check false sharing");
```

For Rust components, use padlock's proc-macro:

```rust
#[padlock::assert_size(32)]
#[repr(C)]
struct Transform {
    x: f64, y: f64, z: f64,  // 24 bytes
    scale: f32,               //  4 bytes
    is_dirty: bool,           //  1 byte
    is_static: bool,          //  1 byte
    _pad: [u8; 2],            //  2 bytes (explicit — no surprise padding)
}
```

---

## Recommended workflow

1. `padlock analyze game/components/ --sort-by waste` — find the worst offenders
2. `padlock explain game/components/rigidbody.h --filter RigidBody` — inspect the exact layout
3. `padlock fix game/components/` — apply reorders automatically
4. Add `static_assert(sizeof(...))` guards for every hot-path component
5. Add padlock to CI so future contributors don't silently regress layouts

See [docs/findings.md](findings.md) for a full reference on all finding types and severity thresholds.
