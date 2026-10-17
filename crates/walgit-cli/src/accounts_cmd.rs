//! D51: `walgit user create|ls` and `walgit org create|owners` — the account log, by an
//! operator with the bucket's credentials (`docs/ACCOUNTS.md`).

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use walgit_config::Config;
use walgit_proto::v1::{
    AccountEvent, OrgCreated, OrgOwnersChanged, PasswordSet, UserCreated, account_event::Body,
};
use walgit_store::open_store;
use walgit_wal::Registry;
use walgit_wal::accounts::{Accounts, AccountsState, Namespace};

use crate::{OrgAction, UserAction};

async fn accounts(cfg: &Arc<Config>) -> Result<Arc<walgit_wal::RepoHandle>> {
    let store = open_store(cfg).await?;
    std::fs::create_dir_all(&cfg.cache.dir).ok();
    let registry = Registry::new(store, cfg.clone());
    Ok(Accounts::new(registry).synced().await?)
}

fn ids(s: &AccountsState, logins: &[String]) -> Result<Vec<String>> {
    logins
        .iter()
        .map(|l| {
            s.user_by_login(l)
                .map(|u| u.user_id.clone())
                .with_context(|| format!("no user {l:?} (walgit user create {l})"))
        })
        .collect()
}

async fn publish(log: &walgit_wal::RepoHandle, ev: AccountEvent) -> Result<()> {
    match log.publish_accounts(vec![ev], "walgit-cli").await {
        Ok(_) => Ok(()),
        Err(walgit_wal::WalError::Invalid(why)) => bail!("{why}"),
        Err(e) => Err(e.into()),
    }
}

pub async fn run_user(action: UserAction, cfg: &Arc<Config>) -> Result<()> {
    let log = accounts(cfg).await?;
    match action {
        UserAction::Create {
            login,
            email,
            name,
            password,
        } => {
            let user_id = uuid::Uuid::new_v4().to_string();
            let mut events = vec![AccountEvent {
                body: Some(Body::UserCreated(UserCreated {
                    user_id: user_id.clone(),
                    login: login.clone(),
                    email: email.unwrap_or_default(),
                    name: name.unwrap_or_default(),
                    avatar_url: String::new(),
                })),
            }];
            if password {
                events.push(password_event(&user_id)?);
            }
            match log.publish_accounts(events, "walgit-cli").await {
                Ok(_) => {}
                Err(walgit_wal::WalError::Invalid(why)) => bail!("{why}"),
                Err(e) => return Err(e.into()),
            }
            println!("{login}\t{user_id}");
        }
        UserAction::Password { login } => {
            let user_id = log
                .accounts_read(|s| s.user_by_login(&login).map(|u| u.user_id.clone()))
                .with_context(|| format!("no user {login:?}"))?;
            publish(&log, password_event(&user_id)?).await?;
            println!("{login}: password set");
        }
        UserAction::List => {
            log.accounts_read(|s| {
                for u in s.users.values() {
                    println!("{}\t{}\t{}", u.login, u.user_id, u.email);
                }
            });
        }
    }
    Ok(())
}

pub async fn run_org(action: OrgAction, cfg: &Arc<Config>) -> Result<()> {
    let log = accounts(cfg).await?;
    match action {
        OrgAction::Create { login, owners } => {
            let owners = log.accounts_read(|s| ids(s, &owners))?;
            publish(
                &log,
                AccountEvent {
                    body: Some(Body::OrgCreated(OrgCreated {
                        org_id: uuid::Uuid::new_v4().to_string(),
                        login: login.clone(),
                        owners,
                    })),
                },
            )
            .await?;
            println!("org {login} created");
        }
        OrgAction::Owners { login, owners } => {
            let (org_id, owners) = log.accounts_read(|s| -> Result<_> {
                let Namespace::Org(org) = s.namespace(&login) else {
                    bail!("no org {login:?}");
                };
                Ok((org.org_id.clone(), ids(s, &owners)?))
            })?;
            publish(
                &log,
                AccountEvent {
                    body: Some(Body::OrgOwnersChanged(OrgOwnersChanged { org_id, owners })),
                },
            )
            .await?;
            println!("org {login}: owners replaced");
        }
    }
    Ok(())
}

/// A `PasswordSet` for `user_id` from `$WALGIT_PASSWORD` or the first line of stdin (never argv:
/// it would show in `ps` and shell history).
fn password_event(user_id: &str) -> Result<AccountEvent> {
    let password = match std::env::var("WALGIT_PASSWORD") {
        Ok(p) if !p.is_empty() => p,
        _ => {
            eprintln!("password (one line on stdin):");
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.trim_end_matches(['\r', '\n']).to_string()
        }
    };
    walgit_server::passwords::check_new(&password).map_err(anyhow::Error::msg)?;
    let phc = walgit_server::passwords::hash_blocking(&password).map_err(anyhow::Error::msg)?;
    Ok(AccountEvent {
        body: Some(Body::PasswordSet(PasswordSet {
            user_id: user_id.to_string(),
            phc,
        })),
    })
}

/// D51 security 1: before a repository is created in a namespace no account holds, reserve
/// that namespace on the account log, so a sign-up can never take it. A namespace that
/// belongs to an account needs nothing.
pub async fn reserve_namespace(registry: &Arc<Registry>, owner: &str) -> Result<()> {
    match Accounts::new(registry.clone())
        .reserve(owner, "walgit-cli")
        .await
    {
        Ok(()) | Err(walgit_wal::WalError::Invalid(_)) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
