// Error unions: a function declared `-> T ! E` returns either a T or a
// non-zero member of the enum E. Callers must say what happens on failure:
// `try` propagates it (running pending defers), `catch e => ...` handles it.
// Cost: the error code travels in x1 next to the result in x0/d0, and each
// check is one cbz. There are no exceptions and no unwinding.
import "std/libc.ig";

enum ParseErr: u8 {
    empty = 1,
    bad_digit = 2,
    overflow = 3,
}

var cleanups: i64 = 0;

// Parses a decimal number from a NUL-terminated string.
fn parse(s: *u8) -> i64 ! ParseErr {
    if s[0] == 0 {
        return @err(ParseErr.empty);
    }
    var n: i64 = 0;
    var i: i64 = 0;
    while s[i] != 0 {
        let c: u8 = s[i];
        if c < 48 as u8 || c > 57 as u8 {
            return @err(ParseErr.bad_digit);
        }
        if n > 100000000000000000 {
            return @err(ParseErr.overflow);
        }
        n = n * 10 + ((c - 48 as u8) as i64);
        i += 1;
    }
    return n;
}

fn sum2(a: *u8, b: *u8) -> i64 ! ParseErr {
    defer { cleanups += 1; } // runs on success and when a try fails
    let x: i64 = try parse(a);
    let y: i64 = try parse(b);
    return x + y;
}

fn report(s: *u8) -> void ! ParseErr {
    let v: i64 = try parse(s);
    printf(c"  parsed %lld\n", v);
}

pub fn main() -> i32 {
    let a: i64 = sum2(c"40", c"2") catch e => -1;
    let b: i64 = sum2(c"40", c"x2") catch e => -(e as i64);
    let c: i64 = parse(c"") catch _ => 7;
    printf(c"sum2 ok: %lld, sum2 bad: %lld, empty->default: %lld, cleanups: %lld\n", a, b, c, cleanups);
    report(c"77") catch e => {
        return 1;
    };
    report(c"7x") catch e => {
        printf(c"  report failed with code %lld\n", e as i64);
    };
    let inputs: [4]*u8 = [4]*u8.{ c"5", c"10", c"oops", c"20" };
    var total: i64 = 0;
    for i in 0..4 {
        let v: i64 = parse(inputs[i]) catch e => {
            printf(c"  stopping at input %lld (code %lld)\n", i, e as i64);
            break;
        };
        total += v;
    }
    printf(c"total before the bad input: %lld\n", total);
    return 0;
}
