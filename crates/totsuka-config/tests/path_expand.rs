use totsuka_config::path_expand::resolve_tilde;

#[test]
fn expands_leading_tilde_slash_with_home() {
    assert_eq!(
        resolve_tilde("~/.config/x", Some("/home/u")),
        "/home/u/.config/x"
    );
}

#[test]
fn expands_bare_tilde_with_home() {
    assert_eq!(resolve_tilde("~", Some("/home/u")), "/home/u");
}

#[test]
fn passes_through_when_home_unset() {
    assert_eq!(resolve_tilde("~/x", None), "~/x");
}

#[test]
fn passes_through_absolute_path() {
    assert_eq!(resolve_tilde("/abs/path", Some("/home/u")), "/abs/path");
}

#[test]
fn passes_through_relative_path_with_tilde_in_middle() {
    // "~foo" is some-other-user notation; we only handle "~/" and bare "~".
    assert_eq!(resolve_tilde("~foo/bar", Some("/home/u")), "~foo/bar");
    assert_eq!(resolve_tilde("dir/~/x", Some("/home/u")), "dir/~/x");
}

#[test]
fn passes_through_empty_string() {
    assert_eq!(resolve_tilde("", Some("/home/u")), "");
}
