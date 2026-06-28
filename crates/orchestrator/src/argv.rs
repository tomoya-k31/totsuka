use totsuka_config::schema::ClaudeArgvSection;
use totsuka_core::Phase;

pub fn merge_argv(cfg: &ClaudeArgvSection, repo: &str, phase: &Phase) -> Vec<String> {
    let mut out = cfg.global.clone();
    if let Some(r) = cfg.per_repo.get(repo) {
        out.extend(r.extra.clone());
    }
    if let Some(p) = cfg.per_phase.get(phase.as_snake()) {
        out.extend(p.extra.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use totsuka_config::schema::ClaudeArgvExtra;

    fn extra(args: &[&str]) -> ClaudeArgvExtra {
        ClaudeArgvExtra {
            extra: args.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn all_three_layers_appended() {
        let mut cfg = ClaudeArgvSection {
            global: vec!["--g".into()],
            per_repo: HashMap::new(),
            per_phase: HashMap::new(),
        };
        cfg.per_repo.insert("x/y".into(), extra(&["--r"]));
        cfg.per_phase.insert("design".into(), extra(&["--p"]));
        let out = merge_argv(&cfg, "x/y", &Phase::Design);
        assert_eq!(out, vec!["--g", "--r", "--p"]);
    }

    #[test]
    fn missing_repo_just_skipped() {
        let cfg = ClaudeArgvSection {
            global: vec!["--g".into()],
            per_repo: HashMap::new(),
            per_phase: HashMap::new(),
        };
        assert_eq!(merge_argv(&cfg, "no/such", &Phase::Design), vec!["--g"]);
    }
}
