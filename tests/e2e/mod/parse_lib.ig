// An error enum and fallible function that other modules use through an alias.
enum ParseErr: u8 { empty = 1, bad = 2 }

fn digit(c: u8) -> i64 ! ParseErr {
    if c < 48 as u8 || c > 57 as u8 {
        return @err(ParseErr.bad);
    }
    return (c - 48 as u8) as i64;
}
