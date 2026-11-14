//! `xone account new | import | list | update` (geth `cmd/geth/accountcmd.go`) over
//! `xone_signer::keystore_dir`. Passwords are read from files (first line, mode 0600); nothing
//! secret is printed.

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use alloy_primitives::Address;
use clap::ArgMatches;
use xone_signer::{
    keystore_dir::{self, Kdf},
    local_keystore::read_password_file,
};

use super::fail;

fn keydir(m: &ArgMatches) -> PathBuf {
    if let Some(d) = m.get_one::<String>("keydir") {
        return PathBuf::from(d);
    }
    let datadir = m
        .get_one::<String>("datadir")
        .map_or_else(|| xone_node::Config::default().datadir, PathBuf::from);
    datadir.join("keystore")
}

fn kdf(m: &ArgMatches) -> Kdf {
    if m.get_flag("lightkdf") {
        Kdf::Light
    } else {
        Kdf::Standard
    }
}

fn password(m: &ArgMatches, key: &str) -> Result<xone_signer::Password, String> {
    let p = m.get_one::<String>(key).map_or("", String::as_str);
    read_password_file(Path::new(p), None).map_err(|e| e.to_string())
}

pub fn account(m: &ArgMatches) -> ExitCode {
    let run = || -> Result<String, String> {
        match m.subcommand() {
            Some(("list", s)) => {
                let all = keystore_dir::list(&keydir(s)).map_err(|e| e.to_string())?;
                Ok(all
                    .iter()
                    .enumerate()
                    .map(|(i, a)| {
                        format!(
                            "Account #{i}: {{{}}} keystore://{}\n",
                            alloy_primitives::hex::encode(a.address),
                            a.path.display()
                        )
                    })
                    .collect())
            }
            Some(("new", s)) => {
                let a = keystore_dir::create(&keydir(s), &password(s, "password")?, kdf(s))
                    .map_err(|e| e.to_string())?;
                Ok(format!(
                    "\nYour new key was generated\n\n\
Public address of the key:   {}\n\
Path of the secret key file: {}\n\n\
- You can share your public address with anyone. Others need it to interact with you.\n\
- You must NEVER share the secret key with anyone! The key controls access to your funds!\n\
- You must BACKUP your key file! Without the key, it's impossible to access account funds!\n\
- You must REMEMBER your password! Without the password, it's impossible to decrypt the key!\n\n",
                    a.address.to_checksum(None),
                    a.path.display()
                ))
            }
            Some(("import", s)) => {
                let kf = s.get_one::<String>("keyfile").map_or("", String::as_str);
                let a = keystore_dir::import(
                    &keydir(s),
                    Path::new(kf),
                    &password(s, "password")?,
                    kdf(s),
                    None,
                )
                .map_err(|e| e.to_string())?;
                Ok(format!(
                    "Address: {{{}}}\n",
                    alloy_primitives::hex::encode(a.address)
                ))
            }
            Some(("update", s)) => {
                let raw = s.get_one::<String>("address").map_or("", String::as_str);
                let hex = raw
                    .strip_prefix("xdc")
                    .or_else(|| raw.strip_prefix("XDC"))
                    .map_or_else(|| raw.to_owned(), |h| format!("0x{h}"));
                let address: Address = hex.parse().map_err(|_| format!("{raw}: not an address"))?;
                keystore_dir::update(
                    &keydir(s),
                    address,
                    &password(s, "password")?,
                    &password(s, "new-password")?,
                    kdf(s),
                )
                .map_err(|e| e.to_string())?;
                Ok(String::new())
            }
            _ => Err("account: a subcommand is required".into()),
        }
    };
    match run() {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}
