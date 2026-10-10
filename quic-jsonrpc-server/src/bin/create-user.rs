//! Administrative account provisioning; credentials are only read from stdin/the terminal.

use std::io::{IsTerminal, Read};

use anyhow::{Result, anyhow, bail};
use quic_jsonrpc_server::{
    env,
    infrastructure::{config::MongoOptions, persistence::MongoAuthenticator},
};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let login = args
        .next()
        .ok_or_else(|| anyhow!("Usage: create-user <email>; MD5 secret is read from stdin"))?;
    if args.next().is_some() {
        bail!("Usage: create-user <email>; never pass credentials as arguments");
    }
    quic_jsonrpc_server::domain::normalize_login(&login).map_err(|_| anyhow!("Invalid email login"))?;
    // Checked loading is single-threaded and never prints a malformed dotenv line.
    env::load_dotenv_checked()?;
    let secret = if std::io::stdin().is_terminal() {
        rpassword::prompt_password("32-character MD5 password equivalent (hidden): ")
            .map_err(|_| anyhow!("Unable to read password equivalent"))?
    } else {
        let mut input = String::new();
        std::io::stdin()
            .lock()
            .take(128)
            .read_to_string(&mut input)
            .map_err(|_| anyhow!("Unable to read password equivalent"))?;
        input.trim_end_matches(['\r', '\n']).to_string()
    };
    quic_jsonrpc_server::domain::normalize_md5(&secret)
        .map_err(|_| anyhow!("Expected exactly 32 hexadecimal MD5 characters"))?;
    let options = MongoOptions::from_env()?;
    let runtime = tokio::runtime::Runtime::new().map_err(|_| anyhow!("Unable to start runtime"))?;
    runtime.block_on(async {
        let auth = MongoAuthenticator::connect(&options, std::time::Duration::from_secs(86_400)).await?;
        auth.create_user(&login, &secret).await?;
        println!("User created.");
        Ok(())
    })
}
