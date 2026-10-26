use std::env;

fn main() {
    soul2d_core::crashdump::install("soul2d-crashdump");
    let args: Vec<String> = env::args().skip(1).collect();
    let url = env::var("SOUL2D_MCP_URL").unwrap_or_default();
    let bearer = env::var("SOUL2D_MCP_BEARER").unwrap_or_default();
    match soul2d_core::njc::run(&args, &url, &bearer) {
        Ok(output) => print!("{output}"),
        Err(message) => {
            eprintln!("njc: {message}");
            std::process::exit(1);
        }
    }
}
