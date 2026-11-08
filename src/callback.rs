use json_rpc_2_callback::GatewayControl;
use json_rpc_2_oauth2::{
    AuthError, AuthPlugin, JsonRpcAuthServer, PasswordResetNotice, PasswordResetNotifier, RpcError,
    RpcServer,
};
use serde_json::{Value, json};

pub const RESET_TOKEN_METHOD: &str = "auth.passwordResetToken";

/// Control Plane client, or `None` when `GATEWAY_CONTROL_ADDR` is not configured.
pub fn control_from_env() -> Option<GatewayControl> {
    match GatewayControl::from_env() {
        Ok(control) => Some(control),
        Err(error) => {
            tracing_warn(&error.to_string());
            None
        }
    }
}

fn tracing_warn(message: &str) {
    eprintln!("callbacks disabled: {message}");
}

/// Registers `navi.callback.subscribe`: binds the caller's connection to the
/// authenticated user id so the gateway can push messages to it.
pub fn register_subscribe<S>(
    auth: &AuthPlugin<S>,
    rpc: &RpcServer,
    control: Option<GatewayControl>,
) -> anyhow::Result<()>
where
    S: JsonRpcAuthServer<StorageError = AuthError> + Clone + 'static,
{
    let auth = auth.clone();
    rpc.register_with_context("navi.callback.subscribe", move |params, context| {
        let auth = auth.clone();
        let control = control.clone();
        async move {
            let request = auth.authorize(params, &[])?;
            let (Some(control), Some(context)) = (control, context) else {
                return Err(RpcError::internal_error());
            };
            control
                .register_client(&context.connection_id, &request.claims.subject)
                .await
                .map_err(|_| RpcError::internal_error())?;
            Ok(json!({ "subscribed": true }))
        }
    })?;
    Ok(())
}

/// Sends reset tokens only to devices of the same user that already subscribed.
/// The token is never logged; unknown or offline users simply receive nothing.
pub fn reset_notifier(control: Option<GatewayControl>) -> PasswordResetNotifier {
    std::sync::Arc::new(move |notice: PasswordResetNotice| {
        let Some(control) = control.clone() else {
            return;
        };
        tokio::spawn(async move {
            let params: Value = json!({ "reset_token": notice.reset_token });
            let _ = control
                .notify_client(&notice.user_id, RESET_TOKEN_METHOD, Some(params))
                .await;
        });
    })
}

#[cfg(test)]
mod tests;
