//! Guards against regressions in DB URL construction. The `:totsuka@` literal
//! must never reappear in main.rs.

#[test]
fn main_rs_has_no_hardcoded_totsuka_password() {
    let src = include_str!("../src/main.rs");
    assert!(
        !src.contains(":totsuka@"),
        "orchestrator main.rs has a hardcoded ':totsuka@' password — \
         this regresses spec §11.7 Secret discipline. Use config.postgres.password.expose() instead."
    );
}
