//! xOne command line. Only genesis and chain-spec commands exist so far.

use clap::{builder::PossibleValuesParser, Parser, Subcommand};
use xone_chainspec::Network;

/// The one place the version stamp comes from.
pub fn stamp() -> &'static str {
    env!("XONE_STAMP")
}

#[derive(Parser)]
#[command(
    name = "xone",
    about = "XDC xOne execution client",
    disable_version_flag = true
)]
struct Cli {
    /// Print the version stamp.
    #[arg(short = 'V', long)]
    version: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print the genesis block hash of a built-in network.
    GenesisHash {
        #[arg(long, value_parser = PossibleValuesParser::new(Network::NAMES))]
        network: String,
    },
}

fn genesis_hash(network: &str) -> Result<String, xone_chainspec::Error> {
    let network: Network = network.parse()?;
    Ok(network.genesis()?.hash(&network.spec())?.to_string())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        _ if cli.version => println!("{}", stamp()),
        Some(Command::GenesisHash { network }) => println!("{}", genesis_hash(&network)?),
        None => return Err("no command given, see --help".into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_shape() {
        let s = stamp();
        // xOne / v<semver>-<commit8>[-dirty]-<DDMMYYYY> / <os>-<arch> / rustc<version>
        let parts: Vec<_> = s.split('/').collect();
        assert_eq!(parts.len(), 4, "{s}");
        assert_eq!(parts[0], "xOne");
        assert!(parts[1].starts_with("v0.1.0-"), "{s}");
        let date = parts[1].rsplit('-').next().unwrap();
        assert!(
            date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()),
            "{s}"
        );
        assert_eq!(
            parts[2],
            format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
        );
        assert!(parts[3].starts_with("rustc1."), "{s}");
    }

    #[test]
    fn genesis_hash_command_matches_geth() {
        assert_eq!(
            genesis_hash("mainnet").unwrap(),
            "0x4a9d748bd78a8d0385b67788c2435dcdb914f98a96250b68863a1f8b7642d6b1"
        );
        assert!(genesis_hash("nope").is_err());
    }
}
