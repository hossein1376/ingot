//! Diagnostics: stable codes, spans, related spans, fix edits and the JSON line format.
//!
//! The catalog in `docs/impl/diagnostics.md` is the source of truth for the code,
//! name and fix of every condition. This MVP emits the subset its phases can
//! decide; the table below is extended with the catalog as phases land.

use crate::source::{Source, Span};
use std::fmt::Write as _;

/// The target `ingot check` uses when `--target` is not given (the first of spec
/// section 24).
pub const DEFAULT_TARGET: &str = "aarch64-macos-m4";

#[derive(Clone, Debug)]
pub struct Edit {
    /// The file the edit changes, relative to the package root. It is not
    /// necessarily the file of the diagnostic that offers it.
    pub file: String,
    pub span: Span,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Fix {
    pub message: String,
    pub edits: Vec<Edit>,
}

#[derive(Clone, Debug)]
pub struct Related {
    pub role: &'static str,
    pub file: String,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub code: &'static str,
    pub name: &'static str,
    pub message: String,
    pub file: String,
    pub span: Span,
    pub related: Vec<Related>,
    pub fixes: Vec<Fix>,
}

impl Diagnostic {
    pub fn new(
        code: &'static str,
        message: impl Into<String>,
        source: &Source,
        span: Span,
    ) -> Self {
        Diagnostic {
            code,
            name: name_of(code),
            message: message.into(),
            file: source.path.clone(),
            span,
            related: Vec::new(),
            fixes: Vec::new(),
        }
    }

    pub fn related(&mut self, role: &'static str, source: &Source, span: Span) -> &mut Self {
        self.related.push(Related {
            role,
            file: source.path.clone(),
            span,
        });
        self
    }

    /// Add a related span in the same file as the primary span.
    pub fn relate(&mut self, role: &'static str, span: Span) -> &mut Self {
        let file = self.file.clone();
        self.related.push(Related { role, file, span });
        self
    }

    /// Add a related span in another file.
    pub fn relate_in(&mut self, role: &'static str, file: &str, span: Span) -> &mut Self {
        self.related.push(Related {
            role,
            file: file.to_string(),
            span,
        });
        self
    }

    pub fn fix(&mut self, message: impl Into<String>, edit: Edit) -> &mut Self {
        self.fixes.push(Fix {
            message: message.into(),
            edits: vec![edit],
        });
        self
    }

    pub fn fix_edits(&mut self, message: impl Into<String>, edits: Vec<Edit>) -> &mut Self {
        self.fixes.push(Fix {
            message: message.into(),
            edits,
        });
        self
    }

    pub fn to_json(&self, sources: &crate::source::Sources) -> String {
        self.to_json_for(sources, DEFAULT_TARGET)
    }

    /// The JSON line of this diagnostic, which occurs on `target`.
    pub fn to_json_for(&self, sources: &crate::source::Sources, target: &str) -> String {
        self.to_json_for_targets(sources, &[target])
    }

    /// The JSON line of this diagnostic on several targets (`--all-targets`).
    pub fn to_json_for_targets(
        &self,
        sources: &crate::source::Sources,
        targets: &[&str],
    ) -> String {
        let mut out = String::new();
        out.push_str("{\"type\":\"diagnostic\",\"code\":");
        json_string(&mut out, self.code);
        out.push_str(",\"name\":");
        json_string(&mut out, self.name);
        out.push_str(",\"message\":");
        json_string(&mut out, &self.message);
        out.push_str(",\"span\":");
        write_span(&mut out, sources, &self.file, self.span);
        out.push_str(",\"related\":[");
        for (i, r) in self.related.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"role\":");
            json_string(&mut out, r.role);
            out.push_str(",\"span\":");
            write_span(&mut out, sources, &r.file, r.span);
            out.push('}');
        }
        out.push_str("],\"fixes\":[");
        for (i, f) in self.fixes.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"message\":");
            json_string(&mut out, &f.message);
            out.push_str(",\"edits\":[");
            for (j, e) in f.edits.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                out.push_str("{\"span\":");
                write_span(&mut out, sources, &e.file, e.span);
                out.push_str(",\"text\":");
                json_string(&mut out, &e.text);
                out.push('}');
            }
            out.push_str("]}");
        }
        out.push_str("],\"targets\":[");
        for (i, target) in targets.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            json_string(&mut out, target);
        }
        out.push_str("]}");
        out
    }

    /// The human-readable form. `json` is the tool protocol; this is for terminals.
    pub fn to_human(&self, sources: &crate::source::Sources) -> String {
        let src = sources.iter().find(|s| s.path == self.file);
        let pos = src
            .map(|s| s.position(self.span.start))
            .unwrap_or(crate::source::Position { line: 1, col: 1 });
        let mut out = format!(
            "{}:{}:{}: {} {}: {}\n",
            self.file, pos.line, pos.col, self.code, self.name, self.message
        );
        if let Some(src) = src {
            let line = src.line_text(pos.line);
            let _ = writeln!(out, "  | {}", line);
            let mut caret = String::from("  | ");
            for _ in 1..pos.col {
                caret.push(' ');
            }
            let width = self
                .span
                .end
                .saturating_sub(self.span.start)
                .max(1)
                .min(line.len().saturating_sub(pos.col as usize - 1).max(1));
            for _ in 0..width {
                caret.push('^');
            }
            out.push_str(&caret);
            out.push('\n');
        }
        for r in &self.related {
            let p = sources
                .iter()
                .find(|s| s.path == r.file)
                .map(|s| s.position(r.span.start))
                .unwrap_or(crate::source::Position { line: 1, col: 1 });
            let _ = writeln!(out, "  = {} at {}:{}:{}", r.role, r.file, p.line, p.col);
        }
        for f in &self.fixes {
            let _ = writeln!(out, "  help: {}", f.message);
        }
        out
    }
}

pub fn write_span(out: &mut String, sources: &crate::source::Sources, file: &str, span: Span) {
    let (start, end) = sources
        .iter()
        .find(|s| s.path == file)
        .map(|s| (s.position(span.start), s.position(span.end)))
        .unwrap_or((
            crate::source::Position { line: 1, col: 1 },
            crate::source::Position { line: 1, col: 1 },
        ));
    out.push_str("{\"file\":");
    json_string(out, file);
    let _ = write!(
        out,
        ",\"start\":{{\"line\":{},\"col\":{},\"offset\":{}}},\"end\":{{\"line\":{},\"col\":{},\"offset\":{}}}}}",
        start.line, start.col, span.start, end.line, end.col, span.end
    );
}

pub fn json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn sort_diagnostics(diags: &mut [Diagnostic]) {
    diags.sort_by(|a, b| {
        a.file
            .as_bytes()
            .cmp(b.file.as_bytes())
            .then(a.span.start.cmp(&b.span.start))
            .then(a.code.cmp(b.code))
    });
}

/// Name for a diagnostic code. Extend this table when a new code is emitted.
pub fn name_of(code: &str) -> &'static str {
    match code {
        "E0101" => "invalid_utf8",
        "E0102" => "carriage_return",
        "E0103" => "non_ascii",
        "E0104" => "bad_identifier",
        "E0105" => "bad_number",
        "E0106" => "bad_escape",
        "E0107" => "unterminated_literal",
        "E0108" => "bad_char_literal",
        "E0109" => "unknown_builtin",
        "E0110" => "block_comment",
        "E0111" => "unknown_token",
        "E0201" => "unexpected_token",
        "E0202" => "missing_semicolon",
        "E0203" => "missing_braces",
        "E0204" => "mixed_operators",
        "E0205" => "chained_comparison",
        "E0206" => "not_a_statement",
        "E0207" => "block_has_no_value",
        "E0208" => "missing_capture_list",
        "E0209" => "struct_literal_syntax",
        "E0210" => "item_not_allowed_here",
        "E0211" => "type_args_syntax",
        "E0212" => "keyword_as_name",
        "E0213" => "bad_modifier",
        "E0301" => "unresolved_import",
        "E0302" => "duplicate_import_name",
        "E0303" => "package_cycle",
        "E0304" => "undefined_name",
        "E0305" => "undefined_type",
        "E0306" => "private_item",
        "E0307" => "private_field",
        "E0308" => "duplicate_definition",
        "E0309" => "shadowing",
        "E0310" => "unused_import",
        "E0311" => "unused_binding",
        "E0312" => "var_never_mutated",
        "E0313" => "ambiguous_method",
        "E0314" => "unknown_member",
        "E0315" => "private_construction",
        "E0316" => "ambiguous_import",
        "E0317" => "undefined_label",
        "E0401" => "type_mismatch",
        "E0402" => "lossy_as",
        "E0403" => "condition_not_bool",
        "E0404" => "cannot_infer_type",
        "E0405" => "literal_out_of_range",
        "E0406" => "invalid_operand",
        "E0407" => "int_vector_plain_arith",
        "E0408" => "bad_bits",
        "E0409" => "bad_zero",
        "E0410" => "wrong_argument_count",
        "E0411" => "not_callable",
        "E0412" => "missing_return",
        "E0413" => "unused_result",
        "E0414" => "discard_not_allowed",
        "E0415" => "index_not_i64",
        "E0416" => "not_indexable",
        "E0417" => "bad_enum_conversion",
        "E0418" => "raw_pointer_member",
        "E0419" => "needs_shared_reborrow",
        "E0420" => "array_slice_needs_borrow",
        "E0421" => "value_in_void_return",
        "E0422" => "bad_format_string",
        "E0423" => "bad_atomic",
        "E0501" => "copy_needs_copy_parts",
        "E0502" => "missing_linear",
        "E0503" => "struct_literal_fields",
        "E0504" => "duplicate_from_variant",
        "E0505" => "bad_enum_tag",
        "E0506" => "impl_wrong_module",
        "E0507" => "duplicate_impl",
        "E0508" => "impl_for_type_parameter",
        "E0509" => "impl_mismatch",
        "E0510" => "not_dyn_capable",
        "E0511" => "static_not_sync",
        "E0512" => "initializer_not_constant",
        "E0513" => "static_borrows_non_static",
        "E0514" => "bad_main",
        "E0515" => "bad_align",
        "E0516" => "infinite_type",
        "E0517" => "bad_self_type",
        "E0518" => "missing_body",
        "E0519" => "bad_associated_type",
        "E0520" => "alias_cycle",
        "E0521" => "negative_length",
        "E0522" => "bad_tag_type",
        "E0523" => "not_an_interface",
        "E0524" => "interface_as_type",
        "E0525" => "impl_not_allowed",
        "E0601" => "non_exhaustive_match",
        "E0602" => "unreachable_arm",
        "E0603" => "bad_pattern",
        "E0604" => "refutable_pattern",
        "E0605" => "break_outside_loop",
        "E0606" => "assign_to_immutable",
        "E0607" => "not_a_place",
        "E0608" => "not_iterable",
        "E0609" => "bad_range",
        "E0610" => "mut_borrow_of_immutable",
        "E0611" => "bad_capture",
        "E0612" => "when_not_constant",
        "E0613" => "early_exit_of_consuming_loop",
        "E0701" => "unhandled_result",
        "E0702" => "try_cannot_propagate",
        "E0703" => "try_error_mismatch",
        "E0704" => "not_a_result",
        "E0705" => "fail_not_fallible",
        "E0706" => "fail_type_mismatch",
        "E0707" => "handler_falls_through",
        "E0708" => "exit_in_deferred_block",
        "E0709" => "errdefer_not_fallible",
        "E0710" => "return_of_result",
        "E0801" => "bound_not_satisfied",
        "E0802" => "missing_bound",
        "E0803" => "cannot_infer_type_argument",
        "E0804" => "wrong_type_argument_count",
        "E0805" => "not_generic",
        "E0806" => "constant_evaluation_trapped",
        "E0807" => "not_constant",
        "E0808" => "evaluation_limit",
        "E0809" => "instantiation_depth",
        "E0810" => "static_assert_failed",
        "E0811" => "layout_depends_on_parameter",
        "E0812" => "constant_cycle",
        "E0813" => "wrong_generic_argument_kind",
        "E1001" => "use_after_move",
        "E1002" => "move_out_of_borrow",
        "E1003" => "linear_not_consumed",
        "E1004" => "linear_overwritten",
        "E1005" => "moved_but_deferred",
        "E1006" => "partial_move_of_linear",
        "E1101" => "assign_while_borrowed",
        "E1102" => "exclusive_borrow_conflict",
        "E1103" => "use_while_exclusively_borrowed",
        "E1104" => "move_while_borrowed",
        "E1105" => "borrow_outlives_place",
        "E1201" => "missing_from",
        "E1202" => "origin_not_declared",
        "E1203" => "returns_local_borrow",
        "E1204" => "store_needs_into",
        "E1205" => "local_borrow_escapes_through_parameter",
        "E1206" => "bad_origin_clause",
        "E1207" => "into_blocks_fn_conversion",
        "E1301" => "needs_unsafe",
        "E1302" => "missing_unsafe_reason",
        "E1303" => "unsafe_impl_mismatch",
        "E1304" => "unneeded_unsafe",
        "E1305" => "unsafe_fn_value",
        "E1401" => "not_send",
        "E1402" => "capture_not_sync",
        "E1403" => "task_borrows_scope_local",
        "E1404" => "spawn_statement_not_void",
        "E1405" => "not_a_scope",
        "E1406" => "par_for_not_splittable",
        "E1407" => "par_for_outer_mutation",
        "E1408" => "par_for_exit",
        "E1409" => "not_waitable",
        "E1410" => "bad_waitable",
        "E1501" => "bad_vector_type",
        "E1502" => "bad_lane_index",
        "E1503" => "vector_shape_mismatch",
        "E1504" => "bad_shuffle",
        "E1510" => "bad_kernel_signature",
        "E1511" => "forbidden_in_kernel",
        "E1512" => "trapping_op_in_kernel",
        "E1513" => "bad_kernel_index",
        "E1514" => "kernel_type_unsupported",
        "E1515" => "not_a_kernel",
        "E1516" => "not_an_executor",
        "E1517" => "domain_mismatch",
        "E1518" => "dispatch_argument_mismatch",
        "E1519" => "bad_reduce",
        "E1601" => "not_ffi_type",
        "E1602" => "bad_variadic_argument",
        "E1603" => "unknown_abi",
        "E1604" => "bad_export",
        "E1605" => "not_on_this_target",
        "E1606" => "unknown_target_field",
        "E1701" => "bad_manifest",
        "E1702" => "unknown_target",
        "E1703" => "dependency_hash_mismatch",
        "E1704" => "no_entry_point",
        "E1705" => "bad_module_name",
        "E1706" => "test_only",
        "E1707" => "duplicate_test_name",
        "E1708" => "conflicting_package",
        "E1709" => "dependency_lacks_target",
        "E1710" => "compiler_too_old",
        "E1711" => "duplicate_export",
        "E1712" => "dependency_not_fetched",
        _ => "unknown_code",
    }
}
