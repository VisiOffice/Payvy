//! Company roles are additive. They authorize a command, never bypass its
//! workflow, person-only gate, separation of duties, or confirmation.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Admin,
    ApClerk,
    Approver,
    PaymentReleaser,
}
impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::ApClerk => "ap_clerk",
            Self::Approver => "approver",
            Self::PaymentReleaser => "payment_releaser",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Read,
    Prepare,
    Approve,
    Pay,
    Admin,
}
impl Permission {
    pub fn allows(self, roles: &[Role]) -> bool {
        if roles.contains(&Role::Admin) {
            return true;
        }
        match self {
            Self::Read => !roles.is_empty(),
            Self::Prepare => roles.contains(&Role::ApClerk),
            Self::Approve => roles.contains(&Role::Approver),
            Self::Pay => roles.contains(&Role::PaymentReleaser),
            Self::Admin => false,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Read => "company access",
            Self::Prepare => "AP clerk",
            Self::Approve => "Approver",
            Self::Pay => "Payment releaser",
            Self::Admin => "Admin",
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roles_combine_without_granting_administration() {
        let both = [Role::Approver, Role::PaymentReleaser];
        assert!(Permission::Approve.allows(&both));
        assert!(Permission::Pay.allows(&both));
        assert!(!Permission::Admin.allows(&both));
        assert!(!Permission::Prepare.allows(&both));
        assert!(!Permission::Read.allows(&[]));
        for p in [
            Permission::Read,
            Permission::Prepare,
            Permission::Approve,
            Permission::Pay,
            Permission::Admin,
        ] {
            assert!(p.allows(&[Role::Admin]));
        }
    }
}
