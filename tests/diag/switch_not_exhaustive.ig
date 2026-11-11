enum Color { Red, Green, Blue }
fn f(c: Color) -> i64 {
    switch c {
        Color.Red => { return 1; }
    }
    return 0;
}
