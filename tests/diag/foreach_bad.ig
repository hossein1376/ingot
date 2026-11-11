fn total(s: []i64) -> i64 {
    var acc: i64 = 0;
    for v in s {
        acc += v;
    }
    return acc;
}
pub fn main() -> i32 {
    let a: [3]i64 = [3]i64.{ 1, 2, 3 };
    for v in 5 { }
    return total(a[0..3]) as i32;
}
