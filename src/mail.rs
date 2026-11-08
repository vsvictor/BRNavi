use anyhow::{Context, Result};
use json_rpc_2_oauth2::{PasswordResetNotice, PasswordResetNotifier};
use lettre::message::header::ContentTransferEncoding;
use lettre::message::{Mailbox, SinglePart};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::extension::ClientId;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use std::sync::Arc;

#[derive(Clone)]
pub struct MailConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub from: Mailbox,
}

impl MailConfig {
    /// Reads `SMTP_HOST`, `SMTP_PORT` (587), `SMTP_USERNAME`, `SMTP_PASSWORD` and
    /// `SMTP_FROM` (defaults to the username). `None` when `SMTP_HOST` is unset.
    pub fn from_env() -> Result<Option<Self>> {
        let Some(host) = std::env::var("SMTP_HOST").ok().filter(|v| !v.is_empty()) else {
            return Ok(None);
        };
        let username = std::env::var("SMTP_USERNAME").context("SMTP_USERNAME is required")?;
        let password = std::env::var("SMTP_PASSWORD").context("SMTP_PASSWORD is required")?;
        let from = std::env::var("SMTP_FROM")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| username.clone());
        let port = match std::env::var("SMTP_PORT") {
            Ok(value) => value.parse().context("SMTP_PORT must be a port number")?,
            Err(_) => 587,
        };
        Ok(Some(Self {
            host,
            port,
            username,
            password,
            from: from.parse().context("SMTP_FROM is not a valid mailbox")?,
        }))
    }
}

/// Builds the bilingual reset message. The six-digit code is only part of the body.
pub fn reset_message(from: &Mailbox, notice: &PasswordResetNotice) -> Result<Message> {
    let to: Mailbox = notice.email.parse().context("recipient is not a valid mailbox")?;
    let body = format!(
        "Для вашого акаунта запросили відновлення пароля. Шестизначний код для введення в застосунку XRNavi (дійсний 10–15 хвилин):\n\n\
         {code}\n\n\
         Якщо це були не ви, просто проігноруйте цей лист.\n\n\
         ---\n\n\
         A password reset was requested for your account. Enter this six-digit code in the XRNavi app (valid for 10–15 minutes):\n\n\
         {code}\n\n\
         If this was not you, ignore this message.\n",
        code = notice.reset_code
    );
    Message::builder()
        .from(from.clone())
        .to(to)
        .subject("XRNavi: відновлення пароля / password reset")
        .singlepart(
            SinglePart::builder()
                .header(lettre::message::header::ContentType::TEXT_PLAIN)
                .header(ContentTransferEncoding::QuotedPrintable)
                .body(body),
        )
        .context("failed to build reset message")
}

/// Sends reset codes by e-mail. Failures are reported without the code.
pub fn reset_notifier(config: MailConfig) -> Result<PasswordResetNotifier> {
    let transport = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)
        .context("invalid SMTP host")?
        .port(config.port)
        .credentials(Credentials::new(config.username.clone(), config.password.clone()))
        .authentication(vec![Mechanism::Login, Mechanism::Plain])
        .hello_name(ClientId::Domain("api.mobilespace.app".to_string()))
        .build();
    let from = config.from;
    Ok(Arc::new(move |notice: PasswordResetNotice| {
        let transport = transport.clone();
        let from = from.clone();
        tokio::spawn(async move {
            let message = match reset_message(&from, &notice) {
                Ok(message) => message,
                Err(error) => {
                    eprintln!("reset e-mail not built: {error:#}");
                    return;
                }
            };
            // A pooled SMTP connection may have been closed by the server; retry on a fresh one.
            for attempt in 1..=3u64 {
                match transport.send(message.clone()).await {
                    Ok(_) => return,
                    Err(error) => {
                        eprintln!("reset e-mail not sent (attempt {attempt}/3): {error}");
                        if attempt < 3 {
                            tokio::time::sleep(std::time::Duration::from_secs(attempt * 2)).await;
                        }
                    }
                }
            }
        });
    }))
}

/// Runs every notifier for each notice.
pub fn combine(notifiers: Vec<PasswordResetNotifier>) -> PasswordResetNotifier {
    Arc::new(move |notice: PasswordResetNotice| {
        for notifier in &notifiers {
            notifier(notice.clone());
        }
    })
}

#[cfg(test)]
mod tests;
