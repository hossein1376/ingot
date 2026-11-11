enum E: u8 {
    bad = 1,
}

fn f() -> i64 ! E {
    return 1;
}

fn g() -> i64 {
    return try f();
}
