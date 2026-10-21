//! The mailer: SMTP transport plus the two mails the email flows send (issue
//! #150, roadmap P6-1).
//!
//! A deployment opts in with three variables together — `OXSUM_SMTP_URL`,
//! `OXSUM_MAIL_FROM` and `OXSUM_PUBLIC_URL` — and a deployment without them has
//! no mailer at all: registration sends nothing, `forgot` answers its
//! indistinguishable 200, and `verify/request` is the one endpoint that reports
//! the absence, because sending the mail is its whole job.
//!
//! The transport is lettre's async SMTP over `tokio1-rustls-tls`; `smtps://`
//! URLs connect wrapped, `smtp://` upgrades with STARTTLS when the server offers
//! it. The mail itself is plain text: a verification or reset link under the
//! public URL, never the token alone — the page route is what consumes it.

use lettre::message::{Mailbox, Message};
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::{AsyncTransport, Tokio1Executor};

/// The configured mailer: transport, sender identity and the deployment's
/// public base URL for building links.
#[derive(Debug, Clone)]
pub struct Mailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    public_url: String,
}

impl Mailer {
    /// Builds a mailer from its URL pieces: the SMTP connection string
    /// (`smtps://user:pass@host` or `smtp://` for STARTTLS), the `From:` address
    /// and the deployment's public URL that links point at.
    ///
    /// # Errors
    ///
    /// A descriptive `Err` when the URL is not a lettre SMTP URL, the From
    /// address does not parse, or the public URL has no http(s) scheme.
    pub fn new(smtp_url: &str, from: &str, public_url: &str) -> Result<Self, String> {
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(smtp_url)
            .map_err(|e| format!("OXSUM_SMTP_URL is not an SMTP URL: {e}"))?
            .build();
        let from: Mailbox = from
            .parse()
            .map_err(|e| format!("OXSUM_MAIL_FROM is not a mailbox: {e}"))?;
        if !(public_url.starts_with("https://") || public_url.starts_with("http://")) {
            return Err(format!(
                "OXSUM_PUBLIC_URL must be http or https, got {public_url:?}"
            ));
        }
        Ok(Self {
            transport,
            from,
            public_url: public_url.trim_end_matches('/').to_owned(),
        })
    }

    /// Mails the email-verification link: `/app/verify-email?token=…`.
    ///
    /// The link lands on the React app's page directly — the legacy
    /// `/verify-email` path only redirects there now (issue #256), so the mail
    /// points where the page actually lives.
    pub async fn send_verification(&self, to: &str, token: &str) -> Result<(), String> {
        self.send(
            to,
            "Verify your oxsum email",
            &format!(
                "Someone registered this address at oxsum. Open the link within seven days \
                 to verify it:\n\n{}/app/verify-email?token={}\n\nIf this was not you, ignore \
                 this mail — the address stays unverified and nothing else happens.",
                self.public_url, token
            ),
        )
        .await
    }

    /// Mails the password-reset link: `/app/reset-password?token=…`.
    pub async fn send_password_reset(&self, to: &str, token: &str) -> Result<(), String> {
        self.send(
            to,
            "Reset your oxsum password",
            &format!(
                "Someone asked to reset the password of the oxsum account registered with \
                 this address. Open the link within one hour to choose a new one:\n\n\
                 {}/app/reset-password?token={}\n\nThe link works once. If this was not you, \
                 ignore this mail — the password is unchanged.",
                self.public_url, token
            ),
        )
        .await
    }

    /// Mails a customer their bill link: `/bill/{token}` (issue #315).
    ///
    /// `detail` is the line the caller already knows — the period and total for
    /// one report, or the sentence naming the customer scope. The link is a
    /// capability, so the mail is as sensitive as the bill: it goes to the
    /// address the operator named, once, and never appears in the database.
    pub async fn send_bill_link(
        &self,
        to: &str,
        token: &str,
        customer: &str,
        detail: &str,
    ) -> Result<(), String> {
        self.send(
            to,
            "Your oxsum bill is ready",
            &format!(
                "A bill for {customer} is ready.\n\n{detail}\n\nOpen it here:\n\
                 {}/bill/{token}\n\nThe page needs no account. Press \"Verify this bill\" to \
                 check it in your browser — it proves the rows are in the operator's ledger and \
                 that nothing was altered; it does not prove completeness, so compare the count \
                 and total with your own records.\n\nThe link expires in 30 days, or earlier if \
                 the operator revokes it.",
                self.public_url
            ),
        )
        .await
    }

    /// Sends one plain-text mail. Failures are a description string: the callers
    /// log them and answer as if nothing had happened — a mail that never
    /// arrives must not turn a registration or a forgot-answer into an error.
    async fn send(&self, to: &str, subject: &str, body: &str) -> Result<(), String> {
        let to: Mailbox = to
            .parse()
            .map_err(|e| format!("the recipient does not parse: {e}"))?;
        let message = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject(subject)
            .body(body.to_owned())
            .map_err(|e| format!("the message did not build: {e}"))?;
        self.transport
            .send(message)
            .await
            .map_err(|e| format!("the mail was not sent: {e}"))?;
        Ok(())
    }
}
