/// Compile-fail tests using trybuild.
///
/// These tests verify that certain code patterns correctly fail to compile,
/// particularly around the Guard type being !Send.

#[test]
fn compile_fail_tests() {
	let t = trybuild::TestCases::new();
	t.compile_fail("tests/ui/*.rs");
}
