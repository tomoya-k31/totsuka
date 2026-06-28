use totsuka_config::Config;

#[test]
fn example_file_parses_and_validates() {
    let path = format!(
        "{}/../../examples/totsuka.toml.example",
        env!("CARGO_MANIFEST_DIR")
    );
    let txt = std::fs::read_to_string(&path).expect("read example");
    let cfg = Config::from_toml_str(&txt).expect("parse example");
    cfg.validate().expect("validate example");
}
