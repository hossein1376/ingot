use ingotc::check::{analyze, analyze_file};
use ingotc::source::Source;
use std::path::PathBuf;

fn codes(text: &str) -> Vec<&'static str> {
    let analysis = analyze(Source::new("test.ingot", text));
    analysis.diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn partial_move_within_linear_struct_is_consumed() {
    let source = r#"
linear struct Token { id: i32 }

impl Token {
    fn make(id: i32) -> Token { return Token.{ id: id }; }
    fn release(self: Token) { let Token.{ id: _ } = self; }
}

linear struct Bundle { token: Token, extra: i64 }

fn f() {
    let b = Bundle.{ token: Token.make(1), extra: 0 };
    b.token.release();
}
"#;
    let analysis = analyze(Source::new("test.ingot", source));
    for d in &analysis.diagnostics {
        eprintln!("{}", d.to_human(&analysis.sources));
    }
    assert!(analysis.diagnostics.is_empty());
}

#[test]
fn local_borrow_through_parameter_is_e1205() {
    let source = r#"
struct Holder { x: &i64 }

fn set(h: &mut Holder) {
    let local: i64 = 1;
    h.x = &local;
}
"#;
    assert!(codes(source).contains(&"E1205"));
}

#[test]
fn literal_and_range_match_dispatch() {
    let source = r#"
fn classify(n: i64) -> i64 {
    return match n {
        0 => 10,
        1..=3 => 20,
        else => 30,
    };
}

fn flag(b: bool) -> i64 {
    return match b {
        true => 1,
        false => 0,
    };
}
"#;
    let analysis = analyze(Source::new("test.ingot", source));
    for d in &analysis.diagnostics {
        eprintln!("{}", d.to_human(&analysis.sources));
    }
    assert!(analysis.diagnostics.is_empty());
}

#[test]
fn moving_linear_field_across_module_is_e1006() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/package_linear_move");
    let analysis = analyze_file(&path).unwrap();
    let codes: Vec<&str> = analysis.diagnostics.iter().map(|d| d.code).collect();
    assert!(codes.contains(&"E1006"), "codes: {codes:?}");
}
