use totsukactl::error::TotsukactlError;

#[test]
fn codes_are_unique_and_prefixed() {
    let variants = [
        TotsukactlError::Io(std::io::Error::other("x")),
        TotsukactlError::Toml("x".into()),
        TotsukactlError::Config("x".into()),
        TotsukactlError::Migrate("x".into()),
        TotsukactlError::Compose("x".into()),
        TotsukactlError::Probe("x".into()),
        TotsukactlError::Spawn("x".into()),
        TotsukactlError::Health("x".into()),
        TotsukactlError::SchemaOutOfRange {
            got: 5,
            min: 6,
            target: 6,
        },
        TotsukactlError::SupervisorUnreachable("x".into()),
        TotsukactlError::AlreadyRunning("x".into()),
        TotsukactlError::NotRunning,
        TotsukactlError::UnknownChild("x".into()),
        TotsukactlError::Timeout("x".into()),
        TotsukactlError::Internal("x".into()),
    ];
    let codes: Vec<&str> = variants.iter().map(|e| e.code()).collect();
    for c in &codes {
        assert!(c.starts_with("/errors/"), "{c} missing /errors/ prefix");
    }
    let set: std::collections::HashSet<_> = codes.iter().copied().collect();
    assert_eq!(set.len(), codes.len(), "duplicate code in TYPE_URI mapping");
}
