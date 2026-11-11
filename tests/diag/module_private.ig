import "../e2e/mod/priv_lib.ig" as pl;

pub fn main() -> i32 {
    var s: pl._State = @zero;
    pl._bump(&s);
    return pl.count_to(3) as i32;
}
