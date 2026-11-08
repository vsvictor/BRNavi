use json_rpc_2_backend::BackendBuilder;

mod auth;
mod callback;
mod mail;

/// Reset tokens go by e-mail when SMTP is configured. The callback push to the
/// user's other device stays available: it is the fallback without SMTP and can be
/// kept alongside e-mail with `RESET_TOKEN_PUSH=1`.
fn reset_delivery(
    control: Option<json_rpc_2_callback::GatewayControl>,
) -> anyhow::Result<json_rpc_2_oauth2::PasswordResetNotifier> {
    let Some(config) = mail::MailConfig::from_env()? else {
        eprintln!("SMTP_HOST is not set: reset tokens use the callback push only");
        return Ok(callback::reset_notifier(control));
    };
    let mut notifiers = vec![mail::reset_notifier(config)?];
    if std::env::var("RESET_TOKEN_PUSH").is_ok_and(|v| v == "1") {
        notifiers.push(callback::reset_notifier(control));
    }
    Ok(mail::combine(notifiers))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let builder = BackendBuilder::from_dotenv()?;
    let control = callback::control_from_env();
    let auth = auth::plugin_from_env()?.with_reset_notifier(reset_delivery(control.clone())?);
    let methods = auth.clone();
    let admin_database = auth::admin_database_from_env().await?;
    let admin_methods = auth.clone();
    let backend = builder
        .default_instance_id("brnavi")
        .log_level("info")
        .configure(move |rpc| {
            auth::register(&methods, rpc)?;
            callback::register_subscribe(&methods, rpc, control.clone())?;
            auth::register_user_admin_methods(&admin_methods, rpc, admin_database.clone())?;
            Ok(())
        })
        .plugin(auth)
        .build()
        .await?;

    backend.run_until_signal().await
}
