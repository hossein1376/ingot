//! The extra `ingot cost` fields: `through`, `bytes` and `allocator`.

use ingot_cfg::removal::{CostItem, CostKind};
use ingot_driver::pipeline;

fn analyze(name: &str, source: &str) -> pipeline::Analysis {
    let dir = std::env::temp_dir().join(format!(
        "ingot-cost-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(dir.join("ingot.toml"), "name = \"app\"\n").expect("manifest");
    std::fs::write(dir.join("main.ingot"), source).expect("source");
    let analysis = pipeline::analyze_path(&dir).expect("analysis");
    let _ = std::fs::remove_dir_all(&dir);
    analysis
}

fn of_kind(analysis: &pipeline::Analysis, kind: CostKind) -> Vec<&CostItem> {
    analysis.cost.iter().filter(|i| i.kind == kind).collect()
}

#[test]
fn indirect_calls_name_what_they_go_through() {
    let analysis = analyze(
        "indirect",
        "fn id(x: i64) -> i64 { return x; }\nfn by_fn(f: fn(i64) -> i64, x: i64) -> i64 { return f(x); }\nfn by_dyn(f: &dyn Fn(i64) -> i64, x: i64) -> i64 { return f(x); }\nfn by_extern(p: extern fn(i64) -> i64) -> i64 {\n    unsafe(\"p is live\") { return p(1); }\n}\nfn run(p: extern fn(i64) -> i64) -> i64 {\n    let f = fn [](x: i64) -> i64 { return x; };\n    return by_fn(id, 1) + by_dyn(&f, 1) + by_extern(p);\n}\n",
    );
    let mut through: Vec<&str> = of_kind(&analysis, CostKind::IndirectCall)
        .into_iter()
        .filter_map(|i| i.through)
        .collect();
    through.sort();
    assert_eq!(through, vec!["dyn", "extern", "fn"]);
}

#[test]
fn a_large_copy_has_its_size_in_bytes() {
    let analysis = analyze(
        "copy",
        "struct Big { v: [9]i64 }\nfn take(b: Big) -> void { @forget(b); }\nfn run(b: Big) -> void { take(b); }\n",
    );
    let copies = of_kind(&analysis, CostKind::Copy);
    assert_eq!(copies.len(), 1, "{:?}", analysis.cost);
    assert_eq!(copies[0].bytes, Some(72));
}

#[test]
fn an_alloc_names_its_allocator_argument_when_there_is_one() {
    let analysis = analyze(
        "alloc",
        "import std.mem;\nfn allocate[A: mem.Allocator](a: A) -> *mut u8 ! mem.AllocErr {\n    return try a.alloc(8, 8);\n}\nfn use_it[A: mem.Allocator](a: A) -> void ! mem.AllocErr {\n    @forget(try allocate(a));\n}\nfn put_or_return[A: mem.Allocator, T](a: A, v: T) -> i64 ! mem.AllocFail[T] {\n    @forget(a);\n    @forget(v);\n    return 0;\n}\nfn use_or_return[A: mem.Allocator, T: Discard](a: A, v: T) -> void ! mem.AllocFail[T] {\n    @forget(try put_or_return(a, v));\n}\n",
    );
    let allocs = of_kind(&analysis, CostKind::Alloc);
    assert_eq!(allocs.len(), 3, "{:?}", analysis.cost);
    assert!(allocs.iter().any(|i| i.allocator.as_deref() == Some("A")));
    assert!(allocs.iter().any(|i| i.allocator.is_none()));
}
