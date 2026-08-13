#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = july_workspace::cli::run(std::env::args_os()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
