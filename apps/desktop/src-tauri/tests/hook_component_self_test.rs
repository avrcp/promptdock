#[test]
fn isolated_component_receipt_requires_actual_encrypted_record() {
    let receipt = promptdock_desktop_lib::hook_self_test::run_with_executable(
        std::path::Path::new(env!("CARGO_BIN_EXE_promptdock-desktop")),
    )
    .unwrap();
    assert_eq!(receipt.outcome, "local_component_passed");
    assert_eq!(receipt.records, 1);
    assert!(receipt.elapsed_ms < 7000);
}
