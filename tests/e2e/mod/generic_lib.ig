// Generic helpers, imported with a module alias by tests/e2e/generics.ig.
struct Stack[T] {
    items: [16]T,
    len: i64,
}

fn push[T](s: *Stack.[T], x: T) {
    s.*.items[s.*.len] = x;
    s.*.len += 1;
}

fn pop[T](s: *Stack.[T]) -> T {
    s.*.len -= 1;
    return s.*.items[s.*.len];
}
