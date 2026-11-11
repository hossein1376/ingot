fn sum[T](xs: []T) -> T {
    var s: T = @zero;
    for i in 0..xs.len {
        s += xs[i];
    }
    return s;
}
fn grow[T](x: T) -> i64 {
    return grow.[*T](&x);
}
fn f() -> i64 {
    var b: [2]bool = @zero;
    let x: bool = sum.[bool](b[0..2]);
    return 0;
}
