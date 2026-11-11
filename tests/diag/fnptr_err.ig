enum E: u8 { bad = 1 }

fn plain(x: i64) -> i64 {
    return x;
}

fn may(x: i64) -> i64 ! E {
    return x;
}

pub fn main() -> i32 {
    let f: fn(i64) -> i64 ! E = plain;
    let g: fn(i64) -> i64 = may;
    let h: fn(i64) -> i64 ! i64 = may;
    let v: i64 = f(1);
    return 0;
}
