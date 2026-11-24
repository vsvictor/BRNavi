use std::{sync::Arc, time::Duration};

use crate::{auth, callback, car::AuthorizedCar, garage, mail, tracking};
use json_rpc_2_backend::{BackendBuilder, FromSettings};
use json_rpc_2_coopnav::{
    CoopNavPlugin,
    cooperative::{CooperativeConfig, GtfsRealtimeSource, RedisCooperativeStorage, TransitSource},
};

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

pub async fn run() -> anyhow::Result<()> {
    let builder = BackendBuilder::from_dotenv()?;
    let control = callback::control_from_env();
    let auth = auth::plugin_from_env()?.with_reset_notifier(reset_delivery(control.clone())?);
    let car = json_rpc_2_car::CarConfig::from_env()?;
    let methods = auth.clone();
    let admin_database = auth::admin_database_from_env().await?;
    let admin_methods = auth.clone();
    let garage_methods = auth.clone();
    let garage_store = garage::MongoGarage::new(admin_database.clone());
    let car_auth = auth.clone();
    let tracking_methods = auth.clone();
    let stage2: Option<tracking::Stage2Env> = tracking::stage2_from_env()?;
    let tracking_grants = tracking::MongoTrackingGrants::new(&admin_database);
    tracking_grants.ensure_indexes().await?;
    let tracking_permissions = Arc::new(tracking::MongoTrackingPermissions::new(
        &admin_database,
        stage2.is_some(),
    ));
    tracking_permissions.initialize().await?;
    let tracking_service = tracking::TrackingService::new(
        Arc::new(tracking::MongoDeviceSessions::new(&admin_database)),
        Arc::new(tracking_grants),
    );
    let mut tracker = CoopNavPlugin::new(
        auth.clone(),
        Arc::new(tracking::HostSessionAuthorizer(tracking_service.clone())),
        Arc::new(tracking::MongoCoopNavStorage::new(&admin_database)),
    );
    if let Some(stage2) = stage2 {
        let transit = stage2
            .gtfs
            .map(|config| {
                GtfsRealtimeSource::new(config)
                    .map(|source| Arc::new(source) as Arc<dyn TransitSource>)
            })
            .transpose()?;
        let cooperative = CooperativeConfig::new(
            Arc::new(
                RedisCooperativeStorage::connect(&stage2.redis_url, "coopnav:v1")
                    .await
                    .map_err(|error| anyhow::anyhow!(error))?,
            ),
            transit,
            Duration::from_secs(2),
            Duration::from_millis(500),
        )?
        .with_shared_epoch(stage2.epoch)?
        .with_shared_brand(stage2.brand)?;
        tracker = tracker.with_cooperative(cooperative);
        tracker.section(1, true).await?;
        tracker.section(2, true).await?;
    }
    let allowlist = tracking::tracking_permissions(tracker.sections().section_enabled(2).await?);
    let backend = builder
        .default_instance_id("brnavi")
        .log_level("info")
        .configure(move |rpc| {
            auth::register(&methods, rpc)?;
            callback::register_subscribe(&methods, rpc, control.clone())?;
            auth::register_user_admin_methods(&admin_methods, rpc, admin_database.clone())?;
            garage::register(&garage_methods, rpc, garage_store.clone())?;
            tracking::register(
                &tracking_methods,
                rpc,
                tracking_service.clone(),
                tracking_permissions.clone(),
                allowlist,
            )?;
            Ok(())
        })
        .plugin(auth)
        .plugin(AuthorizedCar::new(car, car_auth))
        .plugin(tracker)
        .build()
        .await?;

    backend.run_until_signal().await
}
