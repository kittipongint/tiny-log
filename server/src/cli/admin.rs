use crate::auth::password;
use crate::config::Config;
use crate::db;
use crate::models::log::ms_to_rfc3339;
use crate::state::AppState;
use chrono::Utc;
use clap::Subcommand;
use std::io::{self, Write};

#[derive(Debug, Subcommand)]
pub enum AdminCommand {
    /// Create the admin account
    Create,
    /// Change the admin password
    Passwd,
    /// Show admin account info
    Info,
}

pub async fn run(config: Config, command: AdminCommand) -> anyhow::Result<()> {
    let state = AppState::new(config).await?;
    state.migrate().await?;

    match command {
        AdminCommand::Create => create(&state).await,
        AdminCommand::Passwd => passwd(&state).await,
        AdminCommand::Info => info(&state).await,
    }
}

fn prompt(label: &str) -> anyhow::Result<String> {
    eprint!("{label}");
    io::stderr().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    Ok(buf.trim_end_matches(['\r', '\n']).to_string())
}

fn prompt_secret(label: &str) -> anyhow::Result<String> {
    eprint!("{label}");
    io::stderr().flush()?;

    // Interactive terminals: hide input. Piped/scripted stdin: read a line.
    if std::io::IsTerminal::is_terminal(&io::stdin()) {
        let value = rpassword::read_password()?;
        eprintln!();
        Ok(value)
    } else {
        let mut buf = String::new();
        io::stdin().read_line(&mut buf)?;
        Ok(buf.trim_end_matches(['\r', '\n']).to_string())
    }
}

async fn create(state: &AppState) -> anyhow::Result<()> {
    if db::admin::get_admin(&state.system_db).await?.is_some() {
        anyhow::bail!("admin user already exists");
    }

    let username = prompt("Username: ")?;
    if username.trim().is_empty() {
        anyhow::bail!("username is required");
    }

    let password = prompt_secret("Password: ")?;
    let confirm = prompt_secret("Confirm password: ")?;
    if password != confirm {
        anyhow::bail!("passwords do not match");
    }

    let hash = password::hash_password(&password)?;
    let now = Utc::now().timestamp_millis();
    db::admin::create_admin(&state.system_db, username.trim(), &hash, now).await?;
    println!("Admin user created.");
    Ok(())
}

async fn passwd(state: &AppState) -> anyhow::Result<()> {
    let admin = db::admin::get_admin(&state.system_db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("admin user does not exist"))?;

    let current = prompt_secret("Current password: ")?;
    if !password::verify_password(&current, &admin.password_hash)? {
        anyhow::bail!("current password is incorrect");
    }

    let new_password = prompt_secret("New password: ")?;
    let confirm = prompt_secret("Confirm password: ")?;
    if new_password != confirm {
        anyhow::bail!("passwords do not match");
    }

    let hash = password::hash_password(&new_password)?;
    let now = Utc::now().timestamp_millis();
    db::admin::update_password(&state.system_db, &hash, now).await?;
    db::sessions::delete_all_sessions(&state.system_db).await?;
    println!("Password updated. All sessions invalidated.");
    Ok(())
}

async fn info(state: &AppState) -> anyhow::Result<()> {
    let admin = db::admin::get_admin(&state.system_db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("admin user does not exist"))?;

    println!("Username: {}", admin.username);
    println!("Created: {}", ms_to_rfc3339(admin.created_at));
    println!("Updated: {}", ms_to_rfc3339(admin.updated_at));
    Ok(())
}
