#[tokio::main(flavor = "current_thread")]
async fn main() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "__room-mcp")
    {
        if let Err(error) = july_workspace::runtime::run_room_mcp_stdio().await {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = july_workspace::cli::run(std::env::args_os()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
