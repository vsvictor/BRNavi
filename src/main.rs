use json_rpc_2_backend::BackendBuilder;

mod auth;
mod callback;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let builder = BackendBuilder::from_dotenv()?;
    let control = callback::control_from_env();
    let auth = auth::plugin_from_env()?.with_reset_notifier(callback::reset_notifier(control.clone()));
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
