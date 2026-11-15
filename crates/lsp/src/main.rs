use std::process::ExitCode;

fn main() -> ExitCode {
    let code = ingot_lsp::run(std::io::stdin(), std::io::stdout());
    ExitCode::from(code)
}
