//! Outbound email. Exactly one message exists today: the password reset link.
//!
//! **Why this module is small and stays small.** Every email this system can send is a credential
//! delivery mechanism, so the surface is deliberately one function rather than a general-purpose
//! templating layer. Adding a marketing email here would put an unauthenticated send path next to
//! the one that carries account-takeover tokens.
//!
//! Two rules that are not obvious from the code below:
//!
//! * **The link is never logged.** Not at `info`, not at `debug`, not in an error context. A reset
//!   link is a live credential — logging one is invariant 14's "no secret in any log" applied to
//!   the newest secret in the system. `send_password_reset` therefore takes the link and returns
//!   `()`, and its error path reports *that* a send failed and to whom, never *what* was sent.
//! * **Delivery failure is not the caller's business.** `/auth/password/forgot` answers the same
//!   `202` whether the address exists or not (the non-oracle rule, invariant 18), so it cannot
//!   surface a send failure without breaking that. Failures are logged and swallowed there; this
//!   module's job is to report them accurately, not to decide what happens.

use anyhow::Context;
use lettre::message::header::ContentType;
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::{AsyncTransport, Message, Tokio1Executor};
use std::time::Duration;

/// Bound on one SMTP conversation.
///
/// A named constant beside the code it bounds, for the same reason as `llm.rs`'s timeouts
/// (invariant 28): a bound on our own resource use is a correctness decision, not a deployment
/// preference. Generous, because a relay doing greylisting or a slow TLS handshake is normal and a
/// reset email is not latency-sensitive — but finite, because this runs in a spawned task that
/// would otherwise be able to sit on a socket forever.
const SMTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct Mailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: String,
}

impl Mailer {
    /// Build a mailer from an SMTP URL.
    ///
    /// `smtp://host:1025` is plaintext (the local Mailpit sink); `smtps://user:pass@host:465` is
    /// implicit TLS; `smtp://…?tls=required` upgrades via STARTTLS. Parsing is lettre's, so the
    /// scheme decides the security posture and we do not second-guess it — but note what that
    /// means: **`smtp://` to a real relay sends credentials in the clear.** That is a deployment
    /// decision this constructor cannot make for you, which is why the README says so explicitly.
    pub fn from_url(url: &str, from: &str) -> anyhow::Result<Self> {
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
            .context("SMTP_URL is not a valid SMTP URL")?
            .timeout(Some(SMTP_TIMEOUT))
            .build();
        Ok(Self {
            transport,
            from: from.to_string(),
        })
    }

    /// Deliver a password reset link.
    ///
    /// Plain text, not HTML, and that is deliberate: an HTML mail would want a template, a template
    /// wants interpolation, and interpolation next to a credential is where injection lives. It
    /// also renders identically everywhere, which matters for the one message a locked-out user
    /// must be able to act on.
    pub async fn send_password_reset(&self, to: &str, link: &str) -> anyhow::Result<()> {
        // The body deliberately does not name the tenant or the account. The recipient already
        // knows who they are, and an email is forwardable — so anything it states about the account
        // is stated to whoever ends up reading it.
        let body = format!(
            "Someone asked to reset the password for this email address.\n\n\
             Open this link to choose a new one. It expires in one hour and can be used once:\n\n\
             {link}\n\n\
             If this wasn't you, ignore this email. Your password has not changed, and nothing \
             happens until that link is opened.\n"
        );

        let message = Message::builder()
            .from(
                self.from
                    .parse()
                    .context("MAIL_FROM is not a valid address")?,
            )
            .to(to.parse().context("recipient is not a valid address")?)
            .subject("Reset your password")
            .header(ContentType::TEXT_PLAIN)
            .body(body)
            .context("failed to build the reset email")?;

        // Note what is *not* in this error context: the link. `{e:#}` on a failed send reaches the
        // logs, and a reset link there is a credential in a log file (invariant 14).
        self.transport
            .send(message)
            .await
            .with_context(|| format!("failed to send a password reset email to {to}"))?;
        Ok(())
    }
}
