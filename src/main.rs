mod app;
mod cli;
mod output_template;

fn main() -> std::process::ExitCode {
    app::run()
}
