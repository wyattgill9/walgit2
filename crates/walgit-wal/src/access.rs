//! D51: repository roles (`docs/ACCOUNTS.md`). One pure function decides a role; the server
//! calls it after its request sync, the publisher again on every CAS attempt.

use walgit_proto::v1::{AccessRole, RepoAccess, Visibility};

/// Ordered: each role includes every role below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    None,
    Read,
    Triage,
    Write,
    Maintain,
    Admin,
}

impl Role {
    pub fn from_proto(r: AccessRole) -> Self {
        match r {
            AccessRole::None => Role::None,
            AccessRole::Read => Role::Read,
            AccessRole::Triage => Role::Triage,
            AccessRole::Write => Role::Write,
            AccessRole::Maintain => Role::Maintain,
            AccessRole::Admin => Role::Admin,
        }
    }

    pub fn to_proto(self) -> AccessRole {
        match self {
            Role::None => AccessRole::None,
            Role::Read => AccessRole::Read,
            Role::Triage => AccessRole::Triage,
            Role::Write => AccessRole::Write,
            Role::Maintain => AccessRole::Maintain,
            Role::Admin => AccessRole::Admin,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::None => "none",
            Role::Read => "read",
            Role::Triage => "triage",
            Role::Write => "write",
            Role::Maintain => "maintain",
            Role::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "read" => Role::Read,
            "triage" => Role::Triage,
            "write" => Role::Write,
            "maintain" => Role::Maintain,
            "admin" => Role::Admin,
            _ => return None,
        })
    }
}

pub fn visibility_str(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "public",
        Visibility::Internal => "internal",
        Visibility::Private => "private",
    }
}

pub fn parse_visibility(s: &str) -> Option<Visibility> {
    Some(match s {
        "public" => Visibility::Public,
        "internal" => Visibility::Internal,
        "private" => Visibility::Private,
        _ => return None,
    })
}

/// Who asks, as the role function sees them.
#[derive(Debug, Clone, Copy)]
pub struct Asker<'a> {
    /// The account (`None` = anonymous, or a credential with no account).
    pub user_id: Option<&'a str>,
    /// Site admin, the namespace's user, or an owner of the namespace's org: Admin.
    pub owner_admin: bool,
}

/// The effective role on one repository: the highest of owner/site admin, collaborator role
/// and the visibility floor (public: Read for everyone, anonymous only when `anonymous_read`;
/// internal: Read for any account; private: nothing).
pub fn role(access: Option<&RepoAccess>, who: Asker<'_>, anonymous_read: bool) -> Role {
    if who.owner_admin {
        return Role::Admin;
    }
    let collaborator = who
        .user_id
        .and_then(|id| access?.collaborators.get(id).copied())
        .and_then(|r| AccessRole::try_from(r).ok())
        .map_or(Role::None, Role::from_proto);
    let visibility = access.map_or(Visibility::Public, RepoAccess::visibility);
    let floor = match visibility {
        Visibility::Public if who.user_id.is_some() || anonymous_read => Role::Read,
        Visibility::Internal if who.user_id.is_some() => Role::Read,
        _ => Role::None,
    };
    collaborator.max(floor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access(v: Visibility, collab: &[(&str, Role)]) -> RepoAccess {
        RepoAccess {
            visibility: v as i32,
            collaborators: collab
                .iter()
                .map(|(id, r)| ((*id).to_string(), r.to_proto() as i32))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn role_table() {
        let anon = Asker {
            user_id: None,
            owner_admin: false,
        };
        let dave = Asker {
            user_id: Some("dave"),
            owner_admin: false,
        };
        let bob = Asker {
            user_id: Some("bob"),
            owner_admin: false,
        };
        let owner = Asker {
            user_id: Some("alice"),
            owner_admin: true,
        };
        let collab = [("bob", Role::Write)];
        for (vis, anon_read, anon_ok, dave_ok) in [
            (Visibility::Public, true, Role::Read, Role::Read),
            (Visibility::Public, false, Role::None, Role::Read),
            (Visibility::Internal, true, Role::None, Role::Read),
            (Visibility::Private, true, Role::None, Role::None),
        ] {
            let a = access(vis, &collab);
            assert_eq!(role(Some(&a), anon, anon_read), anon_ok, "{vis:?} anon");
            assert_eq!(role(Some(&a), dave, anon_read), dave_ok, "{vis:?} dave");
            assert_eq!(role(Some(&a), bob, anon_read), Role::Write, "{vis:?} bob");
            assert_eq!(
                role(Some(&a), owner, anon_read),
                Role::Admin,
                "{vis:?} owner"
            );
        }
        // No document = public, no collaborators.
        assert_eq!(role(None, dave, false), Role::Read);
        assert_eq!(role(None, anon, false), Role::None);
        // A collaborator role below the floor never lowers it.
        let a = access(Visibility::Public, &[("dave", Role::None)]);
        assert_eq!(role(Some(&a), dave, false), Role::Read);
        // Every role orders above the ones below it.
        assert!(Role::None < Role::Read && Role::Read < Role::Triage);
        assert!(Role::Triage < Role::Write && Role::Write < Role::Maintain);
        assert!(Role::Maintain < Role::Admin);
        for r in [
            Role::Read,
            Role::Triage,
            Role::Write,
            Role::Maintain,
            Role::Admin,
        ] {
            assert_eq!(Role::parse(r.as_str()), Some(r));
            assert_eq!(Role::from_proto(r.to_proto()), r);
        }
    }
}
