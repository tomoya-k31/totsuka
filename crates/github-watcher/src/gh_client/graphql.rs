//! GraphQL documents for ProjectsV2 status polling.
//!
//! IMPORTANT: every user-supplied value (project owner, project number,
//! cursor) MUST be passed through GraphQL `variables` — never
//! format!-interpolated into the query string. See orchestrator PR #4 for the
//! reasoning. The regression test `tests/graphql_injection.rs` enforces this.

/// Resolve `(owner, number) -> ProjectV2.node_id`. Owner is either user or org;
/// we try `user(login)` first, fall back to `organization(login)` on miss.
pub const PROJECT_NODE_QUERY_USER: &str = r#"
    query($login: String!, $number: Int!) {
      user(login: $login) {
        projectV2(number: $number) { id }
      }
    }
"#;

pub const PROJECT_NODE_QUERY_ORG: &str = r#"
    query($login: String!, $number: Int!) {
      organization(login: $login) {
        projectV2(number: $number) { id }
      }
    }
"#;

/// Page through ProjectV2 items, extracting the Status single-select value and
/// the issue/PR/DraftIssue content.
pub const PROJECT_ITEMS_QUERY: &str = r#"
    query($projectId: ID!, $first: Int!, $after: String) {
      node(id: $projectId) {
        ... on ProjectV2 {
          items(first: $first, after: $after) {
            pageInfo { hasNextPage endCursor }
            nodes {
              id
              fieldValueByName(name: "Status") {
                ... on ProjectV2ItemFieldSingleSelectValue { name }
              }
              content {
                __typename
                ... on Issue       { number closedAt repository { nameWithOwner } }
                ... on PullRequest { number closedAt repository { nameWithOwner } }
                ... on DraftIssue  { id }
              }
            }
          }
        }
      }
    }
"#;
