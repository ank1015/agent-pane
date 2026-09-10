use tool_read::{ReadConfig, TextReadOutput, Truncation, read_text};

#[test]
fn immutable_source_windows_preserve_lines_and_report_continuation() {
    let config = ReadConfig::default();
    assert_eq!(
        read_text("first\r\nsecond\r\nlast", Some(2), Some(1), &config).unwrap(),
        TextReadOutput {
            content: "second\r\n".into(),
            start_line: 2,
            end_line: Some(2),
            eof: false,
            truncation: Some(Truncation::LineLimit),
            next_offset: Some(3)
        }
    );
    assert_eq!(
        read_text("first\nlast\n", Some(2), Some(1), &config).unwrap(),
        TextReadOutput {
            content: "last\n".into(),
            start_line: 2,
            end_line: Some(2),
            eof: true,
            truncation: None,
            next_offset: None
        }
    );
    assert_eq!(read_text("", None, None, &config).unwrap().end_line, None);
    assert!(read_text("a\n", Some(2), None, &config).is_err());
    assert!(read_text("a", Some(0), None, &config).is_err());
    assert!(read_text("a", None, Some(0), &config).is_err());
    assert!(read_text("a\0b", None, None, &config).is_err());
}

#[test]
fn byte_budget_keeps_complete_lines_and_errors_on_oversized_first_line() {
    let config = ReadConfig {
        max_output_bytes: 280,
        ..Default::default()
    };
    let output = read_text("12345678\n12345678\n", None, None, &config).unwrap();
    assert_eq!(output.content, "12345678\n");
    assert_eq!(output.truncation, Some(Truncation::ByteLimit));
    assert_eq!(output.next_offset, Some(2));
    assert!(read_text(&"x".repeat(30), None, None, &config).is_err());
}
