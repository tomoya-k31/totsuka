use totsuka_core::{Phase, TaskId};

pub fn phase_short(phase: Phase) -> &'static str {
    match phase {
        Phase::Design => "design",
        Phase::ImplVerify => "implv",
    }
}

pub fn branch_name(task: &TaskId, phase: Phase) -> String {
    format!("totsuka/{}/{}", task.short(), phase_short(phase))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn design_branch_uses_short_12() {
        let t = TaskId::new("PVTI_lAHOAjcRPs4AHvuRzgVabcdef123456");
        assert_eq!(
            branch_name(&t, Phase::Design),
            "totsuka/abcdef123456/design"
        );
    }
    #[test]
    fn implv_short_form() {
        let t = TaskId::new("PVTI_short");
        assert_eq!(
            branch_name(&t, Phase::ImplVerify),
            format!("totsuka/{}/implv", t.short())
        );
    }
}
