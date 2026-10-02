import nodemailer from 'nodemailer';

// Plain SMTP transport — this account already has a Purelymail mailbox
// (smtp.purelymail.com, standard STARTTLS/implicit-TLS SMTP), so there's no
// need for a dedicated transactional-email provider/API. Configure with:
//   SMTP_HOST=smtp.purelymail.com
//   SMTP_PORT=465              (or 587 — see secure: below)
//   SMTP_USER=you@yourdomain.com
//   SMTP_PASS=...              (an app password, if 2FA is on)
//   MAIL_FROM="Backup Buddies <you@yourdomain.com>"   (optional, defaults to SMTP_USER)
let transporter;

function getTransporter() {
  if (transporter !== undefined) return transporter;

  if (!process.env.SMTP_HOST || !process.env.SMTP_USER || !process.env.SMTP_PASS) {
    transporter = null;
    return transporter;
  }

  const port = Number(process.env.SMTP_PORT || 465);
  transporter = nodemailer.createTransport({
    host: process.env.SMTP_HOST,
    port,
    secure: port === 465, // 465 = implicit TLS; 587 (or anything else) = STARTTLS
    auth: { user: process.env.SMTP_USER, pass: process.env.SMTP_PASS },
  });
  return transporter;
}

// Returns true if the mail was handed off to the SMTP server, false if SMTP
// isn't configured on this deployment (logged instead of sent — lets
// signup/etc. keep working in dev without a mailbox set up). Throws if SMTP
// *is* configured but sending actually fails, so callers that need to know
// (e.g. "resend verification") can surface that rather than silently
// pretending it worked.
export async function sendMail({ to, subject, text, html }) {
  const t = getTransporter();
  if (!t) {
    console.warn(`SMTP not configured — email not sent: "${subject}" to ${to}`);
    return false;
  }
  await t.sendMail({ from: process.env.MAIL_FROM || process.env.SMTP_USER, to, subject, text, html });
  return true;
}
