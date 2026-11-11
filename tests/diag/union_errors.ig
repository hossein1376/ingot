union U {
    a: i64,
    b: f64,
    c,
}

fn f(u: U) -> i64 {
    let x: U = U.d(1);
    let y: U = U.c(2);
    let z: U = U.{ a: 1 };
    switch u {
        U.a(n) => { return n; }
        U.c(k) => { return 0; }
    }
    return 0;
}
