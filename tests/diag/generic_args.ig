fn id[T](x: T) -> T {
    return x;
}

fn f() -> i64 {
    let a: i64 = id(1);
    let b: i64 = id.[i64, f64](1);
    return a + b;
}
