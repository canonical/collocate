use clap::Parser;
use collocate_cli::cli::Cli;
use collocate_cli::commands::run;

fn main() {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("collocate: {e}");
            std::process::exit(e.exit_code());
        }
    }
}
