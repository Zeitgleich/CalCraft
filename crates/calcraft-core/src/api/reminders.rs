//! E-mail reminders (`/api/v1/me/reminders`, `/api/v1/admin/reminders`).

use serde::{Deserialize, Serialize};

/// Which alarms send reminder e-mails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReminderModeName {
    Off,
    /// Only alarms with `ACTION:EMAIL`.
    Email,
    /// All alarms (also the display and sound alarms most clients create).
    All,
}

/// The user's reminder settings and status (`GET /api/v1/me/reminders`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderInfo {
    /// Effective mode (the user's choice or the server default).
    pub mode: ReminderModeName,
    /// The user chose `mode`; `false`: the server default applies.
    pub custom: bool,
    /// The server default.
    pub default_mode: ReminderModeName,
    /// Mails include the event's description.
    pub include_description: bool,
    /// The user's own address the mails go to; `None`: none is set (ask
    /// an administrator), nothing is sent.
    #[serde(default)]
    pub address: Option<String>,
    /// Objects with an upcoming alarm.
    pub scheduled: i64,
    /// Mails of the last seven days.
    pub status: MailStatus,
}

/// Counts of mails per state, without any server details.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailStatus {
    pub pending: i64,
    pub sent: i64,
    pub failed: i64,
    /// Not sent because of a limit or a full queue.
    pub dropped: i64,
    /// Code of the newest problem (`smtp_unavailable`, `smtp_rejected`,
    /// `smtp_tls`, `smtp_auth`, `limit`, `queue_full`, `expired`, …).
    #[serde(default)]
    pub last_error: Option<String>,
    /// When it happened (Unix seconds).
    #[serde(default)]
    pub last_error_at: Option<i64>,
}

/// `PATCH /api/v1/me/reminders`: fields that are present are changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateReminders {
    /// A mode, or `"default"` for the server default.
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub include_description: Option<bool>,
}

/// `POST /api/v1/me/reminders/test`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderTest {
    /// Queued for sending (`false`: dropped by a limit).
    pub queued: bool,
}

/// Queue overview for administrators (`GET /api/v1/admin/reminders`):
/// counts only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminReminders {
    pub default_mode: ReminderModeName,
    /// Objects with an upcoming alarm, all users.
    pub scheduled: i64,
    /// Mails of the last 24 hours (pending: all).
    pub status: MailStatus,
    pub cancelled: i64,
    /// Last successful delivery (Unix seconds) since the server started.
    #[serde(default)]
    pub last_success_at: Option<i64>,
    /// Code of the last SMTP problem since the server started.
    #[serde(default)]
    pub last_smtp_error: Option<String>,
    #[serde(default)]
    pub last_smtp_error_at: Option<i64>,
}
