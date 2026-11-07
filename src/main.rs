use json_rpc_2_backend::BackendBuilder;

mod auth;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let builder = BackendBuilder::from_dotenv()?;
    let auth = auth::plugin_from_env()?;
    let methods = auth.clone();
    let admin_database = auth::admin_database_from_env().await?;
    let admin_methods = auth.clone();
    let backend = builder
        .default_instance_id("brnavi")
        .log_level("info")
        .configure(move |rpc| {
            auth::register(&methods, rpc)?;
            auth::register_user_admin_methods(&admin_methods, rpc, admin_database.clone())?;
            Ok(())
        })
        .plugin(auth)
        .build()
        .await?;

    backend.run_until_signal().await
}
