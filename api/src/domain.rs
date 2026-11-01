//! Ticket status codes (internal, stored in `tickets.workflow_status`) and their UI labels.
//! Codes are the existing ones; labels follow the D01 freeze. `closed` exists only so a
//! historical archive row can be read: it is never selectable and is not mapped to resolved.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketStatus {
    New,
    Open,
    PendingCustomer,
    PendingEngineering,
    Resolved,
    Closed,
}

impl TicketStatus {
    pub const ALL: [TicketStatus; 6] = [
        TicketStatus::New,
        TicketStatus::Open,
        TicketStatus::PendingCustomer,
        TicketStatus::PendingEngineering,
        TicketStatus::Resolved,
        TicketStatus::Closed,
    ];

    pub fn code(self) -> &'static str {
        match self {
            TicketStatus::New => "new",
            TicketStatus::Open => "open",
            TicketStatus::PendingCustomer => "pending_customer",
            TicketStatus::PendingEngineering => "pending_engineering",
            TicketStatus::Resolved => "resolved",
            TicketStatus::Closed => "closed",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TicketStatus::New => "Baru",
            TicketStatus::Open => "Ditangani",
            TicketStatus::PendingCustomer => "Menunggu pelanggan",
            TicketStatus::PendingEngineering => "Menunggu pekerjaan eksternal",
            TicketStatus::Resolved => "Terselesaikan",
            TicketStatus::Closed => "Ditutup (arsip)",
        }
    }

    /// Close final is held: `closed` is read-only archive, never a target status.
    pub fn selectable(self) -> bool {
        self != TicketStatus::Closed
    }

    pub fn from_code(code: &str) -> Option<TicketStatus> {
        TicketStatus::ALL.into_iter().find(|s| s.code() == code)
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketStatusView {
    pub code: &'static str,
    pub label: &'static str,
    pub selectable: bool,
    pub archived: bool,
}

impl From<TicketStatus> for TicketStatusView {
    fn from(s: TicketStatus) -> Self {
        TicketStatusView {
            code: s.code(),
            label: s.label(),
            selectable: s.selectable(),
            archived: !s.selectable(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TicketStatus;

    #[test]
    fn codes_round_trip_and_only_closed_is_unselectable() {
        for s in TicketStatus::ALL {
            assert_eq!(TicketStatus::from_code(s.code()), Some(s));
            assert_eq!(s.selectable(), s != TicketStatus::Closed);
        }
        assert_eq!(TicketStatus::from_code("closed_final"), None);
    }

    #[test]
    fn pending_engineering_label_is_external_work() {
        assert_eq!(
            TicketStatus::PendingEngineering.label(),
            "Menunggu pekerjaan eksternal"
        );
    }
}
