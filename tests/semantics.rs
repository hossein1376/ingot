use ingotc::check::analyze;
use ingotc::interp;
use ingotc::source::Source;

fn run(source: &str) -> i32 {
    let analysis = analyze(Source::new("test.ingot", source));
    for d in &analysis.diagnostics {
        eprintln!("{}", d.to_human(&analysis.sources));
    }
    assert!(analysis.diagnostics.is_empty());
    let outcome = interp::run(&analysis).unwrap();
    assert!(outcome.trap.is_none(), "{:?}", outcome.trap);
    outcome.status
}

#[test]
fn u64_wrapping_multiply_uses_the_u64_width() {
    let source = r#"
import std.Env;

pub fn main(env: &Env) -> u8 {
    let max: u64 = 18446744073709551615;
    @assert(max *% 2 == 18446744073709551614);
    return 0;
}
"#;
    assert_eq!(run(source), 0);
}

#[test]
fn boolean_operators_short_circuit() {
    let source = r#"
import std.Env;

pub fn main(env: &Env) -> u8 {
    let xs: [3]i64 = [3]i64.{ 1, 2, 3 };
    let i: i64 = 0;
    if i > 0 && xs[i - 1] == 1 {
        return 1;
    }
    if i < 3 || xs[i] == 2 {
        return 0;
    }
    return 2;
}
"#;
    assert_eq!(run(source), 0);
}
