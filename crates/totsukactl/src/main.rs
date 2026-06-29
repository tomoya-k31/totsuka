use totsukactl::cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let cli = cli::parse();
    cli::dispatch(cli).await?;
    Ok(())
}
