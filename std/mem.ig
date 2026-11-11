// std/mem.ig: explicit memory helpers. Nothing here allocates behind your back.
import "std/libc.ig";

// The VM page size, queried at run time (16 KiB on Apple silicon, usually
// 4 KiB on Linux). _SC_PAGESIZE is 29 on macOS and 30 on Linux.
fn page_size() -> i64 {
    if @is_macos() {
        return sysconf(29);
    }
    return sysconf(30);
}

// A bump allocator: O(1) allocation, everything is freed at once.
struct Arena {
    base: *u8,
    cap: i64,
    used: i64,
}

fn arena_init(a: *Arena, cap: i64) -> bool {
    let p: *u8 = malloc(cap as u64);
    if p == null {
        return false;
    }
    a.* = Arena.{ base: p, cap: cap, used: 0 };
    return true;
}

// Returns null when the arena is exhausted. align must be a power of two.
fn arena_alloc(a: *Arena, size: i64, align: i64) -> *u8 {
    let start: i64 = (a.*.used + align - 1) & (0 - align);
    if start + size > a.*.cap {
        return null;
    }
    a.*.used = start + size;
    return &a.*.base[start];
}

fn arena_reset(a: *Arena) {
    a.*.used = 0;
}

fn arena_free(a: *Arena) {
    free(a.*.base);
    a.* = @zero;
}

test "arena allocates aligned, bounded blocks" {
    var a: Arena = @zero;
    @assert(arena_init(&a, 1024));
    let p: *u8 = arena_alloc(&a, 3, 1);
    let q: *u8 = arena_alloc(&a, 8, 8);
    @assert(((q as i64) & 7) == 0);
    @assert((q as i64) - (p as i64) == 8);
    @assert(arena_alloc(&a, 2000, 8) == null);
    arena_reset(&a);
    @assert(a.used == 0);
    arena_free(&a);
    @assert(page_size() >= 4096);
}
