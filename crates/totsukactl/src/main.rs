use totsukactl::cli;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let cli = cli::parse();
    match cli::dispatch(cli).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
