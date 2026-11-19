mod app;
mod auth;
mod callback;
mod car;
mod garage;
mod mail;
mod tracking;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    app::run().await
}
