union U { a: i64, b }

fn make() -> U {
    return U.a(1);
}

pub fn main() -> i32 {
    let u: U = U.a(5);
    switch u {
        U.a(&p) => { p.* = 6; }
        U.b => {}
    }
    switch make() {
        U.a(&q) => { q.* = 7; }
        U.b => {}
    }
    return 0;
}
