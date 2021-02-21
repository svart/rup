#[macro_use]
extern crate clap;
use clap::App;

const DEFAULT_CONFIG_NAME: &str = "config.json";

fn main() {
    let yaml = load_yaml!("cli.yml");
    let matches = App::from_yaml(yaml).get_matches();

    let config_name = matches.value_of("config").unwrap_or(DEFAULT_CONFIG_NAME);
    println!("Hello! I will use config {}", config_name);
}
