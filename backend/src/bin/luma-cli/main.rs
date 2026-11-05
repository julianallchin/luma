//! `luma-cli` — Luma without a window.
//!
//! ```text
//! luma-cli rpc      [host options]            backend commands as JSON lines
//! luma-cli agent    --prompt TEXT …           one agent turn, as native chat runs it
//! luma-cli mcp      [host options]            the Python workspace over MCP stdio
//! luma-cli record   SCORE OUT.mp4 …           a score to video
//! luma-cli render   --venue-id ID --output …  a saved venue to an image
//! luma-cli profile  SCORE OUT …               replay a score through the renderer
//! ```
//!
//! Every subcommand is a thin adapter: the work is `luma_lib`'s, and the ones
//! that need a library boot it through `luma_lib::headless_host`, as the app
//! does. `luma-cli <command> --help` prints that command's flags.

mod agent;
mod mcp;
mod mcp_stdio;
mod profile;
mod record;
mod render;
mod rpc;

/// The arguments after the subcommand.
type Args = std::vec::IntoIter<String>;

const USAGE: &str = "usage: luma-cli <rpc|agent|mcp|record|render|profile> [args]

  rpc      backend commands as JSON lines on stdin and stdout
  agent    one agent turn, as native chat runs it
  mcp      the agent's Python workspace and skills, over MCP stdio
  record   render a score to an mp4
  render   render a saved venue to a PNG or a scene catalogue
  profile  replay a score through production evaluation and rendering

`luma-cli <command> --help` prints a command's flags.";

#[tokio::main]
async fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    let command = args.remove(0);
    let args = args.into_iter();
    let result = match command.as_str() {
        "rpc" => rpc::run(args).await,
        "agent" => agent::run(args).await,
        "mcp" => mcp::run(args).await,
        "record" => record::run(args).await,
        "render" => render::run(args).await,
        "profile" => profile::run(args).await,
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => {
            eprintln!("luma-cli: unknown command `{other}`\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        eprintln!("luma-cli {command}: {error}");
        std::process::exit(1);
    }
}
