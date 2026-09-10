fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("--live-code-mode-runtime") {
        tool_code_mode::live_guest::main()
    } else {
        tool_code_mode::guest::main()
    }
}
